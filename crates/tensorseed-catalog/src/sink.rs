// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Where the publisher writes objects (signed catalog files, and later the
//! webseed copy of model files): a local directory, which is the default and
//! needs no account anywhere, or one or more S3 compatible buckets. Nothing
//! here assumes a particular provider: any service that speaks the S3 API
//! with a custom endpoint works (Cloudflare R2, Backblaze B2, Vultr Object
//! Storage, Wasabi, MinIO, AWS S3, ...). [`MultiSink`] pushes every object
//! to several targets so a publisher can mirror across providers.
//!
//! S3 crate choice: `object_store` (Apache Arrow project, MIT/Apache-2.0).
//! It is actively maintained, supports custom endpoints, path style and
//! virtual hosted style addressing, and multipart uploads (needed for model
//! files), uses the same `reqwest` 0.13 as the rest of Tensorseed so it adds
//! no second HTTP stack, and passes `deny.toml`. `aws-sdk-s3` was rejected as
//! much heavier (the full AWS SDK runtime) and `rust-s3` because it pulls in
//! an older `reqwest` (0.12) next to ours. Credentials come only from
//! environment variables.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use object_store::aws::{AmazonS3, AmazonS3Builder};
use object_store::{
    Attribute, Attributes, ObjectStore, PutMultipartOptions, PutOptions, PutPayload, WriteMultipart,
};

use crate::{CatalogError, Result};

/// Cache policy for an uploaded object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePolicy {
    /// Rewritten objects (`head.json`): caches must revalidate quickly.
    Mutable,
    /// Append-only objects (deltas, snapshots): cache forever.
    Immutable,
}

impl CachePolicy {
    fn header(self) -> &'static str {
        match self {
            Self::Mutable => "public, max-age=60, must-revalidate",
            Self::Immutable => "public, max-age=31536000, immutable",
        }
    }
}

/// A destination for published objects.
#[async_trait]
pub trait ObjectSink: Send + Sync + fmt::Debug {
    /// Write `body` at `key` (a `/` separated relative path).
    async fn put(
        &self,
        key: &str,
        body: Vec<u8>,
        content_type: &str,
        cache: CachePolicy,
    ) -> Result<()>;
    /// Human readable destination, for logs.
    fn describe(&self) -> String;

    /// Upload a local file (model files for webseeds). The default reads it
    /// into memory; S3 targets stream large files as multipart uploads.
    async fn put_file(
        &self,
        key: &str,
        file: &std::path::Path,
        content_type: &str,
        cache: CachePolicy,
    ) -> Result<()> {
        let body = tokio::fs::read(file).await?;
        self.put(key, body, content_type, cache).await
    }
}

/// Files at or above this size go up as multipart uploads.
pub const MULTIPART_THRESHOLD: u64 = 32 << 20;
/// Multipart part size.
pub const MULTIPART_PART: usize = 16 << 20;

fn check_key(key: &str) -> Result<()> {
    if key.is_empty()
        || key.starts_with('/')
        || key.contains('\\')
        || key
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(CatalogError::Config(format!("unsafe object key {key:?}")));
    }
    Ok(())
}

/// Writes objects under a local directory, atomically (temp file + rename).
#[derive(Debug, Clone)]
pub struct DirSink {
    root: PathBuf,
}

impl DirSink {
    /// Sink rooted at `root` (created on first write).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

#[async_trait]
impl ObjectSink for DirSink {
    async fn put(
        &self,
        key: &str,
        body: Vec<u8>,
        _content_type: &str,
        _cache: CachePolicy,
    ) -> Result<()> {
        check_key(key)?;
        let path = tensorseed_core::paths::join_slash(&self.root, key);
        let parent = path
            .parent()
            .ok_or_else(|| CatalogError::Config(format!("bad key {key}")))?
            .to_path_buf();
        tokio::fs::create_dir_all(&parent).await?;
        let tmp = parent.join(format!(
            ".{}.tmp",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("object")
        ));
        tokio::fs::write(&tmp, &body).await?;
        tokio::fs::rename(&tmp, &path).await?;
        Ok(())
    }

    fn describe(&self) -> String {
        format!("directory {}", self.root.display())
    }

    async fn put_file(
        &self,
        key: &str,
        file: &std::path::Path,
        _content_type: &str,
        _cache: CachePolicy,
    ) -> Result<()> {
        check_key(key)?;
        let path = tensorseed_core::paths::join_slash(&self.root, key);
        if tokio::fs::canonicalize(&path).await.ok() == tokio::fs::canonicalize(file).await.ok()
            && path.exists()
        {
            return Ok(()); // already in place (local storage is the download dir)
        }
        let parent = path
            .parent()
            .ok_or_else(|| CatalogError::Config(format!("bad key {key}")))?
            .to_path_buf();
        tokio::fs::create_dir_all(&parent).await?;
        let tmp = parent.join(".upload.tmp");
        tokio::fs::copy(file, &tmp).await?;
        tokio::fs::rename(&tmp, &path).await?;
        Ok(())
    }
}

/// Generic S3 compatible target. Build one with [`S3Config::from_env`].
#[derive(Clone)]
pub struct S3Config {
    /// Endpoint URL such as `https://s3.us-west-004.backblazeb2.com`. Empty
    /// means AWS S3 itself (the regional endpoint for `region`).
    pub endpoint: String,
    /// Bucket name.
    pub bucket: String,
    /// Signing region (`auto` for R2, the provider's region elsewhere).
    pub region: String,
    /// Access key id.
    pub access_key_id: String,
    /// Secret access key.
    pub secret_access_key: String,
    /// Key prefix inside the bucket (no leading or trailing `/`).
    pub prefix: String,
    /// Path style addressing (`endpoint/bucket/key`), which every provider
    /// here accepts; false selects virtual hosted style (`bucket.endpoint/key`).
    pub path_style: bool,
    /// Allow plain `http://` endpoints (local MinIO, tests).
    pub allow_http: bool,
}

impl fmt::Debug for S3Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3Config")
            .field("endpoint", &self.endpoint)
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("prefix", &self.prefix)
            .field("path_style", &self.path_style)
            .finish_non_exhaustive()
    }
}

/// Non secret settings for [`S3Config::from_env`].
#[derive(Debug, Clone, Default)]
pub struct S3Settings {
    /// Endpoint URL (empty for AWS S3).
    pub endpoint: String,
    /// Region.
    pub region: String,
    /// Bucket.
    pub bucket: String,
    /// Key prefix.
    pub prefix: String,
    /// Path style addressing.
    pub path_style: bool,
    /// Env var holding the access key id.
    pub access_key_env: String,
    /// Env var holding the secret access key.
    pub secret_key_env: String,
}

impl S3Config {
    /// Build from settings, reading the two credentials from the named
    /// environment variables.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Config`] when either environment variable is unset or
    /// the settings are invalid.
    pub fn from_env(s: &S3Settings) -> Result<Self> {
        let var = |name: &str| {
            std::env::var(name)
                .ok()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    CatalogError::Config(format!("environment variable {name} is not set"))
                })
        };
        if s.bucket.is_empty() {
            return Err(CatalogError::Config("S3 target has no bucket".into()));
        }
        if s.region.is_empty() {
            return Err(CatalogError::Config("S3 target has no region".into()));
        }
        let endpoint = s.endpoint.trim_end_matches('/').to_string();
        if !endpoint.is_empty()
            && !endpoint.starts_with("https://")
            && !endpoint.starts_with("http://")
        {
            return Err(CatalogError::Config(format!(
                "S3 endpoint must be an http(s) URL: {endpoint}"
            )));
        }
        Ok(Self {
            allow_http: endpoint.starts_with("http://"),
            endpoint,
            bucket: s.bucket.clone(),
            region: s.region.clone(),
            access_key_id: var(&s.access_key_env)?,
            secret_access_key: var(&s.secret_key_env)?,
            prefix: s.prefix.trim_matches('/').into(),
            path_style: s.path_style,
        })
    }
}

/// An [`ObjectSink`] that writes to an S3 compatible bucket (AWS S3,
/// Cloudflare R2, MinIO, Backblaze B2, and similar), optionally under a
/// key prefix. Build it with [`S3Sink::new`] from an [`S3Config`].
#[derive(Debug, Clone)]
pub struct S3Sink {
    store: Arc<AmazonS3>,
    prefix: String,
    describe: String,
}

impl S3Sink {
    fn full_key(&self, key: &str) -> String {
        if self.prefix.is_empty() {
            key.to_string()
        } else {
            format!("{}/{key}", self.prefix)
        }
    }

    /// Connect (no network traffic until the first put).
    ///
    /// # Errors
    ///
    /// [`CatalogError::Config`] when the endpoint, bucket, or credentials are
    /// rejected by the S3 client builder.
    pub fn new(cfg: S3Config) -> Result<Self> {
        let mut b = AmazonS3Builder::new();
        if !cfg.endpoint.is_empty() {
            b = b.with_endpoint(&cfg.endpoint);
        }
        let store = b
            .with_bucket_name(&cfg.bucket)
            .with_region(&cfg.region)
            .with_access_key_id(&cfg.access_key_id)
            .with_secret_access_key(&cfg.secret_access_key)
            .with_virtual_hosted_style_request(!cfg.path_style)
            .with_allow_http(cfg.allow_http)
            .build()
            .map_err(|e| CatalogError::Config(format!("S3 config: {e}")))?;
        Ok(Self {
            store: Arc::new(store),
            describe: format!(
                "s3 {}/{}/{}",
                if cfg.endpoint.is_empty() {
                    "aws"
                } else {
                    &cfg.endpoint
                },
                cfg.bucket,
                cfg.prefix
            ),
            prefix: cfg.prefix,
        })
    }
}

#[async_trait]
impl ObjectSink for S3Sink {
    async fn put(
        &self,
        key: &str,
        body: Vec<u8>,
        content_type: &str,
        cache: CachePolicy,
    ) -> Result<()> {
        check_key(key)?;
        let full = self.full_key(key);
        let mut attributes = Attributes::new();
        attributes.insert(Attribute::ContentType, content_type.to_string().into());
        attributes.insert(Attribute::CacheControl, cache.header().into());
        let opts = PutOptions {
            attributes,
            ..PutOptions::default()
        };
        self.store
            .put_opts(
                &object_store::path::Path::from(full.as_str()),
                PutPayload::from(body),
                opts,
            )
            .await
            .map_err(|e| CatalogError::Backend(format!("upload of {full} failed: {e}")))?;
        Ok(())
    }

    fn describe(&self) -> String {
        self.describe.clone()
    }

    async fn put_file(
        &self,
        key: &str,
        file: &std::path::Path,
        content_type: &str,
        cache: CachePolicy,
    ) -> Result<()> {
        use tokio::io::AsyncReadExt;
        let size = tokio::fs::metadata(file).await?.len();
        if size < MULTIPART_THRESHOLD {
            let body = tokio::fs::read(file).await?;
            return self.put(key, body, content_type, cache).await;
        }
        check_key(key)?;
        let full = self.full_key(key);
        let mut attributes = Attributes::new();
        attributes.insert(Attribute::ContentType, content_type.to_string().into());
        attributes.insert(Attribute::CacheControl, cache.header().into());
        let err = |e: object_store::Error| {
            CatalogError::Backend(format!("multipart upload of {full} failed: {e}"))
        };
        let upload = self
            .store
            .put_multipart_opts(
                &object_store::path::Path::from(full.as_str()),
                PutMultipartOptions {
                    attributes,
                    ..PutMultipartOptions::default()
                },
            )
            .await
            .map_err(err)?;
        let mut w = WriteMultipart::new_with_chunk_size(upload, MULTIPART_PART);
        let mut f = tokio::fs::File::open(file).await?;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = f.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            w.wait_for_capacity(4).await.map_err(err)?;
            w.write(&buf[..n]);
        }
        w.finish().await.map_err(err)?;
        Ok(())
    }
}

/// Writes every object to each of several sinks in order (for example a
/// local directory plus two providers). A put fails if any target fails, so
/// callers never advance state that a target is missing.
#[derive(Debug, Clone)]
pub struct MultiSink {
    sinks: Vec<Arc<dyn ObjectSink>>,
}

impl MultiSink {
    /// Fan out to `sinks` (at least one).
    ///
    /// # Errors
    ///
    /// [`CatalogError::Config`] when `sinks` is empty.
    pub fn new(sinks: Vec<Arc<dyn ObjectSink>>) -> Result<Self> {
        if sinks.is_empty() {
            return Err(CatalogError::Config("no storage targets".into()));
        }
        Ok(Self { sinks })
    }
}

#[async_trait]
impl ObjectSink for MultiSink {
    async fn put(
        &self,
        key: &str,
        body: Vec<u8>,
        content_type: &str,
        cache: CachePolicy,
    ) -> Result<()> {
        for s in &self.sinks {
            s.put(key, body.clone(), content_type, cache)
                .await
                .map_err(|e| CatalogError::Backend(format!("{}: {e}", s.describe())))?;
        }
        Ok(())
    }

    async fn put_file(
        &self,
        key: &str,
        file: &std::path::Path,
        content_type: &str,
        cache: CachePolicy,
    ) -> Result<()> {
        for s in &self.sinks {
            s.put_file(key, file, content_type, cache)
                .await
                .map_err(|e| CatalogError::Backend(format!("{}: {e}", s.describe())))?;
        }
        Ok(())
    }

    fn describe(&self) -> String {
        self.sinks
            .iter()
            .map(|s| s.describe())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, header_regex, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn dir_sink_writes_atomically_and_rejects_bad_keys() {
        let dir = tempfile::tempdir().unwrap();
        let s = DirSink::new(dir.path());
        s.put(
            "main/deltas/1.json",
            b"{}".to_vec(),
            "application/json",
            CachePolicy::Immutable,
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("main/deltas/1.json")).unwrap(),
            b"{}"
        );
        for bad in ["", "/abs", "../x", "a//b"] {
            assert!(
                s.put(bad, vec![], "x", CachePolicy::Mutable).await.is_err(),
                "{bad}"
            );
        }
    }

    #[tokio::test]
    async fn s3_sink_signs_and_puts() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/bucket/pre/main/head.json"))
            .and(header_regex(
                "authorization",
                "^AWS4-HMAC-SHA256 Credential=AKID/",
            ))
            .and(header("content-type", "application/json"))
            .and(header_regex("cache-control", "max-age=60"))
            .respond_with(ResponseTemplate::new(200).insert_header("etag", "\"abc\""))
            .expect(1)
            .mount(&server)
            .await;
        let sink = S3Sink::new(S3Config {
            endpoint: server.uri(),
            bucket: "bucket".into(),
            region: "auto".into(),
            access_key_id: "AKID".into(),
            secret_access_key: "SECRET".into(),
            prefix: "pre".into(),
            path_style: true,
            allow_http: true,
        })
        .unwrap();
        sink.put(
            "main/head.json",
            b"{}".to_vec(),
            "application/json",
            CachePolicy::Mutable,
        )
        .await
        .unwrap();
        let missing = S3Config::from_env(&S3Settings {
            endpoint: "https://s3.example.invalid".into(),
            region: "auto".into(),
            bucket: "b".into(),
            access_key_env: "TS_TEST_NOPE_B".into(),
            secret_key_env: "TS_TEST_NOPE_C".into(),
            ..S3Settings::default()
        })
        .unwrap_err();
        assert!(missing.to_string().contains("TS_TEST_NOPE_B"));
    }

    #[tokio::test]
    async fn multi_sink_writes_every_target() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let m = MultiSink::new(vec![
            Arc::new(DirSink::new(a.path())),
            Arc::new(DirSink::new(b.path())),
        ])
        .unwrap();
        m.put(
            "x/y.json",
            b"1".to_vec(),
            "application/json",
            CachePolicy::Immutable,
        )
        .await
        .unwrap();
        assert!(a.path().join("x/y.json").exists() && b.path().join("x/y.json").exists());
        assert!(MultiSink::new(vec![]).is_err());
    }

    #[tokio::test]
    async fn s3_virtual_hosted_style_is_selectable() {
        let cfg = S3Config {
            endpoint: "http://127.0.0.1:9".into(),
            bucket: "bucket".into(),
            region: "us-east-1".into(),
            access_key_id: "A".into(),
            secret_access_key: "S".into(),
            prefix: String::new(),
            path_style: false,
            allow_http: true,
        };
        assert!(S3Sink::new(cfg).is_ok());
    }

    #[tokio::test]
    async fn s3_large_files_use_multipart() {
        use wiremock::matchers::query_param;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/bucket/t/big.bin"))
            .and(query_param("uploads", ""))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "<InitiateMultipartUploadResult><Bucket>bucket</Bucket><Key>t/big.bin</Key><UploadId>U1</UploadId></InitiateMultipartUploadResult>",
            ))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/bucket/t/big.bin"))
            .and(query_param("uploadId", "U1"))
            .respond_with(ResponseTemplate::new(200).insert_header("etag", "\"p\""))
            .expect(3)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/bucket/t/big.bin"))
            .and(query_param("uploadId", "U1"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "<CompleteMultipartUploadResult><ETag>\"done\"</ETag></CompleteMultipartUploadResult>",
            ))
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("big.bin");
        std::fs::write(&f, vec![7u8; (MULTIPART_THRESHOLD as usize) + 5]).unwrap();
        let sink = S3Sink::new(S3Config {
            endpoint: server.uri(),
            bucket: "bucket".into(),
            region: "auto".into(),
            access_key_id: "AKID".into(),
            secret_access_key: "SECRET".into(),
            prefix: String::new(),
            path_style: true,
            allow_http: true,
        })
        .unwrap();
        sink.put_file(
            "t/big.bin",
            &f,
            "application/octet-stream",
            CachePolicy::Immutable,
        )
        .await
        .unwrap();
    }
}
