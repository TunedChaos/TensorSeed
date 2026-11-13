// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! NAT port mapping for the torrent port, so peers outside a home router can
//! dial in. Off by default (`[serve] port_mapping = false`), because it
//! opens a port on the router.
//!
//! Two protocols are tried, both for TCP (BitTorrent) and UDP (uTP):
//!
//! - NAT-PMP (RFC 6886) on the default gateway, UDP port 5351. Routers that
//!   speak PCP (RFC 6887) answer version 0 requests too.
//! - UPnP IGD: SSDP discovery and `AddPortMapping` on a `WANIPConnection` or
//!   `WANPPPConnection` service (discovery and description parsing come from
//!   `librqbit-upnp`).
//!
//! Leases are short (one hour by default) and renewed at half time, and the
//! mappings are deleted on shutdown, so a crashed daemon does not leave a
//! port open for long.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, bail};
use librqbit_upnp::{
    SSDP_SEARCH_WAN_IPCONNECTION_ST, discover_once, discover_services, get_local_ip_relative_to,
};
use network_interface::{NetworkInterface, NetworkInterfaceConfig};
use tokio::net::UdpSocket;
use url::Url;

use crate::health::PortMapStatus;

/// NAT-PMP server port on the gateway.
pub const NATPMP_PORT: u16 = 5351;

/// Transport protocol of a mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// TCP (BitTorrent peer connections).
    Tcp,
    /// UDP (uTP, DHT).
    Udp,
}

impl Protocol {
    /// Lower case name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }

    fn natpmp_op(self) -> u8 {
        match self {
            Self::Udp => 1,
            Self::Tcp => 2,
        }
    }

    fn upnp(self) -> &'static str {
        match self {
            Self::Tcp => "TCP",
            Self::Udp => "UDP",
        }
    }
}

/// Default IPv4 gateway from a Linux `/proc/net/route` table.
pub fn parse_route_table(text: &str) -> Option<Ipv4Addr> {
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 4 || f[1] != "00000000" {
            continue;
        }
        let flags = u32::from_str_radix(f[3], 16).ok()?;
        if flags & 0x2 == 0 {
            continue;
        }
        let gw = u32::from_str_radix(f[2], 16).ok()?;
        let ip = Ipv4Addr::from(gw.to_le_bytes());
        if !ip.is_unspecified() {
            return Some(ip);
        }
    }
    None
}

/// The default IPv4 gateway (Linux only; `None` elsewhere).
pub fn default_gateway_v4() -> Option<Ipv4Addr> {
    std::fs::read_to_string("/proc/net/route")
        .ok()
        .and_then(|t| parse_route_table(&t))
}

fn natpmp_error(code: u16) -> &'static str {
    match code {
        1 => "unsupported version",
        2 => "not authorized or refused (port mapping disabled on the router?)",
        3 => "network failure",
        4 => "out of resources",
        5 => "unsupported opcode",
        _ => "unknown error",
    }
}

/// A NAT-PMP client for one gateway.
#[derive(Debug, Clone)]
pub struct NatPmp {
    gateway: SocketAddr,
    first_timeout: Duration,
    tries: u32,
}

impl NatPmp {
    /// Client for `gateway` (usually `<default gateway>:5351`).
    pub fn new(gateway: SocketAddr) -> Self {
        Self {
            gateway,
            first_timeout: Duration::from_millis(250),
            tries: 4,
        }
    }

    /// The gateway address.
    pub fn gateway(&self) -> SocketAddr {
        self.gateway
    }

    async fn request(&self, req: &[u8], op: u8, len: usize) -> anyhow::Result<Vec<u8>> {
        let bind: SocketAddr = if self.gateway.is_ipv4() {
            (Ipv4Addr::UNSPECIFIED, 0).into()
        } else {
            (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
        };
        let sock = UdpSocket::bind(bind).await?;
        sock.connect(self.gateway).await?;
        let mut wait = self.first_timeout;
        let mut buf = [0u8; 64];
        for _ in 0..self.tries {
            sock.send(req).await?;
            let deadline = tokio::time::Instant::now() + wait;
            while let Ok(r) = tokio::time::timeout_at(deadline, sock.recv(&mut buf)).await {
                let n = match r {
                    Ok(n) => n,
                    Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                        bail!(
                            "{} does not run NAT-PMP (port 5351 refused)",
                            self.gateway.ip()
                        )
                    }
                    Err(e) => return Err(e.into()),
                };
                if n >= len && buf[0] == 0 && buf[1] == 128 + op {
                    let code = u16::from_be_bytes([buf[2], buf[3]]);
                    if code != 0 {
                        bail!("NAT-PMP error {code}: {}", natpmp_error(code));
                    }
                    return Ok(buf[..n].to_vec());
                }
            }
            wait *= 2;
        }
        bail!("no NAT-PMP answer from {}", self.gateway)
    }

    /// The router's external IPv4 address.
    pub async fn external_ip(&self) -> anyhow::Result<Ipv4Addr> {
        let r = self.request(&[0, 0], 0, 12).await?;
        Ok(Ipv4Addr::new(r[8], r[9], r[10], r[11]))
    }

    /// Map `internal` (asking for `external`) for `lifetime_secs`. Returns
    /// the external port and lifetime the router granted.
    pub async fn map(
        &self,
        proto: Protocol,
        internal: u16,
        external: u16,
        lifetime_secs: u32,
    ) -> anyhow::Result<(u16, u32)> {
        let op = proto.natpmp_op();
        let mut req = vec![0, op, 0, 0];
        req.extend_from_slice(&internal.to_be_bytes());
        req.extend_from_slice(&external.to_be_bytes());
        req.extend_from_slice(&lifetime_secs.to_be_bytes());
        let r = self.request(&req, op, 16).await?;
        let got_internal = u16::from_be_bytes([r[8], r[9]]);
        if got_internal != internal {
            bail!("NAT-PMP answered for port {got_internal}, asked for {internal}");
        }
        let ext = u16::from_be_bytes([r[10], r[11]]);
        let life = u32::from_be_bytes([r[12], r[13], r[14], r[15]]);
        Ok((ext, life))
    }

    /// Delete the mapping of `internal`.
    pub async fn unmap(&self, proto: Protocol, internal: u16) -> anyhow::Result<()> {
        self.map(proto, internal, 0, 0).await.map(|_| ())
    }
}

/// UPnP IGD services that can map ports.
pub const UPNP_SERVICES: [&str; 3] = [
    "urn:schemas-upnp-org:service:WANIPConnection:1",
    "urn:schemas-upnp-org:service:WANIPConnection:2",
    "urn:schemas-upnp-org:service:WANPPPConnection:1",
];

const SSDP_SEARCH_IGD_ST: &str = "urn:schemas-upnp-org:device:InternetGatewayDevice:1";

/// A UPnP IGD port mapping service.
#[derive(Debug, Clone)]
pub struct Upnp {
    /// SOAP control URL.
    pub control_url: Url,
    /// Service type (one of [`UPNP_SERVICES`]).
    pub service_type: String,
    /// Our address on the router's network.
    pub local_ip: IpAddr,
    http: reqwest::Client,
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Text of the first `<tag>...</tag>` (namespace prefixes on the tag are
/// accepted).
pub fn xml_text(doc: &str, tag: &str) -> Option<String> {
    let mut rest = doc;
    while let Some(i) = rest.find('<') {
        rest = &rest[i + 1..];
        let end = rest.find('>')?;
        let name = &rest[..end];
        let local = name.rsplit(':').next().unwrap_or(name);
        if local == tag {
            let body = &rest[end + 1..];
            let close = body.find("</")?;
            return Some(body[..close].trim().to_owned());
        }
    }
    None
}

impl Upnp {
    /// A client for a known control URL.
    pub fn new(control_url: Url, service_type: &str, local_ip: IpAddr) -> Self {
        Self {
            control_url,
            service_type: service_type.to_owned(),
            local_ip,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
        }
    }

    /// Find an internet gateway on the local network with SSDP.
    pub async fn discover(timeout: Duration) -> anyhow::Result<Self> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (a, b) = tokio::join!(
            discover_once(&tx, SSDP_SEARCH_IGD_ST, timeout, None),
            discover_once(&tx, SSDP_SEARCH_WAN_IPCONNECTION_ST, timeout, None),
        );
        drop(tx);
        if let (Err(e), Err(_)) = (&a, &b) {
            bail!("SSDP discovery failed: {e:#}");
        }
        let mut errors = Vec::new();
        let mut seen = std::collections::HashSet::new();
        while let Some(resp) = rx.recv().await {
            if !seen.insert(resp.location.clone()) {
                continue;
            }
            let desc = match discover_services(resp.location.clone()).await {
                Ok(d) => d,
                Err(e) => {
                    errors.push(format!("{}: {e:#}", resp.location));
                    continue;
                }
            };
            let nics = NetworkInterface::show().context("listing network interfaces")?;
            let local_ip = get_local_ip_relative_to(resp.received_from, &nics)?;
            for d in &desc.devices {
                for (_, s) in d.iter_services(tracing::Span::none()) {
                    if UPNP_SERVICES.contains(&s.service_type.as_str())
                        && let Ok(url) = resp.location.join(&s.control_url)
                    {
                        return Ok(Self::new(url, &s.service_type, local_ip));
                    }
                }
            }
            errors.push(format!("{}: no WAN connection service", resp.location));
        }
        if errors.is_empty() {
            bail!(
                "no UPnP internet gateway answered (UPnP may be off on the router, or a \
                 host firewall such as ufw may drop the SSDP replies)"
            );
        }
        bail!("no usable UPnP gateway: {}", errors.join("; "))
    }

    async fn soap(&self, action: &str, args: &[(&str, String)]) -> anyhow::Result<String> {
        let mut inner = String::new();
        for (k, v) in args {
            inner.push_str(&format!("<{k}>{}</{k}>", xml_escape(v)));
        }
        let body = format!(
            "<?xml version=\"1.0\"?>\r\n<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
             s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body>\
             <u:{action} xmlns:u=\"{st}\">{inner}</u:{action}></s:Body></s:Envelope>\r\n",
            st = self.service_type
        );
        let resp = self
            .http
            .post(self.control_url.clone())
            .header("Content-Type", "text/xml; charset=\"utf-8\"")
            .header("SOAPAction", format!("\"{}#{action}\"", self.service_type))
            .body(body)
            .send()
            .await
            .with_context(|| format!("UPnP {action}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            let detail = xml_text(&text, "errorDescription")
                .or_else(|| xml_text(&text, "faultstring"))
                .unwrap_or_default();
            bail!("UPnP {action}: HTTP {status} {detail}");
        }
        Ok(text)
    }

    /// The router's external address.
    pub async fn external_ip(&self) -> anyhow::Result<String> {
        let t = self.soap("GetExternalIPAddress", &[]).await?;
        xml_text(&t, "NewExternalIPAddress").context("no NewExternalIPAddress in the answer")
    }

    /// Map `external` to our `internal` port for `lease_secs`.
    pub async fn add(
        &self,
        proto: Protocol,
        internal: u16,
        external: u16,
        lease_secs: u32,
        description: &str,
    ) -> anyhow::Result<()> {
        self.soap(
            "AddPortMapping",
            &[
                ("NewRemoteHost", String::new()),
                ("NewExternalPort", external.to_string()),
                ("NewProtocol", proto.upnp().into()),
                ("NewInternalPort", internal.to_string()),
                ("NewInternalClient", self.local_ip.to_string()),
                ("NewEnabled", "1".into()),
                ("NewPortMappingDescription", description.into()),
                ("NewLeaseDuration", lease_secs.to_string()),
            ],
        )
        .await
        .map(|_| ())
    }

    /// Delete the mapping of `external`.
    pub async fn delete(&self, proto: Protocol, external: u16) -> anyhow::Result<()> {
        self.soap(
            "DeletePortMapping",
            &[
                ("NewRemoteHost", String::new()),
                ("NewExternalPort", external.to_string()),
                ("NewProtocol", proto.upnp().into()),
            ],
        )
        .await
        .map(|_| ())
    }
}

/// What to map.
#[derive(Debug, Clone)]
pub struct PortMapOptions {
    /// Local port (the torrent listen port).
    pub port: u16,
    /// Protocols to map.
    pub protocols: Vec<Protocol>,
    /// Lease length; renewed at half time.
    pub lease_secs: u32,
    /// Mapping description shown in the router UI.
    pub description: String,
    /// Try NAT-PMP.
    pub natpmp: bool,
    /// NAT-PMP gateway override (default: the default gateway, port 5351).
    pub natpmp_gateway: Option<SocketAddr>,
    /// Try UPnP IGD.
    pub upnp: bool,
    /// SSDP wait.
    pub discover_timeout: Duration,
    /// Wait before retrying after every method failed.
    pub retry_after: Duration,
}

impl PortMapOptions {
    /// Defaults for `port`: TCP and UDP, one hour leases, both methods.
    pub fn new(port: u16) -> Self {
        Self {
            port,
            protocols: vec![Protocol::Tcp, Protocol::Udp],
            lease_secs: 3600,
            description: "tensorseed".into(),
            natpmp: true,
            natpmp_gateway: None,
            upnp: true,
            discover_timeout: Duration::from_secs(3),
            retry_after: Duration::from_secs(300),
        }
    }
}

enum Active {
    NatPmp(NatPmp, Vec<(Protocol, u16)>),
    Upnp(Upnp, Vec<(Protocol, u16)>),
}

impl Active {
    async fn remove(&self, internal: u16) {
        match self {
            Self::NatPmp(c, maps) => {
                for (p, _) in maps {
                    if let Err(e) = c.unmap(*p, internal).await {
                        tracing::warn!("removing NAT-PMP {} mapping: {e:#}", p.as_str());
                    }
                }
            }
            Self::Upnp(c, maps) => {
                for (p, ext) in maps {
                    if let Err(e) = c.delete(*p, *ext).await {
                        tracing::warn!("removing UPnP {} mapping: {e:#}", p.as_str());
                    }
                }
            }
        }
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

async fn try_natpmp(o: &PortMapOptions) -> anyhow::Result<(Active, PortMapStatus, u32)> {
    let gw = match o.natpmp_gateway {
        Some(g) => g,
        None => SocketAddr::new(
            IpAddr::V4(default_gateway_v4().context("no default IPv4 gateway")?),
            NATPMP_PORT,
        ),
    };
    let c = NatPmp::new(gw);
    let mut maps = Vec::new();
    let mut lease = o.lease_secs;
    for p in &o.protocols {
        let (ext, life) = c.map(*p, o.port, o.port, o.lease_secs).await?;
        maps.push((*p, ext));
        lease = lease.min(life.max(60));
    }
    let ext_ip = c.external_ip().await.ok().map(|i| i.to_string());
    let st = PortMapStatus {
        enabled: true,
        method: Some("nat-pmp".into()),
        external_ip: ext_ip,
        external_port: maps.first().map(|m| m.1),
        protocols: maps.iter().map(|m| m.0.as_str().to_owned()).collect(),
        renewed_unix: Some(unix_now()),
        error: None,
    };
    Ok((Active::NatPmp(c, maps), st, lease))
}

async fn try_upnp(
    o: &PortMapOptions,
    cache: &mut Option<Upnp>,
) -> anyhow::Result<(Active, PortMapStatus, u32)> {
    let c = match cache.take() {
        Some(c) => c,
        None => Upnp::discover(o.discover_timeout).await?,
    };
    let mut maps = Vec::new();
    for p in &o.protocols {
        c.add(*p, o.port, o.port, o.lease_secs, &o.description)
            .await?;
        maps.push((*p, o.port));
    }
    let ext_ip = c.external_ip().await.ok();
    *cache = Some(c.clone());
    let st = PortMapStatus {
        enabled: true,
        method: Some("upnp".into()),
        external_ip: ext_ip,
        external_port: Some(o.port),
        protocols: maps.iter().map(|m| m.0.as_str().to_owned()).collect(),
        renewed_unix: Some(unix_now()),
        error: None,
    };
    Ok((Active::Upnp(c, maps), st, o.lease_secs))
}

/// A background task keeping the mappings alive.
pub struct PortMapper {
    stop: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
    status: Arc<RwLock<PortMapStatus>>,
}

impl PortMapper {
    /// Start mapping. Progress and errors go to [`PortMapper::status`].
    pub fn spawn(o: PortMapOptions) -> Self {
        let status = Arc::new(RwLock::new(PortMapStatus {
            enabled: true,
            ..Default::default()
        }));
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        let st = status.clone();
        let task = tokio::spawn(async move {
            let mut upnp_cache: Option<Upnp> = None;
            let set = |s: PortMapStatus| *st.write().unwrap_or_else(|e| e.into_inner()) = s;
            loop {
                let mut errors = Vec::new();
                let mut got = None;
                if o.natpmp {
                    match try_natpmp(&o).await {
                        Ok(a) => got = Some(a),
                        Err(e) => errors.push(format!("nat-pmp: {e:#}")),
                    }
                }
                if got.is_none() && o.upnp {
                    match try_upnp(&o, &mut upnp_cache).await {
                        Ok(a) => got = Some(a),
                        Err(e) => errors.push(format!("upnp: {e:#}")),
                    }
                }
                let wait = match got {
                    Some((active, s, lease)) => {
                        if s.external_port != Some(o.port) {
                            tracing::warn!(
                                "router mapped external port {:?} for local port {}; peers learn the local port, so set torrent.listen_port to a port the router keeps",
                                s.external_port,
                                o.port
                            );
                        }
                        tracing::info!(
                            method = s.method.as_deref().unwrap_or(""),
                            external_ip = s.external_ip.as_deref().unwrap_or("unknown"),
                            port = o.port,
                            "port mapping active"
                        );
                        set(s);
                        let renew = Duration::from_secs(u64::from(lease / 2).max(30));
                        tokio::select! {
                            _ = tokio::time::sleep(renew) => {}
                            _ = stopped.changed() => {
                                active.remove(o.port).await;
                                return;
                            }
                        }
                        continue;
                    }
                    None => {
                        let msg = if errors.is_empty() {
                            "no port mapping method enabled".to_string()
                        } else {
                            errors.join("; ")
                        };
                        tracing::warn!("port mapping failed: {msg}");
                        set(PortMapStatus {
                            enabled: true,
                            error: Some(msg),
                            ..Default::default()
                        });
                        o.retry_after
                    }
                };
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = stopped.changed() => return,
                }
            }
        });
        Self { stop, task, status }
    }

    /// Current state.
    pub fn status(&self) -> PortMapStatus {
        self.status
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Stop renewing and delete the mappings (waits up to 10 seconds).
    pub async fn shutdown(self) {
        let _ = self.stop.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(10), self.task).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn route_table() {
        let t = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                 eth0\t0000A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n\
                 eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n";
        assert_eq!(parse_route_table(t), Some(Ipv4Addr::new(192, 168, 1, 1)));
        assert_eq!(parse_route_table("Iface\n"), None);
    }

    #[test]
    fn xml() {
        let t = "<s:Envelope><s:Body><u:GetExternalIPAddressResponse>\
                 <NewExternalIPAddress> 203.0.113.7 </NewExternalIPAddress>\
                 </u:GetExternalIPAddressResponse></s:Body></s:Envelope>";
        assert_eq!(
            xml_text(t, "NewExternalIPAddress").as_deref(),
            Some("203.0.113.7")
        );
        assert_eq!(xml_text(t, "Missing"), None);
        assert_eq!(xml_escape("a<&>"), "a&lt;&amp;&gt;");
    }

    /// A fake NAT-PMP gateway: grants every mapping, records requests.
    async fn fake_gateway(log: Arc<Mutex<Vec<Vec<u8>>>>) -> SocketAddr {
        let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = s.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 64];
            loop {
                let Ok((n, from)) = s.recv_from(&mut buf).await else {
                    return;
                };
                let req = buf[..n].to_vec();
                log.lock().unwrap().push(req.clone());
                let mut r = vec![0, 128 + req[1], 0, 0, 0, 0, 0, 9];
                if req[1] == 0 {
                    r.extend_from_slice(&[203, 0, 113, 7]);
                } else if req[4..6] == [0x1a, 0xe1] {
                    // Port 6881 TCP refused, to test errors.
                    r[3] = 2;
                    r.extend_from_slice(&[0; 8]);
                } else {
                    r.extend_from_slice(&req[4..6]);
                    r.extend_from_slice(&req[6..8]);
                    r.extend_from_slice(&req[8..12]);
                }
                let _ = s.send_to(&r, from).await;
            }
        });
        addr
    }

    #[tokio::test]
    async fn natpmp_maps_and_reports_errors() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let gw = fake_gateway(log.clone()).await;
        let c = NatPmp::new(gw);
        assert_eq!(
            c.external_ip().await.unwrap(),
            Ipv4Addr::new(203, 0, 113, 7)
        );
        assert_eq!(
            c.map(Protocol::Udp, 7000, 7000, 3600).await.unwrap(),
            (7000, 3600)
        );
        let e = c.map(Protocol::Tcp, 6881, 6881, 3600).await.unwrap_err();
        assert!(e.to_string().contains("not authorized"), "{e}");
        c.unmap(Protocol::Udp, 7000).await.unwrap();
        let l = log.lock().unwrap();
        assert_eq!(
            l[1],
            vec![0, 1, 0, 0, 0x1b, 0x58, 0x1b, 0x58, 0, 0, 0x0e, 0x10]
        );
        assert_eq!(&l[3][8..12], &[0, 0, 0, 0], "unmap uses lifetime 0");
    }

    #[tokio::test]
    async fn natpmp_times_out_without_gateway() {
        let s = UdpSocket::bind("127.0.0.1:0").await.unwrap(); // never answers
        let mut c = NatPmp::new(s.local_addr().unwrap());
        c.first_timeout = Duration::from_millis(20);
        c.tries = 2;
        let e = c.external_ip().await.unwrap_err();
        assert!(e.to_string().contains("no NAT-PMP answer"), "{e}");
    }

    #[tokio::test]
    async fn natpmp_refused_means_no_natpmp() {
        // A closed UDP port answers with ICMP port unreachable on loopback,
        // like a router without NAT-PMP.
        let addr = UdpSocket::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap();
        let mut c = NatPmp::new(addr);
        c.first_timeout = Duration::from_millis(200);
        c.tries = 2;
        let e = c.external_ip().await.unwrap_err();
        assert!(e.to_string().contains("does not run NAT-PMP"), "{e}");
    }

    #[tokio::test]
    async fn mapper_maps_renews_and_cleans_up() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let gw = fake_gateway(log.clone()).await;
        let mut o = PortMapOptions::new(7001);
        o.natpmp_gateway = Some(gw);
        o.upnp = false;
        let m = PortMapper::spawn(o);
        for _ in 0..100 {
            if m.status().method.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let s = m.status();
        assert_eq!(s.method.as_deref(), Some("nat-pmp"));
        assert_eq!(s.external_ip.as_deref(), Some("203.0.113.7"));
        assert_eq!(s.external_port, Some(7001));
        assert_eq!(s.protocols, vec!["tcp", "udp"]);
        m.shutdown().await;
        let l = log.lock().unwrap();
        let unmaps = l
            .iter()
            .filter(|r| r.len() == 12 && r[8..12] == [0, 0, 0, 0])
            .count();
        assert_eq!(unmaps, 2, "TCP and UDP mappings removed: {l:?}");
    }

    #[tokio::test]
    async fn mapper_reports_failure() {
        let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut o = PortMapOptions::new(6881);
        o.natpmp_gateway = Some(s.local_addr().unwrap());
        o.upnp = false;
        let m = PortMapper::spawn(o);
        for _ in 0..200 {
            if m.status().error.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(m.status().error.unwrap().contains("nat-pmp"));
        m.shutdown().await;
    }

    #[tokio::test]
    async fn upnp_soap_calls() {
        use axum::http::{HeaderMap, StatusCode};
        let seen = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
        let s2 = seen.clone();
        let app = axum::Router::new().route(
            "/ctl",
            axum::routing::post(move |h: HeaderMap, body: String| {
                let seen = s2.clone();
                async move {
                    let action = h["soapaction"].to_str().unwrap().to_owned();
                    seen.lock().unwrap().push((action.clone(), body.clone()));
                    if action.contains("GetExternalIPAddress") {
                        (
                            StatusCode::OK,
                            "<s:Envelope><s:Body><u:R><NewExternalIPAddress>198.51.100.2</NewExternalIPAddress></u:R></s:Body></s:Envelope>".to_string(),
                        )
                    } else if body.contains("<NewExternalPort>1</NewExternalPort>") {
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "<s:Envelope><s:Body><s:Fault><detail><UPnPError><errorCode>718</errorCode><errorDescription>ConflictInMappingEntry</errorDescription></UPnPError></detail></s:Fault></s:Body></s:Envelope>".to_string(),
                        )
                    } else {
                        (StatusCode::OK, "<ok/>".to_string())
                    }
                }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await });
        let c = Upnp::new(
            Url::parse(&format!("http://{addr}/ctl")).unwrap(),
            UPNP_SERVICES[0],
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20)),
        );
        assert_eq!(c.external_ip().await.unwrap(), "198.51.100.2");
        c.add(Protocol::Udp, 6881, 6881, 3600, "tensorseed")
            .await
            .unwrap();
        let e = c.add(Protocol::Tcp, 1, 1, 3600, "t").await.unwrap_err();
        assert!(e.to_string().contains("ConflictInMappingEntry"), "{e}");
        c.delete(Protocol::Udp, 6881).await.unwrap();
        let s = seen.lock().unwrap();
        assert_eq!(
            s[1].0,
            "\"urn:schemas-upnp-org:service:WANIPConnection:1#AddPortMapping\""
        );
        assert!(s[1].1.contains("<NewProtocol>UDP</NewProtocol>"));
        assert!(
            s[1].1
                .contains("<NewInternalClient>192.168.1.20</NewInternalClient>")
        );
        assert!(s[3].0.ends_with("#DeletePortMapping\""));
    }
}
