// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Swarm policy shared by `pull`, `seed`, `serve`, and Tensorseed Desktop:
//! scheduled ("alternative") speed limits, seeding stop rules (share ratio
//! and seeding time), and the helpers that apply them to a running engine.
//!
//! Everything here is plain data plus pure functions, so the rules can be
//! tested without a swarm, and a settings page can validate input with the
//! same code the daemon uses.
//!
//! # Speed schedule
//!
//! `[torrent.alt_speed]` holds a second pair of caps and a list of windows.
//! A window names days of the week and a start and end time in local wall
//! clock time (`HH:MM`, 24 hour). The window is active from `start` up to,
//! but not including, `end`. An `end` at or before `start` runs past
//! midnight into the next day, and belongs to the day it starts on.
//!
//! Wall clock rules make daylight saving changes behave the way people
//! expect: on the night clocks go forward, a window starting inside the
//! skipped hour starts when the clock jumps (for example `02:30` becomes
//! `03:00`); on the night clocks go back, the repeated hour is inside a
//! window once per pass, so a `01:00` to `01:30` window is active twice.

use std::num::NonZeroU32;
use std::time::Duration;

use chrono::{DateTime, Datelike, NaiveTime, TimeZone, Timelike, Weekday};
use serde::{Deserialize, Serialize};

/// Upload and download caps in bytes per second (`None`: unlimited).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RateLimits {
    /// Upload cap.
    pub upload_bps: Option<u32>,
    /// Download cap.
    pub download_bps: Option<u32>,
}

impl RateLimits {
    /// From KiB/s settings where 0 means unlimited.
    pub fn from_kbps(upload_kbps: u64, download_kbps: u64) -> Self {
        Self {
            upload_bps: kbps_to_bps(upload_kbps),
            download_bps: kbps_to_bps(download_kbps),
        }
    }
}

/// KiB/s to bytes per second; 0 is unlimited (`None`). Saturates at
/// `u32::MAX`.
pub fn kbps_to_bps(k: u64) -> Option<u32> {
    match k {
        0 => None,
        k => Some(u32::try_from(k.saturating_mul(1024)).unwrap_or(u32::MAX)),
    }
    .and_then(|b| NonZeroU32::new(b).map(NonZeroU32::get))
}

/// One schedule window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeedWindow {
    /// Days the window starts on: `mon` .. `sun`, or the groups
    /// `weekdays`, `weekends`, `all`. Empty means every day.
    #[serde(default)]
    pub days: Vec<String>,
    /// Local start time, `HH:MM`.
    pub start: String,
    /// Local end time, `HH:MM` (exclusive), or `24:00` for midnight at the
    /// end of the day. Before `start`: the window ends the next day.
    pub end: String,
}

/// `[torrent.alt_speed]`: caps used while a window is active.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AltSpeed {
    /// Turn the schedule on. Off by default.
    pub enabled: bool,
    /// Upload cap in KiB/s while a window is active; 0 means unlimited.
    pub upload_limit_kbps: u64,
    /// Download cap in KiB/s while a window is active; 0 means unlimited.
    pub download_limit_kbps: u64,
    /// When the alternative caps apply.
    pub window: Vec<SpeedWindow>,
}

/// A validated window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// Bit `n` set: the window starts on `Weekday::num_days_from_monday() == n`.
    pub days: u8,
    /// Start, minutes after midnight.
    pub start: u16,
    /// End, minutes after midnight (exclusive, up to 1440).
    pub end: u16,
}

fn parse_hhmm(s: &str, what: &str) -> anyhow::Result<u16> {
    let t = NaiveTime::parse_from_str(s.trim(), "%H:%M")
        .map_err(|_| anyhow::anyhow!("{what}: {s:?} is not a 24 hour HH:MM time"))?;
    Ok(u16::try_from(t.hour() * 60 + t.minute()).unwrap_or(0))
}

fn parse_days(days: &[String]) -> anyhow::Result<u8> {
    if days.is_empty() {
        return Ok(0x7f);
    }
    let mut mask = 0u8;
    for d in days {
        mask |= match d.trim().to_ascii_lowercase().as_str() {
            "mon" | "monday" => 1,
            "tue" | "tuesday" => 1 << 1,
            "wed" | "wednesday" => 1 << 2,
            "thu" | "thursday" => 1 << 3,
            "fri" | "friday" => 1 << 4,
            "sat" | "saturday" => 1 << 5,
            "sun" | "sunday" => 1 << 6,
            "weekdays" => 0x1f,
            "weekends" => 0x60,
            "all" | "daily" => 0x7f,
            other => anyhow::bail!(
                "torrent.alt_speed.window days: unknown day {other:?} (use mon..sun, weekdays, weekends, or all)"
            ),
        };
    }
    Ok(mask)
}

impl SpeedWindow {
    /// Validate and convert.
    ///
    /// # Errors
    ///
    /// Fails on an unknown day, a malformed time, or `start == end`.
    pub fn parse(&self) -> anyhow::Result<Window> {
        let start = parse_hhmm(&self.start, "torrent.alt_speed.window start")?;
        let end = if self.end.trim() == "24:00" {
            24 * 60
        } else {
            parse_hhmm(&self.end, "torrent.alt_speed.window end")?
        };
        if start == end {
            anyhow::bail!(
                "torrent.alt_speed.window: start and end are both {:?} (empty window); \
                 use start = \"00:00\" and end = \"24:00\" for a whole day",
                self.start
            );
        }
        Ok(Window {
            days: parse_days(&self.days)?,
            start,
            end,
        })
    }
}

fn day_bit(d: Weekday) -> u8 {
    1 << d.num_days_from_monday()
}

impl Window {
    /// Is local wall clock time `minute` (after midnight) on `day` inside
    /// this window?
    pub fn contains(&self, day: Weekday, minute: u16) -> bool {
        if self.start < self.end {
            self.days & day_bit(day) != 0 && minute >= self.start && minute < self.end
        } else {
            // Overnight: the evening part belongs to `day`, the morning part
            // to the day before.
            (self.days & day_bit(day) != 0 && minute >= self.start)
                || (self.days & day_bit(day.pred()) != 0 && minute < self.end)
        }
    }
}

/// Normal caps plus an optional schedule: the caps in force at any moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeedPlan {
    /// Caps outside any window.
    pub normal: RateLimits,
    /// Alternative caps and their windows (`None`: no schedule).
    pub alt: Option<(RateLimits, Vec<Window>)>,
}

impl SpeedPlan {
    /// Build from normal caps and `[torrent.alt_speed]`.
    ///
    /// # Errors
    ///
    /// Fails when a window is invalid.
    pub fn new(normal: RateLimits, alt: &AltSpeed) -> anyhow::Result<Self> {
        let windows = alt
            .window
            .iter()
            .map(SpeedWindow::parse)
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Self {
            normal,
            alt: (alt.enabled && !windows.is_empty()).then(|| {
                (
                    RateLimits::from_kbps(alt.upload_limit_kbps, alt.download_limit_kbps),
                    windows,
                )
            }),
        })
    }

    /// True when an alternative window is active at `t` (its local wall
    /// clock time in `t`'s zone).
    pub fn alt_active_at<Tz: TimeZone>(&self, t: &DateTime<Tz>) -> bool {
        let Some((_, windows)) = &self.alt else {
            return false;
        };
        let minute = u16::try_from(t.hour() * 60 + t.minute()).unwrap_or(0);
        let day = t.weekday();
        windows.iter().any(|w| w.contains(day, minute))
    }

    /// Caps in force at `t`.
    pub fn limits_at<Tz: TimeZone>(&self, t: &DateTime<Tz>) -> RateLimits {
        match &self.alt {
            Some((alt, _)) if self.alt_active_at(t) => *alt,
            _ => self.normal,
        }
    }

    /// Caps in force now, in the system time zone.
    pub fn limits_now(&self) -> RateLimits {
        self.limits_at(&chrono::Local::now())
    }

    /// True when the plan has a schedule (caps can change while running).
    pub fn is_scheduled(&self) -> bool {
        self.alt.is_some()
    }
}

/// What to do with a torrent that reached a seeding limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SeedLimitAction {
    /// Keep the torrent in the session, paused (default).
    #[default]
    Pause,
    /// Take the torrent out of the session. Files stay on disk.
    Remove,
}

/// Seeding stop rules.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SeedLimits {
    /// Stop once uploaded bytes reach this multiple of the revision size.
    pub ratio: Option<f64>,
    /// Stop after seeding this long.
    pub time: Option<Duration>,
    /// What stopping means.
    pub action: SeedLimitAction,
}

/// Which limit was reached.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LimitHit {
    /// Share ratio reached.
    Ratio(f64),
    /// Seeding time reached.
    Time(Duration),
}

impl std::fmt::Display for LimitHit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ratio(r) => write!(f, "share ratio {r:.2} reached"),
            Self::Time(d) => write!(f, "seeding time {}s reached", d.as_secs()),
        }
    }
}

impl SeedLimits {
    /// Build from config values: ratio 0 and an empty time mean no limit.
    ///
    /// # Errors
    ///
    /// Fails on a negative or non finite ratio or a malformed duration.
    pub fn new(ratio: f64, time: &str, action: SeedLimitAction) -> anyhow::Result<Self> {
        if !ratio.is_finite() || ratio < 0.0 {
            anyhow::bail!("seed_ratio_limit must be 0 (no limit) or a positive number");
        }
        let time = if time.trim().is_empty() {
            None
        } else {
            crate::torrents::parse_duration(time)
                .map_err(|e| anyhow::anyhow!("seed_time_limit {time:?}: {e:#}"))?
        };
        Ok(Self {
            ratio: (ratio > 0.0).then_some(ratio),
            time,
            action,
        })
    }

    /// True when neither rule is set.
    pub fn is_unlimited(&self) -> bool {
        self.ratio.is_none() && self.time.is_none()
    }

    /// The first limit reached, if any. `size` is the revision size; a zero
    /// size never trips the ratio rule.
    pub fn check(&self, uploaded: u64, size: u64, seeded: Duration) -> Option<LimitHit> {
        if let Some(r) = self.ratio
            && size > 0
        {
            let have = uploaded as f64 / size as f64;
            if have >= r {
                return Some(LimitHit::Ratio(have));
            }
        }
        if let Some(t) = self.time
            && seeded >= t
        {
            return Some(LimitHit::Time(seeded));
        }
        None
    }

    /// [`Self::check`] on saved totals.
    pub fn check_totals(&self, t: &SeedTotals, size: u64) -> Option<LimitHit> {
        self.check(t.uploaded_bytes, size, Duration::from_secs(t.seeding_secs))
    }
}

/// Per torrent seeding totals across engine sessions: what earlier runs
/// recorded plus what the current handle reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SeedTotals {
    /// Bytes uploaded in earlier runs.
    pub uploaded_bytes: u64,
    /// Seconds seeded in earlier runs.
    pub seeding_secs: u64,
}

/// Tracks one torrent's seeding time and upload against [`SeedLimits`].
#[derive(Debug, Clone)]
pub struct SeedTracker {
    prior: SeedTotals,
    secs_this_run: f64,
    last_tick: Option<std::time::Instant>,
}

impl SeedTracker {
    /// Start from totals saved by an earlier run.
    pub fn new(prior: SeedTotals) -> Self {
        Self {
            prior,
            secs_this_run: 0.0,
            last_tick: None,
        }
    }

    /// Record a tick at `now`. Time only counts while `active` (live and
    /// not paused). `uploaded_this_run` is the handle's upload counter.
    pub fn tick(
        &mut self,
        now: std::time::Instant,
        active: bool,
        uploaded_this_run: u64,
    ) -> SeedTotals {
        if let Some(last) = self.last_tick
            && active
        {
            self.secs_this_run += now.saturating_duration_since(last).as_secs_f64();
        }
        self.last_tick = Some(now);
        self.totals(uploaded_this_run)
    }

    /// Totals so far.
    pub fn totals(&self, uploaded_this_run: u64) -> SeedTotals {
        SeedTotals {
            uploaded_bytes: self.prior.uploaded_bytes.saturating_add(uploaded_this_run),
            seeding_secs: self
                .prior
                .seeding_secs
                .saturating_add(self.secs_this_run as u64),
        }
    }
}

/// Keeps a speed schedule running; dropping it stops the schedule (the
/// caps in force stay as they are).
#[derive(Debug)]
pub struct SpeedSchedule(Option<tokio::task::JoinHandle<()>>);

impl SpeedSchedule {
    /// True when a schedule task is running (the plan has alt windows).
    pub fn is_scheduled(&self) -> bool {
        self.0.is_some()
    }
}

impl Drop for SpeedSchedule {
    fn drop(&mut self) {
        if let Some(h) = self.0.take() {
            h.abort();
        }
    }
}

/// Keep the engine's session caps in line with `plan`: apply them now, and
/// if the plan has a schedule, re-check every 30 seconds until the returned
/// guard is dropped. Needs a Tokio runtime when the plan is scheduled.
pub fn apply_plan(rates: tensorseed_torrent::engine::RateHandle, plan: SpeedPlan) -> SpeedSchedule {
    let first = plan.limits_now();
    rates.set(first.upload_bps, first.download_bps);
    if !plan.is_scheduled() {
        return SpeedSchedule(None);
    }
    SpeedSchedule(Some(tokio::spawn(async move {
        let mut current = first;
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        loop {
            tick.tick().await;
            let next = plan.limits_now();
            if next != current {
                tracing::info!(
                    upload_bps = next.upload_bps.unwrap_or(0),
                    download_bps = next.download_bps.unwrap_or(0),
                    alt = next != plan.normal,
                    "speed schedule: caps changed"
                );
                rates.set(next.upload_bps, next.download_bps);
                current = next;
            }
        }
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, NaiveDate};

    fn win(days: &[&str], start: &str, end: &str) -> SpeedWindow {
        SpeedWindow {
            days: days.iter().map(|d| (*d).to_string()).collect(),
            start: start.into(),
            end: end.into(),
        }
    }

    fn plan(windows: Vec<SpeedWindow>) -> SpeedPlan {
        SpeedPlan::new(
            RateLimits::from_kbps(1000, 0),
            &AltSpeed {
                enabled: true,
                upload_limit_kbps: 100,
                download_limit_kbps: 200,
                window: windows,
            },
        )
        .unwrap()
    }

    /// Local wall clock `y-m-d h:m` at a fixed UTC offset in hours.
    fn at(offset_h: i32, y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<FixedOffset> {
        let tz = FixedOffset::east_opt(offset_h * 3600).unwrap();
        tz.from_local_datetime(
            &NaiveDate::from_ymd_opt(y, m, d)
                .unwrap()
                .and_hms_opt(h, min, 0)
                .unwrap(),
        )
        .unwrap()
    }

    /// The same instant as `utc` seen in a zone at `offset_h`.
    fn utc_in(offset_h: i32, y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<FixedOffset> {
        let utc = chrono::Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap();
        utc.with_timezone(&FixedOffset::east_opt(offset_h * 3600).unwrap())
    }

    #[test]
    fn kbps_conversion() {
        assert_eq!(kbps_to_bps(0), None);
        assert_eq!(kbps_to_bps(1), Some(1024));
        assert_eq!(kbps_to_bps(u64::MAX), Some(u32::MAX));
    }

    #[test]
    fn window_parsing_and_validation() {
        assert!(win(&["mon"], "9:00", "17:00").parse().is_ok());
        assert!(win(&["mon"], "09:00", "25:00").parse().is_err());
        assert!(win(&["mon"], "09:00", "09:00").parse().is_err());
        assert!(win(&["funday"], "09:00", "10:00").parse().is_err());
        assert!(
            win(&["mon"], "24:00", "10:00").parse().is_err(),
            "24:00 ends only"
        );
        let day = win(&["sat"], "00:00", "24:00").parse().unwrap();
        assert!(day.contains(Weekday::Sat, 0) && day.contains(Weekday::Sat, 1439));
        assert!(!day.contains(Weekday::Sun, 0));
        assert_eq!(win(&[], "09:00", "10:00").parse().unwrap().days, 0x7f);
        assert_eq!(
            win(&["weekdays"], "09:00", "10:00").parse().unwrap().days,
            0x1f
        );
        assert_eq!(
            win(&["weekends", "mon"], "09:00", "10:00")
                .parse()
                .unwrap()
                .days,
            0x61
        );
    }

    #[test]
    fn daytime_window_by_weekday() {
        let p = plan(vec![win(&["weekdays"], "09:00", "17:00")]);
        // 2026-11-13 is a Friday, 2026-11-14 a Saturday.
        assert!(p.alt_active_at(&at(-5, 2026, 11, 13, 9, 0)));
        assert!(p.alt_active_at(&at(-5, 2026, 11, 13, 16, 59)));
        assert!(
            !p.alt_active_at(&at(-5, 2026, 11, 13, 17, 0)),
            "end is exclusive"
        );
        assert!(!p.alt_active_at(&at(-5, 2026, 11, 13, 8, 59)));
        assert!(!p.alt_active_at(&at(-5, 2026, 11, 14, 12, 0)), "Saturday");
        let l = p.limits_at(&at(-5, 2026, 11, 13, 12, 0));
        assert_eq!(l, RateLimits::from_kbps(100, 200));
        assert_eq!(p.limits_at(&at(-5, 2026, 11, 14, 12, 0)), p.normal);
    }

    #[test]
    fn overnight_window_belongs_to_its_start_day() {
        let p = plan(vec![win(&["fri"], "22:00", "06:00")]);
        assert!(
            p.alt_active_at(&at(-5, 2026, 11, 13, 23, 0)),
            "Friday night"
        );
        assert!(
            p.alt_active_at(&at(-5, 2026, 11, 14, 5, 59)),
            "Saturday early"
        );
        assert!(!p.alt_active_at(&at(-5, 2026, 11, 14, 6, 0)));
        assert!(
            !p.alt_active_at(&at(-5, 2026, 11, 13, 5, 0)),
            "Friday early is Thursday's"
        );
        assert!(
            !p.alt_active_at(&at(-5, 2026, 11, 14, 23, 0)),
            "Saturday night"
        );
    }

    #[test]
    fn sunday_overnight_wraps_to_monday() {
        let p = plan(vec![win(&["sun"], "23:00", "01:00")]);
        // 2026-11-15 is a Sunday.
        assert!(p.alt_active_at(&at(-5, 2026, 11, 15, 23, 30)));
        assert!(p.alt_active_at(&at(-5, 2026, 11, 16, 0, 30)));
        assert!(!p.alt_active_at(&at(-5, 2026, 11, 16, 1, 0)));
    }

    #[test]
    fn dst_spring_forward_skipped_hour() {
        // US Eastern, 2026-03-08: 01:59 EST (UTC-5) is followed by 03:00
        // EDT (UTC-4). A window from 02:30 to 04:00 starts at the jump.
        let p = plan(vec![win(&["sun"], "02:30", "04:00")]);
        // 06:59 UTC = 01:59 EST: before the window.
        assert!(!p.alt_active_at(&utc_in(-5, 2026, 3, 8, 6, 59)));
        // 07:00 UTC = 03:00 EDT: the clock skipped 02:30, the window is on.
        assert!(p.alt_active_at(&utc_in(-4, 2026, 3, 8, 7, 0)));
        // 07:59 UTC = 03:59 EDT: still on; 08:00 UTC = 04:00 EDT: off.
        assert!(p.alt_active_at(&utc_in(-4, 2026, 3, 8, 7, 59)));
        assert!(!p.alt_active_at(&utc_in(-4, 2026, 3, 8, 8, 0)));
        // A window entirely inside the skipped hour never runs that night.
        let gone = plan(vec![win(&["sun"], "02:10", "02:50")]);
        assert!(!gone.alt_active_at(&utc_in(-5, 2026, 3, 8, 6, 59)));
        assert!(!gone.alt_active_at(&utc_in(-4, 2026, 3, 8, 7, 0)));
    }

    #[test]
    fn dst_fall_back_repeated_hour() {
        // US Eastern, 2026-11-01: 01:59 EDT (UTC-4) is followed by 01:00
        // EST (UTC-5), so 01:00 to 01:59 happens twice.
        let p = plan(vec![win(&["sun"], "01:00", "01:30")]);
        // First pass: 05:15 UTC = 01:15 EDT.
        assert!(p.alt_active_at(&utc_in(-4, 2026, 11, 1, 5, 15)));
        // Between passes: 05:45 UTC = 01:45 EDT.
        assert!(!p.alt_active_at(&utc_in(-4, 2026, 11, 1, 5, 45)));
        // Second pass: 06:15 UTC = 01:15 EST.
        assert!(p.alt_active_at(&utc_in(-5, 2026, 11, 1, 6, 15)));
        // After: 06:30 UTC = 01:30 EST.
        assert!(!p.alt_active_at(&utc_in(-5, 2026, 11, 1, 6, 30)));
        // A window spanning the change (00:30 to 02:30) covers both passes
        // and ends at 02:30 standard time.
        let wide = plan(vec![win(&["sun"], "00:30", "02:30")]);
        assert!(wide.alt_active_at(&utc_in(-4, 2026, 11, 1, 5, 59)));
        assert!(wide.alt_active_at(&utc_in(-5, 2026, 11, 1, 7, 29)));
        assert!(!wide.alt_active_at(&utc_in(-5, 2026, 11, 1, 7, 30)));
    }

    #[test]
    fn disabled_or_empty_schedule_is_normal() {
        let off = SpeedPlan::new(
            RateLimits::from_kbps(5, 6),
            &AltSpeed {
                enabled: false,
                window: vec![win(&[], "00:00", "23:59")],
                ..AltSpeed::default()
            },
        )
        .unwrap();
        assert!(!off.is_scheduled());
        assert_eq!(off.limits_now(), RateLimits::from_kbps(5, 6));
        let empty = SpeedPlan::new(
            RateLimits::default(),
            &AltSpeed {
                enabled: true,
                ..AltSpeed::default()
            },
        )
        .unwrap();
        assert!(!empty.is_scheduled());
    }

    #[test]
    fn seed_limits() {
        let l = SeedLimits::new(2.0, "1h", SeedLimitAction::Pause).unwrap();
        assert_eq!(l.check(100, 100, Duration::from_secs(10)), None);
        assert!(matches!(
            l.check(200, 100, Duration::from_secs(10)),
            Some(LimitHit::Ratio(_))
        ));
        assert!(matches!(
            l.check(0, 100, Duration::from_secs(3600)),
            Some(LimitHit::Time(_))
        ));
        assert_eq!(l.check(10_000, 0, Duration::ZERO), None, "empty revision");
        assert!(SeedLimits::new(-1.0, "", SeedLimitAction::Pause).is_err());
        assert!(SeedLimits::new(f64::NAN, "", SeedLimitAction::Pause).is_err());
        assert!(SeedLimits::new(0.0, "5x", SeedLimitAction::Pause).is_err());
        assert!(
            SeedLimits::new(0.0, "", SeedLimitAction::Remove)
                .unwrap()
                .is_unlimited()
        );
    }

    #[test]
    fn tracker_counts_active_time_and_prior_runs() {
        let mut t = SeedTracker::new(SeedTotals {
            uploaded_bytes: 50,
            seeding_secs: 100,
        });
        let t0 = std::time::Instant::now();
        t.tick(t0, true, 0);
        let s = t.tick(t0 + Duration::from_secs(10), true, 5);
        assert_eq!(s.seeding_secs, 110);
        assert_eq!(s.uploaded_bytes, 55);
        // Paused time does not count.
        let s = t.tick(t0 + Duration::from_secs(70), false, 5);
        assert_eq!(s.seeding_secs, 110);
        let s = t.tick(t0 + Duration::from_secs(75), true, 9);
        assert_eq!(s.seeding_secs, 115);
        assert_eq!(s.uploaded_bytes, 59);
    }
}
