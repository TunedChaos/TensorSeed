// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Where the publisher writes signed catalog files: a local directory (any
//! static host, or tests) or an S3 compatible bucket (Cloudflare R2 behind
//! `catalog.tensorseed.dev`, or Backblaze B2).
//!
//! S3 crate choice: `object_store` (Apache Arrow project, MIT/Apache-2.0).
//! It is actively maintained, speaks the S3 API including multipart
//! uploads (needed later for model files on the webseed), uses the same
//! `reqwest` 0.13 as the rest of Tensorseed so it adds no second HTTP stack,
//! and passes `deny.toml`. `aws-sdk-s3` was rejected as much heavier (the
//! full AWS SDK runtime) and `rust-s3` because it pulls in an older `reqwest`
//! (0.12) next to ours. Credentials come only from environment variables.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use object_store::aws::{AmazonS3, AmazonS3Builder};
use object_store::{Attribute, Attributes, ObjectStore, PutOptions, PutPayload};

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
}

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
        let path = self.root.join(key);
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
}

/// S3 connection settings. Secrets are passed in by the caller from env.
#[derive(Clone)]
pub struct S3Config {
    /// Endpoint, for example `https://<account>.r2.cloudflarestorage.com`.
    pub endpoint: String,
    /// Bucket.
    pub bucket: String,
    /// Region (`auto` for R2).
    pub region: String,
    /// Access key id.
    pub access_key_id: String,
    /// Secret access key.
    pub secret_access_key: String,
    /// Key prefix inside the bucket (may be empty).
    pub prefix: String,
    /// Allow plain HTTP endpoints (tests only).
    pub allow_http: bool,
}

impl fmt::Debug for S3Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3Config")
            .field("endpoint", &self.endpoint)
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

/// Names of the environment variables holding R2 credentials.
#[derive(Debug, Clone)]
pub struct R2Env {
    /// Account id variable (used to build the endpoint when none is given).
    pub account_id: String,
    /// Access key id variable.
    pub access_key: String,
    /// Secret access key variable.
    pub secret_key: String,
}

impl S3Config {
    /// Build a Cloudflare R2 config from environment variables. `endpoint`
    /// overrides the default `https://<account_id>.r2.cloudflarestorage.com`.
    pub fn r2_from_env(
        env: &R2Env,
        bucket: &str,
        prefix: &str,
        endpoint: Option<&str>,
    ) -> Result<Self> {
        let var = |name: &str| {
            std::env::var(name)
                .ok()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    CatalogError::Config(format!("environment variable {name} is not set"))
                })
        };
        let endpoint = match endpoint.filter(|e| !e.is_empty()) {
            Some(e) => e.to_string(),
            None => format!("https://{}.r2.cloudflarestorage.com", var(&env.account_id)?),
        };
        Ok(Self {
            endpoint,
            bucket: bucket.into(),
            region: "auto".into(),
            access_key_id: var(&env.access_key)?,
            secret_access_key: var(&env.secret_key)?,
            prefix: prefix.trim_matches('/').into(),
            allow_http: false,
        })
    }
}

/// S3 compatible sink (R2, B2, MinIO).
#[derive(Debug, Clone)]
pub struct S3Sink {
    store: Arc<AmazonS3>,
    prefix: String,
    describe: String,
}

impl S3Sink {
    /// Connect (no network traffic until the first put).
    pub fn new(cfg: S3Config) -> Result<Self> {
        let store = AmazonS3Builder::new()
            .with_endpoint(&cfg.endpoint)
            .with_bucket_name(&cfg.bucket)
            .with_region(&cfg.region)
            .with_access_key_id(&cfg.access_key_id)
            .with_secret_access_key(&cfg.secret_access_key)
            .with_virtual_hosted_style_request(false)
            .with_allow_http(cfg.allow_http)
            .build()
            .map_err(|e| CatalogError::Config(format!("S3 config: {e}")))?;
        Ok(Self {
            store: Arc::new(store),
            describe: format!("s3 {}/{}/{}", cfg.endpoint, cfg.bucket, cfg.prefix),
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
        let full = if self.prefix.is_empty() {
            key.to_string()
        } else {
            format!("{}/{key}", self.prefix)
        };
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
        assert!(
            !format!(
                "{:?}",
                S3Config::r2_from_env(
                    &R2Env {
                        account_id: "TS_TEST_NOPE_A".into(),
                        access_key: "TS_TEST_NOPE_B".into(),
                        secret_key: "TS_TEST_NOPE_C".into()
                    },
                    "b",
                    "",
                    None
                )
                .unwrap_err()
            )
            .is_empty()
        );
    }
}
