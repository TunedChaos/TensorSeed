// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Logprob challenges for verified serving (docs/plans/verified-serving.md,
//! section 5.2): `tensorseed reference-logprobs` and
//! `tensorseed check-endpoint`.
//!
//! - [`ReferenceFile`] is `ts.reference-logprobs/1`: challenge prompts and
//!   the next-token logprobs a verified copy of one entrypoint produced
//!   under fixed decoding. It is bound to the manifest hash, revision,
//!   entrypoint, runtime, and engine version, and sealed by `set_hash`
//!   (SHA-256 of the canonical JSON, SPEC 6.4, of the object without
//!   `set_hash`). A random `salt` makes the hash a hiding commitment.
//! - [`Commitment`] is the public part (`ts.reference-logprobs-commitment/1`):
//!   everything a marketplace publishes before the set is revealed.
//! - [`generate_prompts`] is the published, auditable prompt generator
//!   (`ts.challenge-gen/1`): counter mode SHA-256 over a 32-byte seed.
//! - [`parse_logprobs`] reads both OpenAI shapes: chat
//!   (`choices[0].logprobs.content[]`) and legacy completions
//!   (`choices[0].logprobs.tokens` / `token_logprobs` / `top_logprobs`).
//! - [`compare`] and [`Tolerance`] are the tolerance math; [`run_checks`]
//!   sends sampled challenges with bounded concurrency and repeats, and
//!   [`decide`] turns the per-challenge results into one verdict over the
//!   set (`ts.set-decision/1`).
//! - [`median_steps`] builds expected values from several samples of the
//!   same request, and [`Calibration`] records tolerances measured
//!   against an honest endpoint inside the sealed set.
//!
//! Nothing here holds a wallet, marketplace, or signing key. API keys are
//! read by the caller from an environment variable and are never recorded.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tensorseed_core::canonical::to_canonical_vec;
use tensorseed_core::hash::{is_lower_hex, sha256_bytes};

use crate::inference::{Entrypoint, Runtime};

/// Schema of a reference file.
pub const REFERENCE_SCHEMA: &str = "ts.reference-logprobs/1";
/// Schema of the public commitment.
pub const COMMITMENT_SCHEMA: &str = "ts.reference-logprobs-commitment/1";
/// Schema of `check-endpoint --json`.
pub const CHECK_SCHEMA: &str = "ts.check-endpoint/1";
/// Method name of a [`Calibration`].
pub const CALIBRATION_METHOD: &str = "ts.calibrate/1";
/// Rule name of [`decide`].
pub const DECISION_RULE: &str = "ts.set-decision/1";
/// Default number of samples per prompt for a reference run.
pub const DEFAULT_REFERENCE_SAMPLES: u32 = 5;
/// Prompt generator method id.
pub const GEN_METHOD: &str = "ts.challenge-gen/1";
/// Generation method for prompts read from a file.
pub const GEN_FILE: &str = "file";
/// Tool name recorded in `generator.tool`.
pub const GENERATOR_TOOL: &str = "tensorseed reference-logprobs";
/// Note recorded in `generator.note`.
pub const GENERATOR_NOTE: &str = "A reference run, not a bit-identical guarantee.";
/// Domain separator of [`generate_prompts`].
pub const GEN_DOMAIN: &[u8] = b"tensorseed challenge-gen/1\n";
/// Largest `top_logprobs` the OpenAI API accepts.
pub const MAX_TOP_LOGPROBS: u32 = 20;

/// Fixed decoding for every challenge in a set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sampling {
    /// Always 0.
    pub temperature: f64,
    /// Request seed.
    pub seed: u64,
    /// Tokens to generate per challenge.
    pub max_tokens: u32,
    /// Always true.
    pub logprobs: bool,
    /// Alternatives per position.
    pub top_logprobs: u32,
}

impl Default for Sampling {
    fn default() -> Self {
        Self {
            temperature: 0.0,
            seed: 1,
            max_tokens: 1,
            logprobs: true,
            top_logprobs: 5,
        }
    }
}

impl Sampling {
    /// Check values.
    ///
    /// # Errors
    ///
    /// Fails for a non-zero temperature, `logprobs` false, `max_tokens` 0
    /// or above 64, or `top_logprobs` outside 1..=20.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.temperature != 0.0 {
            bail!("sampling.temperature must be 0");
        }
        if !self.logprobs {
            bail!("sampling.logprobs must be true");
        }
        if self.max_tokens == 0 || self.max_tokens > 64 {
            bail!("sampling.max_tokens must be 1 to 64");
        }
        if self.top_logprobs == 0 || self.top_logprobs > MAX_TOP_LOGPROBS {
            bail!("sampling.top_logprobs must be 1 to {MAX_TOP_LOGPROBS}");
        }
        Ok(())
    }
}

/// How the reference values were produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Generator {
    /// [`GENERATOR_TOOL`].
    pub tool: String,
    /// Tensorseed version that wrote the file.
    pub tensorseed_version: String,
    /// Runtime that produced the values.
    pub runtime: Runtime,
    /// Version string the runtime reported (or `unknown`).
    pub engine_version: String,
    /// Engine specific request fields sent with every reference request
    /// (llama.cpp: `{"cache_prompt": false}`, so every value comes from a
    /// full prefill and repeats are reproducible).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_extra: Option<serde_json::Map<String, serde_json::Value>>,
    /// Requests sent per prompt; the expected values are their per-token
    /// medians ([`median_steps`]). Absent in 0.3.0 files, which used one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samples: Option<u32>,
    /// [`GENERATOR_NOTE`].
    pub note: String,
}

/// Engine specific request fields for a reference run on `rt`.
#[must_use]
pub fn reference_request_extra(rt: Runtime) -> Option<serde_json::Map<String, serde_json::Value>> {
    match rt {
        Runtime::LlamaCpp => Some(no_prompt_cache()),
        Runtime::Vllm => None,
    }
}

/// `{"cache_prompt": false}`: a llama.cpp request field that turns off
/// prompt cache reuse. Other engines ignore unknown fields or reject them.
#[must_use]
pub fn no_prompt_cache() -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert("cache_prompt".into(), serde_json::Value::Bool(false));
    m
}

/// Where the prompts came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Generation {
    /// [`GEN_METHOD`] or [`GEN_FILE`].
    pub method: String,
    /// Generator seed (64 lowercase hex) for [`GEN_METHOD`]. Secret until
    /// the set is revealed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
    /// Number of prompts.
    pub count: u32,
}

/// One chat message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    /// `system`, `user`, or `assistant`.
    pub role: String,
    /// Text.
    pub content: String,
}

/// One alternative at a position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopLogprob {
    /// Token text.
    pub token: String,
    /// Natural log probability.
    pub logprob: f64,
}

/// One generated position: the chosen token and its alternatives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    /// Chosen token.
    pub token: String,
    /// Its logprob.
    pub logprob: f64,
    /// Alternatives, most likely first.
    #[serde(default)]
    pub top_logprobs: Vec<TopLogprob>,
}

/// One challenge: a chat (`messages`) or completions (`prompt`) request
/// and the expected positions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChallengePrompt {
    /// Stable id (`lp-0001`).
    pub id: String,
    /// Chat messages (sent to `/chat/completions`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<Message>>,
    /// Raw prompt (sent to the legacy `/completions`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Expected positions (empty before the reference run).
    #[serde(default)]
    pub expected: Vec<Step>,
}

impl ChallengePrompt {
    /// True for a chat challenge.
    #[must_use]
    pub fn is_chat(&self) -> bool {
        self.messages.is_some()
    }
}

/// `ts.reference-logprobs/1`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceFile {
    /// [`REFERENCE_SCHEMA`].
    pub schema: String,
    /// SHA-256 of the signed manifest value (same as the served-weights claim).
    pub manifest_sha256: String,
    /// Catalog key of the manifest entry.
    pub manifest_key: String,
    /// Repo id.
    pub repo: String,
    /// Commit.
    pub revision: String,
    /// What was loaded: one GGUF file, or the safetensors directory.
    pub entrypoint: Entrypoint,
    /// SHA-256 of the entrypoint file (GGUF), from the signed manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint_sha256: Option<String>,
    /// Fixed decoding.
    pub sampling: Sampling,
    /// How the values were produced.
    pub generator: Generator,
    /// Where the prompts came from.
    pub generation: Generation,
    /// 32 random bytes (64 lowercase hex) so `set_hash` hides the prompts.
    pub salt: String,
    /// When the file was written (RFC 3339 UTC).
    pub created_at: String,
    /// SHA-256 (64 lowercase hex) of the canonical object without this field.
    pub set_hash: String,
    /// Tolerances measured against an honest endpoint
    /// (`tensorseed calibrate-reference`). Covered by `set_hash`, so the
    /// tolerances are committed together with the set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calibration: Option<Calibration>,
    /// The challenges.
    pub prompts: Vec<ChallengePrompt>,
}

/// Round a logprob to 6 decimals (and fold `-0` into `0`) so files are
/// stable to read, diff, and hash.
#[must_use]
pub fn round_logprob(v: f64) -> f64 {
    let r = (v * 1e6).round() / 1e6;
    if r == 0.0 { 0.0 } else { r }
}

impl ReferenceFile {
    /// `set_hash` for the current contents (the field itself is ignored).
    ///
    /// # Errors
    ///
    /// Fails only if the object cannot be serialized.
    pub fn compute_set_hash(&self) -> anyhow::Result<String> {
        let mut v = serde_json::to_value(self)?;
        if let Some(o) = v.as_object_mut() {
            o.remove("set_hash");
        }
        Ok(sha256_bytes(&to_canonical_vec(&v)?))
    }

    /// Fill in `set_hash`.
    ///
    /// # Errors
    ///
    /// Same as [`Self::compute_set_hash`].
    pub fn seal(&mut self) -> anyhow::Result<()> {
        self.set_hash = self.compute_set_hash()?;
        Ok(())
    }

    /// Check schema, structure, and `set_hash`.
    ///
    /// # Errors
    ///
    /// Fails on a wrong schema, a bad hash, duplicate or empty ids, a
    /// prompt with both or neither of `messages` and `prompt`, or a prompt
    /// without expected values.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.schema != REFERENCE_SCHEMA {
            bail!("schema is {:?}, expected {REFERENCE_SCHEMA}", self.schema);
        }
        self.sampling.validate()?;
        for (name, v) in [
            ("manifest_sha256", &self.manifest_sha256),
            ("salt", &self.salt),
            ("set_hash", &self.set_hash),
        ] {
            if !is_lower_hex(v, 64) {
                bail!("{name} must be 64 lowercase hex");
            }
        }
        if self.prompts.is_empty() {
            bail!("the set has no prompts");
        }
        let mut seen = BTreeSet::new();
        for p in &self.prompts {
            if p.id.is_empty() || !seen.insert(p.id.as_str()) {
                bail!("prompt id {:?} is empty or repeated", p.id);
            }
            if p.messages.is_some() == p.prompt.is_some() {
                bail!("prompt {} needs exactly one of messages or prompt", p.id);
            }
            if p.expected.is_empty() {
                bail!("prompt {} has no expected values", p.id);
            }
        }
        if let Some(c) = &self.calibration {
            c.validate()?;
        }
        let want = self.compute_set_hash()?;
        if want != self.set_hash {
            bail!(
                "set_hash does not match the contents (file says {}, contents hash to {want}); the file was changed after it was sealed",
                self.set_hash
            );
        }
        Ok(())
    }

    /// Read and validate a reference file.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be read, parsed, or validated.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let r: Self = serde_json::from_slice(&raw)
            .with_context(|| format!("{} is not a {REFERENCE_SCHEMA} file", path.display()))?;
        r.validate()
            .with_context(|| format!("checking {}", path.display()))?;
        Ok(r)
    }

    /// The public commitment for this set.
    #[must_use]
    pub fn commitment(&self) -> Commitment {
        Commitment {
            schema: COMMITMENT_SCHEMA.into(),
            set_hash: self.set_hash.clone(),
            manifest_sha256: self.manifest_sha256.clone(),
            manifest_key: self.manifest_key.clone(),
            repo: self.repo.clone(),
            revision: self.revision.clone(),
            entrypoint: self.entrypoint.clone(),
            entrypoint_sha256: self.entrypoint_sha256.clone(),
            sampling: self.sampling.clone(),
            generator: self.generator.clone(),
            generation_method: self.generation.method.clone(),
            prompt_count: u32::try_from(self.prompts.len()).unwrap_or(u32::MAX),
            created_at: self.created_at.clone(),
            tolerance: self.calibration.as_ref().map(|c| c.tolerance),
        }
    }
}

/// `ts.reference-logprobs-commitment/1`: safe to publish before reveal.
/// It carries no prompts, no expected values, no salt, and no generator seed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Commitment {
    /// [`COMMITMENT_SCHEMA`].
    pub schema: String,
    /// The committed `set_hash`.
    pub set_hash: String,
    /// Manifest hash the set is bound to.
    pub manifest_sha256: String,
    /// Catalog key of the manifest entry.
    pub manifest_key: String,
    /// Repo id.
    pub repo: String,
    /// Commit.
    pub revision: String,
    /// Entrypoint.
    pub entrypoint: Entrypoint,
    /// Entrypoint file hash (GGUF).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint_sha256: Option<String>,
    /// Fixed decoding.
    pub sampling: Sampling,
    /// How the values were produced.
    pub generator: Generator,
    /// [`GEN_METHOD`] or [`GEN_FILE`].
    pub generation_method: String,
    /// Number of prompts.
    pub prompt_count: u32,
    /// When the set was written.
    pub created_at: String,
    /// The calibrated tolerances, when the set was calibrated. Publishing
    /// them commits the checker to them before any check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tolerance: Option<Tolerance>,
}

// ---------------------------------------------------------------------------
// Prompt generation (`ts.challenge-gen/1`)
// ---------------------------------------------------------------------------

/// Word list of [`generate_prompts`] (256 entries, index = one byte).
#[rustfmt::skip]
pub const GEN_WORDS: [&str; 256] = [
    "apple", "river", "stone", "cloud", "light", "green", "music", "paper",
    "table", "window", "garden", "silver", "orange", "winter", "summer", "forest",
    "ocean", "bridge", "candle", "engine", "planet", "rocket", "pencil", "basket",
    "mirror", "ladder", "castle", "island", "valley", "desert", "market", "school",
    "doctor", "farmer", "artist", "sailor", "teacher", "number", "letter", "circle",
    "square", "yellow", "purple", "bright", "quiet", "gentle", "simple", "strong",
    "narrow", "ancient", "modern", "golden", "frozen", "hidden", "honest", "lucky",
    "rapid", "careful", "sudden", "distant", "early", "late", "warm", "cold",
    "soft", "sharp", "sweet", "bitter", "heavy", "empty", "full", "open",
    "closed", "small", "large", "first", "last", "north", "south", "east",
    "west", "morning", "evening", "night", "today", "always", "never", "often",
    "maybe", "because", "after", "before", "under", "over", "between", "around",
    "through", "inside", "outside", "again", "quickly", "slowly", "together", "alone",
    "water", "fire", "earth", "wind", "metal", "wood", "glass", "sand",
    "snow", "rain", "storm", "thunder", "shadow", "dream", "story", "song",
    "dance", "game", "puzzle", "answer", "question", "reason", "idea", "memory",
    "future", "history", "science", "nature", "animal", "horse", "tiger", "eagle",
    "salmon", "rabbit", "turtle", "spider", "dragon", "lemon", "bread", "cheese",
    "butter", "coffee", "honey", "pepper", "carrot", "potato", "tomato", "onion",
    "garlic", "sugar", "salt", "train", "plane", "boat", "bicycle", "road",
    "street", "city", "village", "country", "world", "kingdom", "station", "harbor",
    "tower", "temple", "palace", "library", "museum", "theater", "kitchen", "bedroom",
    "door", "roof", "floor", "wall", "chair", "clock", "camera", "phone",
    "screen", "button", "signal", "network", "machine", "robot", "tool", "hammer",
    "needle", "thread", "rope", "chain", "key", "lock", "map", "compass",
    "anchor", "flag", "crown", "coin", "treasure", "gift", "letterbox", "journey",
    "voyage", "harvest", "festival", "holiday", "season", "minute", "hour", "week",
    "month", "year", "century", "zero", "one", "two", "three", "four",
    "five", "six", "seven", "eight", "nine", "ten", "hundred", "thousand",
    "red", "blue", "black", "white", "brown", "gray", "pink", "violet",
    "run", "walk", "jump", "swim", "fly", "climb", "build", "write",
    "read", "sing", "paint", "cook", "listen", "think", "learn", "begin",
];

/// Templates of [`generate_prompts`]; `{w}` is the word sequence.
pub const GEN_TEMPLATES: [&str; 8] = [
    "Continue this text with the next word: {w}",
    "Here is a list of words: {w}. Which word should come next?",
    "Repeat these words and then continue the sequence: {w}",
    "Write one short sentence that uses these words: {w}",
    "Translate into French: {w}",
    "{w}",
    "Describe in one word what these have in common: {w}",
    "Sort these words alphabetically: {w}",
];

/// System message used on some generated prompts.
pub const GEN_SYSTEM: &str = "You are a helpful assistant.";

struct GenStream<'a> {
    seed: &'a [u8; 32],
    index: u32,
    block: u32,
    buf: [u8; 32],
    pos: usize,
}

impl GenStream<'_> {
    fn byte(&mut self) -> u8 {
        if self.pos == 32 {
            let mut h = Sha256::new();
            h.update(GEN_DOMAIN);
            h.update(self.seed);
            h.update(self.index.to_be_bytes());
            h.update(self.block.to_be_bytes());
            self.buf = h.finalize().into();
            self.block += 1;
            self.pos = 0;
        }
        let b = self.buf[self.pos];
        self.pos += 1;
        b
    }
}

/// Generate `count` chat prompts from a 32-byte seed (`ts.challenge-gen/1`).
///
/// The method is public so anyone can regenerate a revealed set: for
/// prompt `i` (0 based), a byte stream is the concatenation of
/// `SHA-256(GEN_DOMAIN || seed || be32(i) || be32(block))` for
/// `block = 0, 1, ...`. From that stream, in order: byte 0 picks the
/// template (`% 8`), byte 1 adds the system message when odd, byte 2 sets
/// the word count (`3 + b % 18`), then one byte per word indexes
/// [`GEN_WORDS`]. Words are joined with single spaces. Ids are
/// `lp-0001`, `lp-0002`, ...
#[must_use]
pub fn generate_prompts(seed: &[u8; 32], count: u32) -> Vec<ChallengePrompt> {
    (0..count)
        .map(|i| {
            let mut s = GenStream {
                seed,
                index: i,
                block: 0,
                buf: [0; 32],
                pos: 32,
            };
            let template = GEN_TEMPLATES[usize::from(s.byte() % 8)];
            let system = s.byte() % 2 == 1;
            let n = 3 + usize::from(s.byte() % 18);
            let words: Vec<&str> = (0..n).map(|_| GEN_WORDS[usize::from(s.byte())]).collect();
            let mut messages = Vec::new();
            if system {
                messages.push(Message {
                    role: "system".into(),
                    content: GEN_SYSTEM.into(),
                });
            }
            messages.push(Message {
                role: "user".into(),
                content: template.replace("{w}", &words.join(" ")),
            });
            ChallengePrompt {
                id: prompt_id(i as usize),
                messages: Some(messages),
                prompt: None,
                expected: Vec::new(),
            }
        })
        .collect()
}

/// `lp-0001` for index 0.
#[must_use]
pub fn prompt_id(i: usize) -> String {
    format!("lp-{:04}", i + 1)
}

/// Parse a 64 hex seed.
///
/// # Errors
///
/// Fails unless `s` is 64 hex digits.
pub fn parse_seed(s: &str) -> anyhow::Result<[u8; 32]> {
    let b = hex::decode(s.trim()).context("seed must be 64 hex digits")?;
    b.try_into()
        .map_err(|_| anyhow::anyhow!("seed must be 64 hex digits (32 bytes)"))
}

/// 32 random bytes from the operating system.
///
/// # Errors
///
/// Fails when the OS random source fails.
pub fn random_32() -> anyhow::Result<[u8; 32]> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("random source: {e}"))?;
    Ok(b)
}

/// Read a prompt file: a JSON array of prompts, an object with a `prompts`
/// array (such as a revealed reference file), or JSON Lines. Each prompt
/// has `messages` or `prompt` and an optional `id`; expected values are
/// dropped.
///
/// # Errors
///
/// Fails when the file is not one of those shapes or a prompt is invalid.
pub fn parse_prompt_file(raw: &[u8]) -> anyhow::Result<Vec<ChallengePrompt>> {
    #[derive(Deserialize)]
    struct In {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        messages: Option<Vec<Message>>,
        #[serde(default)]
        prompt: Option<String>,
    }
    let text = std::str::from_utf8(raw).context("prompt file is not UTF-8")?;
    let items: Vec<serde_json::Value> = match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Array(a)) => a,
        Ok(serde_json::Value::Object(o)) => match o.get("prompts") {
            Some(serde_json::Value::Array(a)) => a.clone(),
            _ => vec![serde_json::Value::Object(o)],
        },
        Ok(_) => bail!("prompt file must be a JSON array, an object with prompts, or JSON Lines"),
        Err(_) => text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .enumerate()
            .map(|(n, l)| {
                serde_json::from_str(l).with_context(|| format!("prompt file line {}", n + 1))
            })
            .collect::<anyhow::Result<_>>()?,
    };
    if items.is_empty() {
        bail!("prompt file has no prompts");
    }
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for (i, v) in items.into_iter().enumerate() {
        let p: In = serde_json::from_value(v).with_context(|| format!("prompt {}", i + 1))?;
        let id = p.id.unwrap_or_else(|| prompt_id(i));
        if !seen.insert(id.clone()) {
            bail!("prompt id {id:?} is repeated");
        }
        match (&p.messages, &p.prompt) {
            (Some(m), None) if !m.is_empty() => {}
            (None, Some(s)) if !s.is_empty() => {}
            _ => bail!("prompt {id} needs exactly one non-empty messages or prompt"),
        }
        out.push(ChallengePrompt {
            id,
            messages: p.messages,
            prompt: p.prompt,
            expected: Vec::new(),
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Requests and response parsing
// ---------------------------------------------------------------------------

/// Request path and body for one challenge.
#[must_use]
pub fn request_body(
    p: &ChallengePrompt,
    model: &str,
    s: &Sampling,
    extra: Option<&serde_json::Map<String, serde_json::Value>>,
) -> (&'static str, serde_json::Value) {
    let (path, mut body) = base_request_body(p, model, s);
    if let (Some(extra), Some(o)) = (extra, body.as_object_mut()) {
        for (k, v) in extra {
            o.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
    (path, body)
}

fn base_request_body(
    p: &ChallengePrompt,
    model: &str,
    s: &Sampling,
) -> (&'static str, serde_json::Value) {
    if let Some(m) = &p.messages {
        (
            "chat/completions",
            serde_json::json!({
                "model": model,
                "messages": m,
                "temperature": s.temperature,
                "seed": s.seed,
                "max_tokens": s.max_tokens,
                "logprobs": true,
                "top_logprobs": s.top_logprobs,
                "stream": false,
            }),
        )
    } else {
        (
            "completions",
            serde_json::json!({
                "model": model,
                "prompt": p.prompt.as_deref().unwrap_or_default(),
                "temperature": s.temperature,
                "seed": s.seed,
                "max_tokens": s.max_tokens,
                "logprobs": s.top_logprobs,
                "stream": false,
            }),
        )
    }
}

fn num(v: &serde_json::Value) -> Option<f64> {
    v.as_f64().filter(|f| f.is_finite())
}

fn parse_top_list(v: Option<&serde_json::Value>) -> Vec<TopLogprob> {
    let mut out = Vec::new();
    match v {
        // Chat shape: [{"token": "...", "logprob": -0.1}, ...]
        Some(serde_json::Value::Array(a)) => {
            for t in a {
                if let (Some(tok), Some(lp)) = (
                    t.get("token").and_then(|x| x.as_str()),
                    t.get("logprob").and_then(num),
                ) {
                    out.push(TopLogprob {
                        token: tok.to_string(),
                        logprob: lp,
                    });
                }
            }
        }
        // Legacy completions shape: {"token": -0.1, ...}
        Some(serde_json::Value::Object(o)) => {
            for (tok, lp) in o {
                if let Some(lp) = num(lp) {
                    out.push(TopLogprob {
                        token: tok.clone(),
                        logprob: lp,
                    });
                }
            }
        }
        _ => {}
    }
    out.sort_by(|a, b| b.logprob.total_cmp(&a.logprob));
    out
}

/// Positions from an OpenAI response body.
///
/// Returns `Ok(None)` when the response carries no logprobs (the endpoint
/// does not support them, or ignored the request).
///
/// # Errors
///
/// Fails when the body is not a completion response (no `choices[0]`).
pub fn parse_logprobs(body: &serde_json::Value) -> anyhow::Result<Option<Vec<Step>>> {
    let choice = body
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first())
        .context("response has no choices[0]")?;
    let Some(lp) = choice.get("logprobs").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    // Chat shape (also what llama.cpp returns for /v1/completions).
    if let Some(content) = lp.get("content") {
        let Some(arr) = content.as_array() else {
            return Ok(None);
        };
        let steps: Vec<Step> = arr
            .iter()
            .filter_map(|e| {
                Some(Step {
                    token: e.get("token")?.as_str()?.to_string(),
                    logprob: num(e.get("logprob")?)?,
                    top_logprobs: parse_top_list(e.get("top_logprobs")),
                })
            })
            .collect();
        return Ok((!steps.is_empty()).then_some(steps));
    }
    // Legacy completions shape.
    if let Some(tokens) = lp.get("tokens").and_then(|t| t.as_array()) {
        let lps = lp.get("token_logprobs").and_then(|t| t.as_array());
        let tops = lp.get("top_logprobs").and_then(|t| t.as_array());
        let mut steps = Vec::new();
        for (i, t) in tokens.iter().enumerate() {
            let Some(tok) = t.as_str() else { continue };
            let top = parse_top_list(tops.and_then(|a| a.get(i)));
            let logprob = lps
                .and_then(|a| a.get(i))
                .and_then(num)
                .or_else(|| top.iter().find(|x| x.token == tok).map(|x| x.logprob));
            let Some(logprob) = logprob else { continue };
            steps.push(Step {
                token: tok.to_string(),
                logprob,
                top_logprobs: top,
            });
        }
        return Ok((!steps.is_empty()).then_some(steps));
    }
    Ok(None)
}

/// Result of one request.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Positions with logprobs.
    Steps(Vec<Step>),
    /// A valid response without logprobs.
    NoLogprobs,
    /// Transport error, timeout, HTTP error, or an unreadable body.
    Error(String),
}

/// What a signed endpoint must sign with (from its `ts.served-weights/1` claim).
#[derive(Debug, Clone)]
pub struct SigContext {
    /// `response_key.pubkey`.
    pub pubkey: [u8; 32],
    /// `claim_sha256`.
    pub claim_sha256: String,
    /// `served_model_id`.
    pub served_model_id: String,
    /// Allowed timestamp skew (seconds).
    pub max_skew_secs: u64,
    /// The claim's `expires_at` (Unix seconds); responses signed after it
    /// fail.
    pub claim_expires_unix: Option<i64>,
    /// When a verified revocation of the claim takes effect (Unix seconds,
    /// `i64::MIN` for `key_compromised`); responses signed at or after it
    /// fail. See [`assess_revocations`].
    pub claim_revoked_unix: Option<i64>,
}

/// Signature status of one response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SigStatus {
    /// Verified against the claim.
    Verified,
    /// A claim was given but the response is not signed.
    Absent,
    /// Signed, but the signature does not verify.
    Invalid,
    /// Signed, but there is no claim to check it against.
    Unverifiable,
}

/// Signature result of one response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SigResult {
    /// Status.
    pub status: SigStatus,
    /// Why, when not verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// One response: the parsed outcome and, when checked, its signature.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    /// Logprob outcome.
    pub outcome: Outcome,
    /// Signature result (`None` when unsigned and no claim was given).
    pub signature: Option<SigResult>,
}

/// Check the signature headers of a response.
#[must_use]
pub fn check_signature(
    ctx: Option<&SigContext>,
    headers: &[(String, String)],
    body: &[u8],
    request_id: Option<&str>,
    requested_model: &str,
    now_unix: i64,
) -> Option<SigResult> {
    use crate::signing::{Expect, SigError, extract_signed, verify_fields};
    let fields = extract_signed(headers, body);
    let Some(ctx) = ctx else {
        return match fields {
            Err(SigError::Absent) => None,
            _ => Some(SigResult {
                status: SigStatus::Unverifiable,
                detail: Some("signed, but no claim was given or found".into()),
            }),
        };
    };
    let (f, body) = match fields {
        Ok(x) => x,
        Err(SigError::Absent) => {
            return Some(SigResult {
                status: SigStatus::Absent,
                detail: Some("the response is not signed".into()),
            });
        }
        Err(e) => {
            return Some(SigResult {
                status: SigStatus::Invalid,
                detail: Some(e.to_string()),
            });
        }
    };
    let e = Expect {
        pubkey: ctx.pubkey,
        claim_sha256: &ctx.claim_sha256,
        served_model_id: &ctx.served_model_id,
        request_id,
        requested_model: Some(requested_model),
        now_unix,
        max_skew_secs: ctx.max_skew_secs,
        claim_expires_unix: ctx.claim_expires_unix,
        claim_revoked_unix: ctx.claim_revoked_unix,
    };
    Some(match verify_fields(&f, body, &e) {
        Ok(()) => SigResult {
            status: SigStatus::Verified,
            detail: None,
        },
        Err(err) => SigResult {
            status: SigStatus::Invalid,
            detail: Some(err.to_string()),
        },
    })
}

/// An OpenAI compatible endpoint.
#[derive(Debug, Clone)]
pub struct Endpoint {
    /// Base URL, usually ending in `/v1`.
    pub base_url: String,
    /// Model id to request.
    pub model: String,
    /// Bearer token (read from an environment variable by the caller).
    pub api_key: Option<String>,
    /// HTTP client with the per-request timeout.
    pub http: reqwest::Client,
    /// Extra request fields (never overriding the standard ones).
    pub extra: Option<serde_json::Map<String, serde_json::Value>>,
    /// When set, every request carries a fresh `X-Tensorseed-Request-Id`
    /// and every response must be signed with this claim's key.
    pub sig: Option<SigContext>,
}

impl Endpoint {
    /// Client with a per-request timeout.
    ///
    /// # Errors
    ///
    /// Fails when the HTTP client cannot be built.
    pub fn new(
        base_url: &str,
        model: &str,
        api_key: Option<String>,
        timeout: Duration,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            api_key,
            http: reqwest::Client::builder()
                .timeout(timeout)
                .user_agent(concat!("tensorseed/", env!("CARGO_PKG_VERSION")))
                .build()?,
            extra: None,
            sig: None,
        })
    }

    /// Send one challenge.
    pub async fn ask(&self, p: &ChallengePrompt, s: &Sampling) -> Outcome {
        self.ask_signed(p, s).await.outcome
    }

    /// Send one challenge and check its signature.
    pub async fn ask_signed(&self, p: &ChallengePrompt, s: &Sampling) -> Answer {
        let err = |e: String| Answer {
            outcome: Outcome::Error(e),
            signature: None,
        };
        let (path, body) = request_body(p, &self.model, s, self.extra.as_ref());
        let url = format!("{}/{path}", self.base_url);
        let mut req = self.http.post(&url).json(&body);
        if let Some(k) = &self.api_key {
            req = req.bearer_auth(k);
        }
        let request_id = if self.sig.is_some() {
            match crate::signing::mint_request_id() {
                Ok(id) => Some(id),
                Err(e) => return err(format!("{e:#}")),
            }
        } else {
            None
        };
        if let Some(id) = &request_id {
            req = req.header(crate::signing::H_REQUEST_ID, id);
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) if e.is_timeout() => return err(format!("{path}: timed out")),
            Err(e) => return err(format!("{path}: {}", without_url(&e))),
        };
        let status = resp.status();
        let headers: Vec<(String, String)> = resp
            .headers()
            .iter()
            .filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string())))
            .collect();
        let bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => return err(format!("{path}: reading body: {}", without_url(&e))),
        };
        if !status.is_success() {
            let snippet: String = String::from_utf8_lossy(&bytes).chars().take(200).collect();
            return err(format!("{path}: HTTP {status}: {snippet}"));
        }
        let signature = check_signature(
            self.sig.as_ref(),
            &headers,
            &bytes,
            request_id.as_deref(),
            &self.model,
            i64::try_from(tensorseed_core::time::unix_now()).unwrap_or(i64::MAX),
        );
        let v: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => return err(format!("{path}: response is not JSON")),
        };
        let outcome = match parse_logprobs(&v) {
            Ok(Some(steps)) => Outcome::Steps(steps),
            Ok(None) => Outcome::NoLogprobs,
            Err(e) => Outcome::Error(format!("{path}: {e:#}")),
        };
        Answer { outcome, signature }
    }
}

fn without_url(e: &reqwest::Error) -> String {
    // The URL is shown once by the caller; reqwest repeats it in Display.
    let mut s = e.to_string();
    if let Some(u) = e.url() {
        s = s
            .replace(&format!(" ({u})"), "")
            .replace(u.as_str(), "<url>");
    }
    s
}

// ---------------------------------------------------------------------------
// Tolerance math
// ---------------------------------------------------------------------------

/// Default [`Tolerance::max_fail_fraction`].
pub const DEFAULT_MAX_FAIL_FRACTION: f64 = 0.25;
/// Default [`Tolerance::max_median_mean_abs`].
pub const DEFAULT_MAX_MEDIAN_MEAN_ABS: f64 = 0.35;

fn default_max_fail_fraction() -> f64 {
    DEFAULT_MAX_FAIL_FRACTION
}

fn default_max_median_mean_abs() -> f64 {
    DEFAULT_MAX_MEDIAN_MEAN_ABS
}

/// Thresholds, in nats: two per challenge, and two over the whole set
/// ([`decide`]). The defaults are starting points; `calibrate-reference`
/// measures an honest endpoint and writes calibrated values into the set.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Tolerance {
    /// Largest mean absolute deviation over the compared tokens of one
    /// challenge.
    pub max_mean_abs: f64,
    /// Largest deviation of any one token (spike cap).
    pub max_token_abs: f64,
    /// Largest share of scored challenges that may fail before the run
    /// fails (0 restores the 0.3.0 rule: any failing challenge fails the run).
    #[serde(default = "default_max_fail_fraction")]
    pub max_fail_fraction: f64,
    /// Largest median, over the scored challenges, of their mean absolute
    /// deviation.
    #[serde(default = "default_max_median_mean_abs")]
    pub max_median_mean_abs: f64,
}

impl Default for Tolerance {
    fn default() -> Self {
        Self {
            max_mean_abs: 0.5,
            max_token_abs: 2.0,
            max_fail_fraction: DEFAULT_MAX_FAIL_FRACTION,
            max_median_mean_abs: DEFAULT_MAX_MEDIAN_MEAN_ABS,
        }
    }
}

impl Tolerance {
    /// Check values.
    ///
    /// # Errors
    ///
    /// Fails for a negative or non-finite value, or a fail fraction above 1.
    pub fn validate(&self) -> anyhow::Result<()> {
        for (name, v) in [
            ("max_mean_abs", self.max_mean_abs),
            ("max_token_abs", self.max_token_abs),
            ("max_fail_fraction", self.max_fail_fraction),
            ("max_median_mean_abs", self.max_median_mean_abs),
        ] {
            if !v.is_finite() || v < 0.0 {
                bail!("tolerance {name} must be finite and not negative");
            }
        }
        if self.max_fail_fraction > 1.0 {
            bail!("tolerance max_fail_fraction must be at most 1");
        }
        Ok(())
    }
}

/// What a calibration run observed (nats, over every challenge of every run).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalibrationStats {
    /// Challenges scored across all runs.
    pub scored: usize,
    /// Median of the per-challenge mean deviation.
    pub mean_abs_p50: f64,
    /// 90th percentile of the per-challenge mean deviation.
    pub mean_abs_p90: f64,
    /// 99th percentile of the per-challenge mean deviation.
    pub mean_abs_p99: f64,
    /// Largest per-challenge mean deviation.
    pub mean_abs_max: f64,
    /// Largest single token deviation.
    pub token_abs_max: f64,
    /// Largest per-run median of the per-challenge mean deviation.
    pub run_median_max: f64,
    /// Share of challenges whose first chosen token matched the reference.
    pub first_token_match_rate: f64,
}

/// Tolerances measured against an honest endpoint serving the same
/// verified file (`ts.calibrate/1`), stored in the reference file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Calibration {
    /// [`CALIBRATION_METHOD`].
    pub method: String,
    /// Full passes over the set.
    pub runs: u32,
    /// Challenges per pass.
    pub challenges: u32,
    /// Engine version the calibration endpoint reported (or `unknown`).
    pub engine_version: String,
    /// True when the calibration sent llama.cpp's `cache_prompt: false`.
    pub no_prompt_cache: bool,
    /// Safety factor applied to the observations.
    pub margin: f64,
    /// What was observed.
    pub observed: CalibrationStats,
    /// The tolerances derived from it.
    pub tolerance: Tolerance,
    /// When the calibration finished (RFC 3339 UTC).
    pub created_at: String,
}

impl Calibration {
    /// Check values.
    ///
    /// # Errors
    ///
    /// Fails on an unknown method, zero runs, or invalid tolerances.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.method != CALIBRATION_METHOD {
            bail!(
                "calibration.method is {:?}, expected {CALIBRATION_METHOD}",
                self.method
            );
        }
        if self.runs == 0 || self.challenges == 0 {
            bail!("calibration needs at least one run and one challenge");
        }
        self.tolerance.validate()
    }
}

/// Round `v` up to the next multiple of 0.05 (and to 6 decimals).
fn ceil_005(v: f64) -> f64 {
    round_logprob((v / 0.05).ceil() * 0.05)
}

/// Value at quantile `q` (0..=1) of `sorted` (nearest rank).
fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * q).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Median of `v` (0 for an empty slice).
#[must_use]
pub fn median(v: &[f64]) -> f64 {
    let mut s: Vec<f64> = v.iter().copied().filter(|x| x.is_finite()).collect();
    if s.is_empty() {
        return 0.0;
    }
    s.sort_by(f64::total_cmp);
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2.0
    }
}

/// Observations from calibration runs: one `Vec<Comparison>` per run.
#[must_use]
pub fn calibration_stats(runs: &[Vec<Comparison>]) -> CalibrationStats {
    let mut means: Vec<f64> = Vec::new();
    let mut token_max = 0.0_f64;
    let mut run_median_max = 0.0_f64;
    let mut first_match = 0usize;
    for run in runs {
        let m: Vec<f64> = run.iter().map(|c| c.mean_abs_dev).collect();
        run_median_max = run_median_max.max(median(&m));
        for c in run {
            means.push(c.mean_abs_dev);
            token_max = token_max.max(c.max_abs_dev);
            first_match += usize::from(c.first_token_match);
        }
    }
    means.sort_by(f64::total_cmp);
    let n = means.len();
    CalibrationStats {
        scored: n,
        mean_abs_p50: round_logprob(quantile(&means, 0.5)),
        mean_abs_p90: round_logprob(quantile(&means, 0.9)),
        mean_abs_p99: round_logprob(quantile(&means, 0.99)),
        mean_abs_max: round_logprob(means.last().copied().unwrap_or(0.0)),
        token_abs_max: round_logprob(token_max),
        run_median_max: round_logprob(run_median_max),
        first_token_match_rate: if n == 0 {
            0.0
        } else {
            round_logprob(first_match as f64 / n as f64)
        },
    }
}

/// Tolerances from calibration observations, with safety factor `margin`
/// (at least 1). Failing challenges are allowed up to
/// `max_fail_fraction`, so the per-challenge limits sit just above the
/// honest tail rather than above every outlier, and the median limit,
/// which separates other weights best, sits above the worst honest run:
///
/// - `max_mean_abs`: the 99th percentile plus 0.1, at least the 90th
///   percentile times `margin`;
/// - `max_token_abs`: the largest observed token deviation plus 0.25, at
///   least 0.5;
/// - `max_median_mean_abs`: the largest per-run median times `margin`, at
///   least that median plus 0.15 (honest run medians move by about 0.1
///   between engine instances);
/// - `max_fail_fraction`: kept from `base`.
///
/// Every value is rounded up to a multiple of 0.05.
#[must_use]
pub fn calibrated_tolerance(o: &CalibrationStats, margin: f64, base: &Tolerance) -> Tolerance {
    let m = if margin.is_finite() {
        margin.max(1.0)
    } else {
        1.0
    };
    Tolerance {
        max_mean_abs: ceil_005((o.mean_abs_p99 + 0.1).max(o.mean_abs_p90 * m)),
        max_token_abs: ceil_005((o.token_abs_max + 0.25).max(0.5)),
        max_fail_fraction: base.max_fail_fraction,
        max_median_mean_abs: ceil_005((o.run_median_max * m).max(o.run_median_max + 0.15)),
    }
}

/// Comparison of one response against its expected positions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comparison {
    /// Reference tokens compared (across positions).
    pub compared: usize,
    /// Positions compared.
    pub positions: usize,
    /// Mean absolute deviation (nats).
    pub mean_abs_dev: f64,
    /// Largest single deviation (nats).
    pub max_abs_dev: f64,
    /// Reference tokens absent from the endpoint's list at their position.
    pub missing: usize,
    /// True when the endpoint chose the reference token at position 0.
    pub first_token_match: bool,
    /// First position where the chosen token differs (later positions
    /// are conditioned on different text and are not compared).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diverged_at: Option<usize>,
}

impl Comparison {
    /// Within `t`?
    #[must_use]
    pub fn within(&self, t: &Tolerance) -> bool {
        self.compared > 0
            && self.mean_abs_dev <= t.max_mean_abs
            && self.max_abs_dev <= t.max_token_abs
    }
}

/// Compare `actual` positions with `expected`.
///
/// At each position, every reference token (the chosen one plus its
/// top-k alternatives) is looked up in the endpoint's list (its chosen
/// token plus its top-k). A token that is present contributes
/// `|actual - expected|`. A token that is absent can have a logprob no
/// higher than the smallest one the endpoint listed (the floor), so it
/// contributes the lower bound `max(0, expected - floor)`. Comparison
/// stops after the first position whose chosen token differs, because the
/// next positions follow different text. Missing positions in `actual`
/// are not compared.
#[must_use]
pub fn compare(expected: &[Step], actual: &[Step]) -> Comparison {
    let mut devs: Vec<f64> = Vec::new();
    let mut missing = 0;
    let mut positions = 0;
    let mut diverged_at = None;
    for (i, (e, a)) in expected.iter().zip(actual).enumerate() {
        positions += 1;
        let mut have: HashMap<&str, f64> = HashMap::new();
        have.insert(a.token.as_str(), a.logprob);
        for t in &a.top_logprobs {
            have.entry(t.token.as_str()).or_insert(t.logprob);
        }
        let floor = have.values().copied().fold(f64::INFINITY, f64::min);
        let mut refs: Vec<(&str, f64)> = vec![(e.token.as_str(), e.logprob)];
        for t in &e.top_logprobs {
            if !refs.iter().any(|(k, _)| *k == t.token) {
                refs.push((t.token.as_str(), t.logprob));
            }
        }
        for (tok, lexp) in refs {
            match have.get(tok) {
                Some(lact) => devs.push((lact - lexp).abs()),
                None => {
                    missing += 1;
                    devs.push((lexp - floor).max(0.0));
                }
            }
        }
        if a.token != e.token {
            diverged_at = Some(i);
            break;
        }
    }
    let compared = devs.len();
    let mean = if compared == 0 {
        0.0
    } else {
        devs.iter().sum::<f64>() / compared as f64
    };
    let max = devs.iter().copied().fold(0.0, f64::max);
    Comparison {
        compared,
        positions,
        mean_abs_dev: round_logprob(mean),
        max_abs_dev: round_logprob(max),
        missing,
        first_token_match: matches!((expected.first(), actual.first()), (Some(e), Some(a)) if e.token == a.token),
        diverged_at,
    }
}

// ---------------------------------------------------------------------------
// check-endpoint
// ---------------------------------------------------------------------------

/// Verdict of one challenge or of a whole run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Within tolerance.
    Pass,
    /// Outside tolerance on every attempt.
    Fail,
    /// Errors, timeouts, or attempts that disagree.
    Inconclusive,
    /// The endpoint returned no logprobs.
    Unsupported,
}

impl Verdict {
    /// Lowercase label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Inconclusive => "inconclusive",
            Self::Unsupported => "unsupported",
        }
    }

    /// Process exit code: 0 pass, 1 fail, 2 inconclusive or unsupported.
    #[must_use]
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Pass => 0,
            Self::Fail => 1,
            Self::Inconclusive | Self::Unsupported => 2,
        }
    }
}

/// Result of one challenge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChallengeResult {
    /// Prompt id.
    pub id: String,
    /// Verdict.
    pub verdict: Verdict,
    /// Requests sent.
    pub attempts: u32,
    /// Comparison of the attempt reported (the first pass, or the last
    /// attempt otherwise).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comparison: Option<Comparison>,
    /// Mean deviation of every attempt that returned logprobs.
    #[serde(default)]
    pub attempt_mean_abs_dev: Vec<f64>,
    /// Why the verdict is not pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Signature of the last response, when signatures were checked or
    /// present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<SigResult>,
    /// Time across all attempts (milliseconds).
    pub elapsed_ms: u64,
}

/// Options for [`run_checks`].
#[derive(Debug, Clone, Copy)]
pub struct CheckOptions {
    /// Tolerances.
    pub tolerance: Tolerance,
    /// Attempts for a challenge that fails (at least 1). A failure counts
    /// only when every attempt fails.
    pub repeats: u32,
    /// Challenges in flight at once (at least 1).
    pub concurrency: usize,
}

/// Run one challenge with repeats.
pub async fn check_one(
    ep: &Endpoint,
    p: &ChallengePrompt,
    s: &Sampling,
    o: &CheckOptions,
) -> ChallengeResult {
    let start = Instant::now();
    let repeats = o.repeats.max(1);
    let mut attempts = 0;
    let mut means = Vec::new();
    let mut last_cmp = None;
    let mut fails = 0u32;
    let mut errors = Vec::new();
    let mut unsupported = 0u32;
    let mut signature: Option<SigResult> = None;
    while attempts < repeats {
        attempts += 1;
        let answer = ep.ask_signed(p, s).await;
        if !matches!(answer.outcome, Outcome::Error(_)) {
            signature.clone_from(&answer.signature);
            // A claimed signer that answers unsigned or with a bad signature
            // fails outright: retrying cannot make it honest.
            if ep.sig.is_some()
                && let Some(sr) = &answer.signature
                && matches!(sr.status, SigStatus::Absent | SigStatus::Invalid)
            {
                return ChallengeResult {
                    id: p.id.clone(),
                    verdict: Verdict::Fail,
                    attempts,
                    comparison: last_cmp,
                    attempt_mean_abs_dev: means,
                    reason: Some(format!(
                        "signature {}: {}",
                        if sr.status == SigStatus::Absent {
                            "absent"
                        } else {
                            "invalid"
                        },
                        sr.detail.as_deref().unwrap_or("")
                    )),
                    signature,
                    elapsed_ms: elapsed_ms(start),
                };
            }
        }
        match answer.outcome {
            Outcome::Steps(steps) => {
                let c = compare(&p.expected, &steps);
                means.push(c.mean_abs_dev);
                if c.within(&o.tolerance) {
                    let verdict = if fails == 0 && errors.is_empty() {
                        Verdict::Pass
                    } else {
                        Verdict::Inconclusive
                    };
                    return ChallengeResult {
                        id: p.id.clone(),
                        verdict,
                        attempts,
                        comparison: Some(c),
                        attempt_mean_abs_dev: means,
                        reason: (verdict != Verdict::Pass).then(|| {
                            "attempts disagree: an earlier attempt was outside tolerance or failed"
                                .to_string()
                        }),
                        signature,
                        elapsed_ms: elapsed_ms(start),
                    };
                }
                fails += 1;
                last_cmp = Some(c);
            }
            Outcome::NoLogprobs => {
                unsupported += 1;
                // Asking again will not add logprobs.
                break;
            }
            Outcome::Error(e) => {
                errors.push(e);
                if attempts < repeats {
                    // Brief backoff so a busy or warming server can recover.
                    tokio::time::sleep(Duration::from_millis(250 * u64::from(attempts))).await;
                }
            }
        }
    }
    let (verdict, reason) = if unsupported > 0 && fails == 0 {
        (
            Verdict::Unsupported,
            "the endpoint returned no logprobs".to_string(),
        )
    } else if fails == attempts {
        let c = last_cmp.as_ref();
        (
            Verdict::Fail,
            format!(
                "outside tolerance on all {attempts} attempt(s) (last mean {:.3}, max {:.3} nats)",
                c.map_or(0.0, |c| c.mean_abs_dev),
                c.map_or(0.0, |c| c.max_abs_dev)
            ),
        )
    } else if errors.is_empty() {
        (Verdict::Inconclusive, "attempts disagree".to_string())
    } else {
        (Verdict::Inconclusive, summarize_errors(&errors))
    };
    ChallengeResult {
        id: p.id.clone(),
        verdict,
        attempts,
        comparison: last_cmp,
        attempt_mean_abs_dev: means,
        reason: Some(reason),
        signature,
        elapsed_ms: elapsed_ms(start),
    }
}

/// Distinct errors in first-seen order, with a count when repeated.
fn summarize_errors(errors: &[String]) -> String {
    let mut seen: Vec<(&str, usize)> = Vec::new();
    for e in errors {
        match seen.iter_mut().find(|(k, _)| *k == e.as_str()) {
            Some((_, n)) => *n += 1,
            None => seen.push((e.as_str(), 1)),
        }
    }
    seen.iter()
        .map(|(e, n)| {
            if *n > 1 {
                format!("{e} (x{n})")
            } else {
                (*e).to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn elapsed_ms(t: Instant) -> u64 {
    u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Run `prompts` against `ep` with at most `o.concurrency` in flight.
/// Results keep the order of `prompts`.
pub async fn run_checks(
    ep: &Endpoint,
    prompts: &[&ChallengePrompt],
    s: &Sampling,
    o: &CheckOptions,
) -> Vec<ChallengeResult> {
    use futures_util::StreamExt;
    let mut out: Vec<(usize, ChallengeResult)> =
        futures_util::stream::iter(prompts.iter().enumerate())
            .map(|(i, p)| async move { (i, check_one(ep, p, s, o).await) })
            .buffer_unordered(o.concurrency.max(1))
            .collect()
            .await;
    out.sort_by_key(|(i, _)| *i);
    out.into_iter().map(|(_, r)| r).collect()
}

/// The 0.3.0 overall rule, kept for callers that want it: any fail is a
/// fail; all pass is a pass; all unsupported is unsupported; anything else
/// is inconclusive. `check-endpoint` uses [`decide`].
#[must_use]
pub fn overall(results: &[ChallengeResult]) -> Verdict {
    if results.is_empty() {
        return Verdict::Inconclusive;
    }
    if results.iter().any(|r| r.verdict == Verdict::Fail) {
        Verdict::Fail
    } else if results.iter().all(|r| r.verdict == Verdict::Pass) {
        Verdict::Pass
    } else if results.iter().all(|r| r.verdict == Verdict::Unsupported) {
        Verdict::Unsupported
    } else {
        Verdict::Inconclusive
    }
}

/// How [`decide`] reached the verdict (`ts.set-decision/1`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetDecision {
    /// [`DECISION_RULE`].
    pub rule: String,
    /// Challenges with a comparison (pass, fail, or attempts that disagree).
    pub scored: usize,
    /// Challenges that failed on every attempt.
    pub failed: usize,
    /// `failed / scored` (0 when nothing was scored).
    pub fail_fraction: f64,
    /// Median over the scored challenges of their mean deviation (nats).
    pub median_mean_abs_dev: f64,
    /// Responses that were unsigned or badly signed while a claim was in force.
    pub signature_failures: usize,
    /// One line explanation of the verdict.
    pub reason: String,
}

/// Overall verdict over the set (`ts.set-decision/1`).
///
/// Low-bit quantizations and engines such as llama.cpp return logprobs that
/// move by tenths of a nat between identical requests, so one challenge
/// just outside tolerance is expected now and then from an honest endpoint,
/// while serving other weights moves most challenges. In order:
///
/// 1. Any response that is unsigned or badly signed under a claim: `fail`.
/// 2. Nothing sent: `inconclusive`. Every challenge unsupported: `unsupported`.
/// 3. Fewer than half of the challenges scored (errors, timeouts, no
///    logprobs): `inconclusive`.
/// 4. More than `max_fail_fraction` of the scored challenges failed, or the
///    median of their mean deviations is above `max_median_mean_abs`: `fail`.
/// 5. Otherwise `pass`.
#[must_use]
pub fn decide(results: &[ChallengeResult], t: &Tolerance) -> (Verdict, SetDecision) {
    let signature_failures = results
        .iter()
        .filter(|r| {
            r.verdict == Verdict::Fail
                && r.signature
                    .as_ref()
                    .is_some_and(|s| matches!(s.status, SigStatus::Absent | SigStatus::Invalid))
        })
        .count();
    let scored: Vec<&ChallengeResult> = results
        .iter()
        .filter(|r| r.comparison.is_some() && r.verdict != Verdict::Unsupported)
        .filter(|r| {
            !(r.verdict == Verdict::Fail
                && r.signature
                    .as_ref()
                    .is_some_and(|s| matches!(s.status, SigStatus::Absent | SigStatus::Invalid)))
        })
        .collect();
    let failed = scored.iter().filter(|r| r.verdict == Verdict::Fail).count();
    let fail_fraction = if scored.is_empty() {
        0.0
    } else {
        round_logprob(failed as f64 / scored.len() as f64)
    };
    let means: Vec<f64> = scored
        .iter()
        .filter_map(|r| r.comparison.as_ref().map(|c| c.mean_abs_dev))
        .collect();
    let median_mean_abs_dev = round_logprob(median(&means));
    let (verdict, reason) = if signature_failures > 0 {
        (
            Verdict::Fail,
            format!("{signature_failures} response(s) unsigned or badly signed under the claim"),
        )
    } else if results.is_empty() {
        (Verdict::Inconclusive, "no challenges were sent".to_string())
    } else if results.iter().all(|r| r.verdict == Verdict::Unsupported) {
        (
            Verdict::Unsupported,
            "the endpoint returned no logprobs".to_string(),
        )
    } else if scored.len() * 2 < results.len() {
        (
            Verdict::Inconclusive,
            format!(
                "only {} of {} challenges could be scored",
                scored.len(),
                results.len()
            ),
        )
    } else if fail_fraction > t.max_fail_fraction {
        (
            Verdict::Fail,
            format!(
                "{failed} of {} scored challenges outside tolerance ({:.0}% > {:.0}%)",
                scored.len(),
                fail_fraction * 100.0,
                t.max_fail_fraction * 100.0
            ),
        )
    } else if median_mean_abs_dev > t.max_median_mean_abs {
        (
            Verdict::Fail,
            format!(
                "median deviation {median_mean_abs_dev:.3} nats > {}",
                t.max_median_mean_abs
            ),
        )
    } else {
        (
            Verdict::Pass,
            format!(
                "{failed} of {} scored challenges outside tolerance (max {:.0}%), median deviation {median_mean_abs_dev:.3} nats (max {})",
                scored.len(),
                t.max_fail_fraction * 100.0,
                t.max_median_mean_abs
            ),
        )
    };
    (
        verdict,
        SetDecision {
            rule: DECISION_RULE.into(),
            scored: scored.len(),
            failed,
            fail_fraction,
            median_mean_abs_dev,
            signature_failures,
            reason,
        },
    )
}

/// Pick `n` prompt indexes (all when `n` is 0 or at least the set size),
/// ordered by `SHA-256(seed || id)`, so a run is reproducible from its seed.
#[must_use]
pub fn sample_indexes(prompts: &[ChallengePrompt], n: usize, seed: &[u8; 32]) -> Vec<usize> {
    let mut keyed: Vec<([u8; 32], usize)> = prompts
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mut h = Sha256::new();
            h.update(seed);
            h.update(p.id.as_bytes());
            (h.finalize().into(), i)
        })
        .collect();
    keyed.sort();
    let take = if n == 0 {
        keyed.len()
    } else {
        n.min(keyed.len())
    };
    let mut v: Vec<usize> = keyed.into_iter().take(take).map(|(_, i)| i).collect();
    v.sort_unstable();
    v
}

/// Counts per verdict.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    /// Passed.
    pub pass: usize,
    /// Failed.
    pub fail: usize,
    /// Inconclusive.
    pub inconclusive: usize,
    /// Unsupported.
    pub unsupported: usize,
}

impl Counts {
    /// Count `results`.
    #[must_use]
    pub fn of(results: &[ChallengeResult]) -> Self {
        let mut c = Self::default();
        for r in results {
            match r.verdict {
                Verdict::Pass => c.pass += 1,
                Verdict::Fail => c.fail += 1,
                Verdict::Inconclusive => c.inconclusive += 1,
                Verdict::Unsupported => c.unsupported += 1,
            }
        }
        c
    }
}

/// The reference a check ran against (no prompts, no expected values).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckReference {
    /// `set_hash` of the reference file.
    pub set_hash: String,
    /// Manifest hash.
    pub manifest_sha256: String,
    /// Repo id.
    pub repo: String,
    /// Commit.
    pub revision: String,
    /// Entrypoint.
    pub entrypoint: Entrypoint,
    /// Runtime that produced the reference.
    pub runtime: Runtime,
    /// Its version.
    pub engine_version: String,
    /// Prompts in the set.
    pub prompt_count: usize,
}

/// Which challenges were sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckSample {
    /// `--sample` (0 means all).
    pub requested: usize,
    /// Challenges sent.
    pub sent: usize,
    /// Sampling seed (64 lowercase hex), to reproduce the selection.
    pub seed: String,
}

/// `ts.check-endpoint/1`: the `--json` report. Carries no prompt text, no
/// expected values, and no credentials.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckReport {
    /// [`CHECK_SCHEMA`].
    pub schema: String,
    /// Tensorseed version.
    pub tensorseed_version: String,
    /// Endpoint base URL.
    pub base_url: String,
    /// Model id requested.
    pub model_id: String,
    /// The reference.
    pub reference: CheckReference,
    /// Decoding sent.
    pub sampling: Sampling,
    /// Tolerances applied.
    pub tolerance: Tolerance,
    /// Attempts allowed per failing challenge.
    pub repeats: u32,
    /// Concurrency used.
    pub concurrency: usize,
    /// Per-request timeout (seconds).
    pub timeout_secs: u64,
    /// Selection.
    pub sample: CheckSample,
    /// Overall verdict.
    pub verdict: Verdict,
    /// How the verdict was reached over the set (added in 0.3.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<SetDecision>,
    /// Where the tolerances came from: `default`, `calibration`, `flags`,
    /// or `calibration+flags` (added in 0.3.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tolerance_source: Option<String>,
    /// Counts.
    pub counts: Counts,
    /// Signature summary, when a claim was used or signatures were seen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signatures: Option<SignatureSummary>,
    /// Revocation state of the claim, when a claim was used (added in
    /// 0.3.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revocation: Option<RevocationSummary>,
    /// Per challenge, in set order.
    pub challenges: Vec<ChallengeResult>,
    /// When the check finished (RFC 3339 UTC).
    pub checked_at: String,
}

/// Signature summary across a check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignatureSummary {
    /// Where the claim came from (path or URL), when one was used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_source: Option<String>,
    /// The claim's `claim_sha256`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_sha256: Option<String>,
    /// The claim's response public key (hex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_pubkey: Option<String>,
    /// The seller wallet, when the claim's wallet signature verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seller_wallet: Option<String>,
    /// Responses verified.
    pub verified: u32,
    /// Responses with an invalid signature.
    pub invalid: u32,
    /// Responses that were not signed.
    pub absent: u32,
    /// Signed responses with no claim to check against.
    pub unverifiable: u32,
}

impl SignatureSummary {
    /// Count the per challenge statuses.
    #[must_use]
    pub fn count(results: &[ChallengeResult]) -> (u32, u32, u32, u32) {
        let mut c = (0, 0, 0, 0);
        for s in results.iter().filter_map(|r| r.signature.as_ref()) {
            match s.status {
                SigStatus::Verified => c.0 += 1,
                SigStatus::Invalid => c.1 += 1,
                SigStatus::Absent => c.2 += 1,
                SigStatus::Unverifiable => c.3 += 1,
            }
        }
        c
    }
}

/// Revocation state of the claim a check ran under (added in 0.3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevocationStatus {
    /// A list was read and no verified entry revokes the claim.
    NotRevoked,
    /// The seller publishes no list (HTTP 404) and no other list was given.
    NoList,
    /// A verified revocation takes effect later; responses signed before
    /// then still verify.
    Pending,
    /// A verified revocation is in effect: the run fails.
    Revoked,
    /// A list could not be read or did not parse, so the claim may be
    /// revoked: a run that would pass is inconclusive.
    Unreadable,
    /// No list was looked at.
    NotChecked,
}

impl RevocationStatus {
    /// The snake case name used in reports.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotRevoked => "not_revoked",
            Self::NoList => "no_list",
            Self::Pending => "pending",
            Self::Revoked => "revoked",
            Self::Unreadable => "unreadable",
            Self::NotChecked => "not_checked",
        }
    }
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// What a check found about revocations of its claim (added in 0.3.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevocationSummary {
    /// Outcome.
    pub status: RevocationStatus,
    /// One line explanation, worded as `check-claim` words it.
    pub detail: String,
    /// The lists that were read (URLs or paths).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lists: Option<String>,
    /// The verified revocation's `revoked_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<String>,
    /// Its `reason`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Who signed it: `response_key`, `wallet`, or `both`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_by: Option<String>,
    /// Entries naming the claim whose signatures did not verify (ignored).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub ignored: usize,
}

/// Assess the revocations of `claim` at `now_unix`, as `check-claim` does
/// (the entries are verified by [`tensorseed_verify::revocation`]).
/// `seller_error` is a failure to read the seller's list when other lists
/// were read: without a verified revocation the result is then
/// [`RevocationStatus::Unreadable`], because the missing list may hold one.
///
/// Returns the summary and the time from which responses under the claim
/// no longer verify, for [`SigContext::claim_revoked_unix`].
#[must_use]
pub fn assess_revocations(
    claim: &crate::signing::ServedClaim,
    input: &crate::claim_check::RevocationInput,
    seller_error: Option<&str>,
    now_unix: i64,
) -> (RevocationSummary, Option<i64>) {
    use crate::claim_check::{ClaimCheckOptions, RevocationInput, check_revocation};
    use crate::signing::REASON_KEY_COMPROMISED;
    let opts = ClaimCheckOptions {
        now_unix,
        ..ClaimCheckOptions::default()
    };
    let detail = check_revocation(claim, input, &opts).detail;
    let mut out = RevocationSummary {
        status: RevocationStatus::NotChecked,
        detail,
        lists: None,
        revoked_at: None,
        reason: None,
        revoked_by: None,
        ignored: 0,
    };
    let mut from_unix = None;
    match input {
        RevocationInput::NotChecked(_) => {}
        RevocationInput::Absent(_) => out.status = RevocationStatus::NoList,
        RevocationInput::Unavailable(_) => out.status = RevocationStatus::Unreadable,
        RevocationInput::List { list, from } => {
            out.lists = Some(from.clone());
            let f = list.find(claim);
            out.ignored = f.ignored.len();
            match f.revoked {
                Some((r, by)) => {
                    let at = r.revoked_unix().unwrap_or(i64::MIN);
                    out.status = if r.reason == REASON_KEY_COMPROMISED || at <= now_unix {
                        RevocationStatus::Revoked
                    } else {
                        RevocationStatus::Pending
                    };
                    from_unix = Some(r.effective_unix().unwrap_or(i64::MIN));
                    out.revoked_at = Some(r.revoked_at.clone());
                    out.reason = Some(r.reason.clone());
                    out.revoked_by = Some(
                        match by {
                            crate::signing::RevokedBy::ResponseKey => "response_key",
                            crate::signing::RevokedBy::Wallet => "wallet",
                            crate::signing::RevokedBy::Both => "both",
                        }
                        .to_string(),
                    );
                }
                None => {
                    if let Some(e) = seller_error {
                        out.status = RevocationStatus::Unreadable;
                        out.detail = format!("could not read revocations: {e}; {}", out.detail);
                    } else {
                        out.status = RevocationStatus::NotRevoked;
                    }
                }
            }
        }
    }
    (out, from_unix)
}

/// Fold the revocation state into the run's verdict: a revocation in
/// effect fails the run, and a list that could not be read turns a pass
/// into `inconclusive` (the claim may be revoked). Anything else leaves
/// the verdict alone.
pub fn apply_revocations(
    verdict: Verdict,
    decision: &mut SetDecision,
    r: &RevocationSummary,
) -> Verdict {
    match r.status {
        RevocationStatus::Revoked => {
            decision.reason = format!("the claim is revoked ({}); {}", r.detail, decision.reason);
            Verdict::Fail
        }
        RevocationStatus::Unreadable if verdict == Verdict::Pass => {
            decision.reason = format!(
                "the claim's revocations could not be checked ({}); {}",
                r.detail, decision.reason
            );
            Verdict::Inconclusive
        }
        _ => verdict,
    }
}

impl CheckReference {
    /// From a reference file.
    #[must_use]
    pub fn of(r: &ReferenceFile) -> Self {
        Self {
            set_hash: r.set_hash.clone(),
            manifest_sha256: r.manifest_sha256.clone(),
            repo: r.repo.clone(),
            revision: r.revision.clone(),
            entrypoint: r.entrypoint.clone(),
            runtime: r.generator.runtime,
            engine_version: r.generator.engine_version.clone(),
            prompt_count: r.prompts.len(),
        }
    }
}

/// Expected positions from several samples of the same request: the
/// per-token median.
///
/// At each position the chosen token is the one most samples chose (ties go
/// to the higher median logprob). Every token listed by any sample gets the
/// median of its logprobs, where a sample that does not list it counts as
/// below its list (so a token most samples do not list is dropped).
/// Alternatives are sorted most likely first. Only samples that chose the
/// same tokens at every earlier position are used for a position, and the
/// positions stop when fewer than half of the samples remain. One sample is
/// returned unchanged.
#[must_use]
pub fn median_steps(samples: &[Vec<Step>]) -> Vec<Step> {
    match samples {
        [] => return Vec::new(),
        [one] => return one.clone(),
        _ => {}
    }
    let need = samples.len().div_ceil(2);
    let mut live: Vec<&Vec<Step>> = samples.iter().collect();
    let mut out = Vec::new();
    let max_len = samples.iter().map(Vec::len).max().unwrap_or(0);
    for i in 0..max_len {
        live.retain(|s| s.len() > i);
        if live.len() < need {
            break;
        }
        let n = live.len();
        // Every token and the logprobs the samples gave it at this position.
        let mut values: Vec<(String, Vec<f64>)> = Vec::new();
        for s in &live {
            let st = &s[i];
            let mut seen: BTreeSet<&str> = BTreeSet::new();
            let listed = std::iter::once((st.token.as_str(), st.logprob)).chain(
                st.top_logprobs
                    .iter()
                    .map(|t| (t.token.as_str(), t.logprob)),
            );
            for (tok, lp) in listed {
                if !seen.insert(tok) {
                    continue;
                }
                match values.iter_mut().find(|(t, _)| t == tok) {
                    Some((_, v)) => v.push(lp),
                    None => values.push((tok.to_string(), vec![lp])),
                }
            }
        }
        let med = |v: &[f64]| -> Option<f64> {
            // Absent from more than half of the samples: below the lists.
            if v.len() * 2 < n {
                return None;
            }
            let mut all: Vec<f64> = v.to_vec();
            all.resize(n, f64::NEG_INFINITY);
            all.sort_by(f64::total_cmp);
            let m = if n % 2 == 1 {
                all[n / 2]
            } else {
                (all[n / 2 - 1] + all[n / 2]) / 2.0
            };
            m.is_finite().then_some(m)
        };
        let mut chosen_counts: Vec<(&str, usize)> = Vec::new();
        for s in &live {
            let t = s[i].token.as_str();
            match chosen_counts.iter_mut().find(|(k, _)| *k == t) {
                Some((_, c)) => *c += 1,
                None => chosen_counts.push((t, 1)),
            }
        }
        let median_of = |tok: &str| {
            values
                .iter()
                .find(|(t, _)| t == tok)
                .and_then(|(_, v)| med(v))
                .unwrap_or(f64::NEG_INFINITY)
        };
        let Some(&(chosen, _)) = chosen_counts.iter().max_by(|a, b| {
            a.1.cmp(&b.1)
                .then_with(|| median_of(a.0).total_cmp(&median_of(b.0)))
                .then_with(|| b.0.cmp(a.0))
        }) else {
            break;
        };
        let chosen = chosen.to_string();
        let chosen_lp = Some(median_of(&chosen))
            .filter(|m| m.is_finite())
            .unwrap_or_else(|| {
                let v: Vec<f64> = live
                    .iter()
                    .filter(|s| s[i].token == chosen)
                    .map(|s| s[i].logprob)
                    .collect();
                median(&v)
            });
        let mut top: Vec<TopLogprob> = values
            .iter()
            .filter_map(|(t, v)| {
                med(v).map(|m| TopLogprob {
                    token: t.clone(),
                    logprob: m,
                })
            })
            .collect();
        top.sort_by(|a, b| {
            b.logprob
                .total_cmp(&a.logprob)
                .then_with(|| a.token.cmp(&b.token))
        });
        out.push(Step {
            token: chosen.clone(),
            logprob: chosen_lp,
            top_logprobs: top,
        });
        live.retain(|s| s[i].token == chosen);
    }
    out
}

/// Positions to store as expected values: rounded, at most `top` alternatives.
#[must_use]
pub fn expected_from(steps: Vec<Step>, top: u32) -> Vec<Step> {
    steps
        .into_iter()
        .map(|s| Step {
            token: s.token,
            logprob: round_logprob(s.logprob),
            top_logprobs: s
                .top_logprobs
                .into_iter()
                .take(top as usize)
                .map(|t| TopLogprob {
                    token: t.token,
                    logprob: round_logprob(t.logprob),
                })
                .collect(),
        })
        .collect()
}

/// First line of a runtime's `--version` output that mentions a version,
/// or the first non-empty line, trimmed to 200 characters.
#[must_use]
pub fn engine_version_line(output: &str) -> Option<String> {
    let lines: Vec<&str> = output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let pick = lines
        .iter()
        .find(|l| l.to_ascii_lowercase().starts_with("version"))
        .or_else(|| {
            lines
                .iter()
                .find(|l| l.to_ascii_lowercase().contains("version"))
        })
        .or_else(|| lines.first())?;
    Some(pick.chars().take(200).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(tok: &str, lp: f64, top: &[(&str, f64)]) -> Step {
        Step {
            token: tok.into(),
            logprob: lp,
            top_logprobs: top
                .iter()
                .map(|(t, l)| TopLogprob {
                    token: (*t).into(),
                    logprob: *l,
                })
                .collect(),
        }
    }

    fn sample_file() -> ReferenceFile {
        let mut prompts = generate_prompts(&[7; 32], 3);
        for p in &mut prompts {
            p.expected = vec![step("Hello", -0.1, &[("Hello", -0.1), ("Hi", -2.5)])];
        }
        let mut r = ReferenceFile {
            schema: REFERENCE_SCHEMA.into(),
            manifest_sha256: "a".repeat(64),
            manifest_key: "ts/v1/repo/model/org/tiny/rev/x".into(),
            repo: "org/tiny".into(),
            revision: "0".repeat(40),
            entrypoint: Entrypoint::Gguf("tiny-q4.gguf".into()),
            entrypoint_sha256: Some("b".repeat(64)),
            sampling: Sampling::default(),
            generator: Generator {
                tool: GENERATOR_TOOL.into(),
                tensorseed_version: "0.3.0".into(),
                runtime: Runtime::LlamaCpp,
                engine_version: "version: test".into(),
                request_extra: reference_request_extra(Runtime::LlamaCpp),
                samples: None,
                note: GENERATOR_NOTE.into(),
            },
            generation: Generation {
                method: GEN_METHOD.into(),
                seed: Some(hex::encode([7; 32])),
                count: 3,
            },
            salt: "c".repeat(64),
            created_at: "2026-11-13T00:00:00Z".into(),
            set_hash: String::new(),
            calibration: None,
            prompts,
        };
        r.seal().unwrap();
        r
    }

    #[test]
    fn set_hash_is_canonical_and_detects_changes() {
        let r = sample_file();
        r.validate().unwrap();
        // The hash covers the canonical object without set_hash.
        let mut v = serde_json::to_value(&r).unwrap();
        v.as_object_mut().unwrap().remove("set_hash");
        assert_eq!(r.set_hash, sha256_bytes(&to_canonical_vec(&v).unwrap()));
        // Pretty printing and key order do not change it.
        let pretty = serde_json::to_string_pretty(&r).unwrap();
        let back: ReferenceFile = serde_json::from_str(&pretty).unwrap();
        assert_eq!(back.compute_set_hash().unwrap(), r.set_hash);
        // Any change to an expected value, prompt, salt, or binding does.
        for f in [
            |r: &mut ReferenceFile| r.prompts[0].expected[0].logprob = -0.2,
            |r: &mut ReferenceFile| r.prompts[1].messages.as_mut().unwrap()[0].content.push('x'),
            |r: &mut ReferenceFile| r.salt = "d".repeat(64),
            |r: &mut ReferenceFile| r.manifest_sha256 = "e".repeat(64),
            |r: &mut ReferenceFile| r.entrypoint = Entrypoint::Gguf("tiny-q8.gguf".into()),
        ] {
            let mut t = r.clone();
            f(&mut t);
            let e = t.validate().unwrap_err().to_string();
            assert!(e.contains("set_hash does not match"), "{e}");
        }
        // The commitment carries no secrets.
        let c = serde_json::to_string(&r.commitment()).unwrap();
        assert!(c.contains(&r.set_hash));
        assert!(!c.contains("Hello") && !c.contains(&r.salt) && !c.contains(&hex::encode([7; 32])));
        assert!(!c.contains("lp-0001"));
    }

    #[test]
    fn validate_rejects_bad_structure() {
        let mut r = sample_file();
        r.prompts[1].id = r.prompts[0].id.clone();
        r.seal().unwrap();
        assert!(r.validate().unwrap_err().to_string().contains("repeated"));
        let mut r = sample_file();
        r.prompts[0].prompt = Some("x".into());
        r.seal().unwrap();
        assert!(
            r.validate()
                .unwrap_err()
                .to_string()
                .contains("exactly one")
        );
        let mut r = sample_file();
        r.sampling.temperature = 0.7;
        r.seal().unwrap();
        assert!(r.validate().is_err());
        let mut r = sample_file();
        r.schema = "ts.reference-logprobs/2".into();
        assert!(r.validate().is_err());
    }

    #[test]
    fn generator_is_deterministic_and_pinned() {
        let a = generate_prompts(&[1; 32], 5);
        let b = generate_prompts(&[1; 32], 5);
        assert_eq!(a, b);
        assert_ne!(a, generate_prompts(&[2; 32], 5));
        assert_eq!(a[0].id, "lp-0001");
        assert_eq!(a[4].id, "lp-0005");
        // A longer set starts with the same prompts.
        assert_eq!(generate_prompts(&[1; 32], 9)[..5], a[..]);
        // Pin the published method with a test vector.
        let mut h = Sha256::new();
        h.update(GEN_DOMAIN);
        h.update([1u8; 32]);
        h.update(0u32.to_be_bytes());
        h.update(0u32.to_be_bytes());
        let block: [u8; 32] = h.finalize().into();
        let n = 3 + usize::from(block[2] % 18);
        let words: Vec<&str> = (0..n)
            .map(|k| GEN_WORDS[usize::from(block[3 + k])])
            .collect();
        let msgs = a[0].messages.as_ref().unwrap();
        let user = msgs.last().unwrap();
        assert_eq!(user.role, "user");
        assert_eq!(
            user.content,
            GEN_TEMPLATES[usize::from(block[0] % 8)].replace("{w}", &words.join(" "))
        );
        assert_eq!(msgs.len() == 2, block[1] % 2 == 1);
        // Word list entries are unique.
        let set: BTreeSet<&str> = GEN_WORDS.iter().copied().collect();
        assert_eq!(set.len(), 256);
    }

    #[test]
    fn parses_chat_logprobs() {
        let v = serde_json::json!({
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "Hi"},
              "logprobs": {"content": [
                {"token": "Hi", "logprob": -0.25, "bytes": [72, 105],
                 "top_logprobs": [{"token": "Hello", "logprob": -1.75}, {"token": "Hi", "logprob": -0.25}]},
                {"token": "!", "logprob": -0.5, "top_logprobs": []}
              ]}}]
        });
        let s = parse_logprobs(&v).unwrap().unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].token, "Hi");
        assert_eq!(s[0].logprob, -0.25);
        // Sorted most likely first.
        assert_eq!(s[0].top_logprobs[0].token, "Hi");
        assert_eq!(s[0].top_logprobs[1].token, "Hello");
        assert!(s[1].top_logprobs.is_empty());
    }

    #[test]
    fn parses_legacy_completions_logprobs() {
        let v = serde_json::json!({
            "object": "text_completion",
            "choices": [{"text": " Paris", "index": 0, "logprobs": {
                "tokens": [" Paris", "."],
                "token_logprobs": [-0.1, null],
                "top_logprobs": [{" Paris": -0.1, " Lyon": -3.0}, {".": -0.2, ",": -1.9}],
                "text_offset": [0, 6]
            }}]
        });
        let s = parse_logprobs(&v).unwrap().unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].token, " Paris");
        assert_eq!(s[0].top_logprobs[1].token, " Lyon");
        // A null token logprob falls back to the top list.
        assert_eq!(s[1].logprob, -0.2);
    }

    #[test]
    fn missing_logprobs_are_none_and_bad_bodies_error() {
        for v in [
            serde_json::json!({"choices": [{"message": {"content": "x"}}]}),
            serde_json::json!({"choices": [{"message": {"content": "x"}, "logprobs": null}]}),
            serde_json::json!({"choices": [{"logprobs": {"content": null}}]}),
            serde_json::json!({"choices": [{"logprobs": {"content": []}}]}),
            serde_json::json!({"choices": [{"logprobs": {"refusal": []}}]}),
        ] {
            assert_eq!(parse_logprobs(&v).unwrap(), None, "{v}");
        }
        assert!(parse_logprobs(&serde_json::json!({"error": "x"})).is_err());
        assert!(parse_logprobs(&serde_json::json!({"choices": []})).is_err());
    }

    #[test]
    fn tolerance_math() {
        let exp = vec![step("A", -0.1, &[("A", -0.1), ("B", -2.5), ("C", -3.0)])];
        // Identical: zero deviation.
        let c = compare(&exp, &exp);
        assert_eq!((c.compared, c.mean_abs_dev, c.max_abs_dev), (3, 0.0, 0.0));
        assert!(c.first_token_match && c.diverged_at.is_none());
        assert!(c.within(&Tolerance::default()));
        // Small shifts: mean of |0.05|, |0.2|, |0.15|.
        let act = vec![step("A", -0.15, &[("A", -0.15), ("B", -2.3), ("C", -3.15)])];
        let c = compare(&exp, &act);
        assert!((c.mean_abs_dev - 0.4 / 3.0).abs() < 1e-6, "{c:?}");
        assert!((c.max_abs_dev - 0.2).abs() < 1e-9);
        assert!(c.within(&Tolerance::default()));
        // A missing alternative counts from the endpoint's floor: C (-3.0)
        // is absent, floor is -2.3, so it contributes max(0, -3.0 + 2.3) = 0.
        let act = vec![step("A", -0.1, &[("A", -0.1), ("B", -2.3)])];
        let c = compare(&exp, &act);
        assert_eq!(c.missing, 1);
        assert!((c.max_abs_dev - 0.2).abs() < 1e-9, "{c:?}");
        // A different model: the reference token is far down.
        let act = vec![step("Z", -0.05, &[("Z", -0.05), ("Y", -3.5)])];
        let c = compare(&exp, &act);
        assert!(!c.first_token_match);
        assert_eq!(c.diverged_at, Some(0));
        assert_eq!(c.missing, 3);
        // A: -0.1 - (-3.5) = 3.4
        assert!((c.max_abs_dev - 3.4).abs() < 1e-9);
        assert!(!c.within(&Tolerance::default()));
        // Spike cap alone can fail a challenge with a small mean.
        let t = Tolerance {
            max_mean_abs: 10.0,
            max_token_abs: 1.0,
            ..Tolerance::default()
        };
        assert!(!c.within(&t));
        // Comparison stops after divergence.
        let exp2 = vec![step("A", -0.1, &[]), step("B", -0.1, &[])];
        let act2 = vec![step("X", -0.1, &[("A", -0.2)]), step("B", -0.1, &[])];
        let c = compare(&exp2, &act2);
        assert_eq!((c.positions, c.diverged_at), (1, Some(0)));
        // Nothing to compare is never within tolerance.
        assert!(!compare(&exp, &[]).within(&Tolerance::default()));
    }

    #[test]
    fn overall_verdicts_and_exit_codes() {
        let r = |v| ChallengeResult {
            id: "x".into(),
            verdict: v,
            attempts: 1,
            comparison: None,
            attempt_mean_abs_dev: vec![],
            reason: None,
            signature: None,
            elapsed_ms: 0,
        };
        use Verdict::*;
        assert_eq!(overall(&[r(Pass), r(Pass)]), Pass);
        assert_eq!(overall(&[r(Pass), r(Fail), r(Unsupported)]), Fail);
        assert_eq!(overall(&[r(Pass), r(Inconclusive)]), Inconclusive);
        assert_eq!(overall(&[r(Unsupported), r(Unsupported)]), Unsupported);
        assert_eq!(overall(&[r(Pass), r(Unsupported)]), Inconclusive);
        assert_eq!(overall(&[]), Inconclusive);
        assert_eq!(
            [Pass, Fail, Inconclusive, Unsupported].map(Verdict::exit_code),
            [0, 1, 2, 2]
        );
    }

    fn cmp(mean: f64) -> Comparison {
        Comparison {
            compared: 5,
            positions: 1,
            mean_abs_dev: mean,
            max_abs_dev: mean * 2.0,
            missing: 0,
            first_token_match: true,
            diverged_at: None,
        }
    }

    fn res(v: Verdict, mean: Option<f64>, sig: Option<SigStatus>) -> ChallengeResult {
        ChallengeResult {
            id: "x".into(),
            verdict: v,
            attempts: 1,
            comparison: mean.map(cmp),
            attempt_mean_abs_dev: vec![],
            reason: None,
            signature: sig.map(|status| SigResult {
                status,
                detail: None,
            }),
            elapsed_ms: 0,
        }
    }

    #[test]
    fn set_decision_tolerates_isolated_drift_but_not_other_weights() {
        use Verdict::*;
        let t = Tolerance::default();
        // One honest challenge just outside tolerance out of eight: pass.
        let mut honest: Vec<_> = (0..7).map(|_| res(Pass, Some(0.18), None)).collect();
        honest.push(res(Fail, Some(0.62), None));
        let (v, d) = decide(&honest, &t);
        assert_eq!(v, Pass, "{}", d.reason);
        assert_eq!((d.scored, d.failed), (8, 1));
        assert!((d.fail_fraction - 0.125).abs() < 1e-9);
        assert_eq!(d.rule, DECISION_RULE);
        // The 0.3.0 rule (any fail) is max_fail_fraction 0.
        let strict = Tolerance {
            max_fail_fraction: 0.0,
            ..t
        };
        assert_eq!(decide(&honest, &strict).0, Fail);
        assert_eq!(overall(&honest), Fail);
        // Attempts that disagree are scored, not failed.
        let mut mixed: Vec<_> = (0..6).map(|_| res(Pass, Some(0.2), None)).collect();
        mixed.push(res(Inconclusive, Some(0.3), None));
        assert_eq!(decide(&mixed, &t).0, Pass);
        // Other weights: most challenges fail.
        let other: Vec<_> = (0..8)
            .map(|i| res(if i < 5 { Fail } else { Pass }, Some(0.7), None))
            .collect();
        let (v, d) = decide(&other, &t);
        assert_eq!(v, Fail);
        assert!(d.reason.contains("5 of 8"), "{}", d.reason);
        // A shifted median fails even when every challenge passes alone.
        let shifted: Vec<_> = (0..8).map(|_| res(Pass, Some(0.45), None)).collect();
        let (v, d) = decide(&shifted, &t);
        assert_eq!(v, Fail);
        assert!(d.reason.contains("median"), "{}", d.reason);
        // Any bad signature under a claim fails the run outright.
        let mut signed: Vec<_> = (0..8)
            .map(|_| res(Pass, Some(0.1), Some(SigStatus::Verified)))
            .collect();
        signed[3] = res(Fail, None, Some(SigStatus::Invalid));
        let (v, d) = decide(&signed, &t);
        assert_eq!((v, d.signature_failures), (Fail, 1));
        // Too few answers: inconclusive. No logprobs at all: unsupported.
        let errs = vec![
            res(Pass, Some(0.1), None),
            res(Inconclusive, None, None),
            res(Inconclusive, None, None),
        ];
        assert_eq!(decide(&errs, &t).0, Inconclusive);
        assert_eq!(decide(&[res(Unsupported, None, None)], &t).0, Unsupported);
        assert_eq!(decide(&[], &t).0, Inconclusive);
    }

    #[test]
    fn median_steps_drop_an_unlucky_sample() {
        // Three samples of one position; the second is an outlier.
        let s = vec![
            vec![step(
                "The",
                -0.52,
                &[("The", -0.52), ("Based", -1.40), ("To", -2.85)],
            )],
            vec![step(
                "The",
                -0.75,
                &[("The", -0.75), ("Based", -0.98), ("I", -2.90)],
            )],
            vec![step(
                "The",
                -0.50,
                &[("The", -0.50), ("Based", -1.42), ("To", -2.80)],
            )],
        ];
        let m = median_steps(&s);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].token, "The");
        assert!((m[0].logprob + 0.52).abs() < 1e-9);
        let get = |t: &str| {
            m[0].top_logprobs
                .iter()
                .find(|x| x.token == t)
                .map(|x| x.logprob)
        };
        assert!((get("Based").unwrap() + 1.40).abs() < 1e-9);
        assert!((get("To").unwrap() + 2.85).abs() < 1e-9);
        // Listed by one sample of three: below the lists, dropped.
        assert_eq!(get("I"), None);
        // Sorted most likely first.
        assert_eq!(m[0].top_logprobs[0].token, "The");
        // The chosen token is the majority choice.
        let s2 = vec![
            vec![step("A", -0.9, &[("A", -0.9), ("B", -1.0)])],
            vec![step("B", -0.8, &[("B", -0.8), ("A", -1.1)])],
            vec![step("A", -0.85, &[("A", -0.85), ("B", -1.05)])],
        ];
        assert_eq!(median_steps(&s2)[0].token, "A");
        // One sample is returned as is; none gives nothing.
        assert_eq!(median_steps(&s[..1]), s[0]);
        assert!(median_steps(&[]).is_empty());
        // Later positions only use samples that agree on earlier tokens.
        let s3 = vec![
            vec![step("A", -0.1, &[]), step("x", -0.2, &[])],
            vec![step("A", -0.1, &[]), step("x", -0.4, &[])],
            vec![step("B", -0.1, &[]), step("y", -0.1, &[])],
        ];
        let m3 = median_steps(&s3);
        assert_eq!(m3.len(), 2);
        assert_eq!(m3[1].token, "x");
        assert!((m3[1].logprob + 0.3).abs() < 1e-9);
    }

    #[test]
    fn calibration_is_sealed_and_older_files_still_load() {
        let mut r = sample_file();
        let old_hash = r.set_hash.clone();
        // A 0.3.0 file has no samples, calibration, or tolerance fields.
        let v = serde_json::to_value(&r).unwrap();
        assert!(v.get("calibration").is_none());
        assert!(v["generator"].get("samples").is_none());
        let back: ReferenceFile = serde_json::from_value(v).unwrap();
        back.validate().unwrap();
        assert_eq!(back.compute_set_hash().unwrap(), old_hash);
        assert!(r.commitment().tolerance.is_none());
        // Calibrate from observations of two runs.
        let runs = vec![
            vec![cmp(0.10), cmp(0.20), cmp(0.30)],
            vec![cmp(0.15), cmp(0.25), cmp(0.45)],
        ];
        let o = calibration_stats(&runs);
        assert_eq!(o.scored, 6);
        assert!((o.mean_abs_max - 0.45).abs() < 1e-9);
        assert!((o.token_abs_max - 0.9).abs() < 1e-9);
        assert!((o.run_median_max - 0.25).abs() < 1e-9);
        let t = calibrated_tolerance(&o, 1.5, &Tolerance::default());
        t.validate().unwrap();
        // mean max(p99 + 0.1, p90 x 1.5) = 0.675, rounded up to 0.7;
        // token 0.9 + 0.25; median max(0.25 x 1.5, 0.25 + 0.15).
        assert!((t.max_mean_abs - 0.7).abs() < 1e-9, "{t:?}");
        assert!((t.max_token_abs - 1.15).abs() < 1e-9, "{t:?}");
        assert!((t.max_median_mean_abs - 0.4).abs() < 1e-9, "{t:?}");
        assert!((t.max_fail_fraction - DEFAULT_MAX_FAIL_FRACTION).abs() < 1e-9);
        r.calibration = Some(Calibration {
            method: CALIBRATION_METHOD.into(),
            runs: 2,
            challenges: 3,
            engine_version: "test".into(),
            no_prompt_cache: false,
            margin: 1.5,
            observed: o,
            tolerance: t,
            created_at: "2026-11-14T00:00:00Z".into(),
        });
        // The calibration is covered by set_hash.
        assert!(r.validate().is_err());
        r.seal().unwrap();
        r.validate().unwrap();
        assert_ne!(r.set_hash, old_hash);
        assert_eq!(r.commitment().tolerance, Some(t));
        let mut tampered = r.clone();
        tampered
            .calibration
            .as_mut()
            .unwrap()
            .tolerance
            .max_median_mean_abs = 9.0;
        assert!(tampered.validate().is_err());
        let mut bad = r.clone();
        bad.calibration.as_mut().unwrap().method = "other".into();
        bad.seal().unwrap();
        assert!(bad.validate().is_err());
        // Reports and commitments from 0.3.0 still parse: the set-level
        // tolerances take their defaults.
        let t0: Tolerance =
            serde_json::from_str(r#"{"max_mean_abs":0.5,"max_token_abs":2.0}"#).unwrap();
        assert_eq!(t0, Tolerance::default());
    }

    #[test]
    fn sampling_is_reproducible() {
        let p = generate_prompts(&[3; 32], 20);
        let a = sample_indexes(&p, 5, &[9; 32]);
        assert_eq!(a, sample_indexes(&p, 5, &[9; 32]));
        assert_eq!(a.len(), 5);
        assert_ne!(a, sample_indexes(&p, 5, &[8; 32]));
        assert_eq!(sample_indexes(&p, 0, &[9; 32]).len(), 20);
        assert_eq!(sample_indexes(&p, 99, &[9; 32]).len(), 20);
    }

    #[test]
    fn prompt_files_and_request_bodies() {
        let p = parse_prompt_file(
            br#"[{"messages":[{"role":"user","content":"hi"}]},{"id":"c1","prompt":"The sky is"}]"#,
        )
        .unwrap();
        assert_eq!(p[0].id, "lp-0001");
        assert_eq!(p[1].id, "c1");
        let jsonl = b"{\"prompt\":\"a\"}\n\n{\"prompt\":\"b\"}\n";
        assert_eq!(parse_prompt_file(jsonl).unwrap().len(), 2);
        assert!(parse_prompt_file(br#"[{"prompt":"a","messages":[]}]"#).is_err());
        assert!(
            parse_prompt_file(br#"[{"id":"x","prompt":"a"},{"id":"x","prompt":"b"}]"#).is_err()
        );
        let s = Sampling::default();
        let (path, body) = request_body(&p[0], "org/tiny", &s, None);
        assert_eq!(path, "chat/completions");
        assert!(body.get("cache_prompt").is_none());
        let mut extra = no_prompt_cache();
        extra.insert("temperature".into(), serde_json::json!(1.0));
        let (_, b2) = request_body(&p[0], "org/tiny", &s, Some(&extra));
        assert_eq!(b2["cache_prompt"], false);
        assert_eq!(
            b2["temperature"], 0.0,
            "extra fields never override sampling"
        );
        assert_eq!(body["logprobs"], true);
        assert_eq!(body["top_logprobs"], 5);
        assert_eq!(body["temperature"], 0.0);
        let (path, body) = request_body(&p[1], "org/tiny", &s, None);
        assert_eq!(path, "completions");
        assert_eq!(body["logprobs"], 5);
        assert_eq!(body["prompt"], "The sky is");
    }

    #[test]
    fn rounding_and_engine_versions() {
        assert_eq!(round_logprob(-0.123_456_789), -0.123_457);
        assert_eq!(round_logprob(-0.000_000_1).to_bits(), 0.0f64.to_bits());
        let llama = "load_backend: loaded CPU\nversion: 1.0.0-dev (build 12366, commit 31953bcf7)\nbuilt with GNU 11.4.0";
        assert_eq!(
            engine_version_line(llama).unwrap(),
            "version: 1.0.0-dev (build 12366, commit 31953bcf7)"
        );
        assert_eq!(engine_version_line("0.6.3\n").unwrap(), "0.6.3");
        assert_eq!(engine_version_line("\n"), None);
    }

    #[test]
    fn signature_statuses() {
        let k = crate::signing::ResponseKey::from_seed(&[7; 32]);
        let claim = "1".repeat(64);
        let body = br#"{"ok":true}"#;
        let f = k.sign_response(
            &claim,
            "r1",
            "org/tiny",
            &tensorseed_core::time::now_rfc3339(),
            body,
        );
        let signed: Vec<(String, String)> = f
            .headers()
            .iter()
            .map(|(a, b)| ((*a).to_string(), (*b).to_string()))
            .collect();
        let ctx = SigContext {
            pubkey: k.pubkey(),
            claim_sha256: claim.clone(),
            served_model_id: "org/tiny".into(),
            max_skew_secs: 300,
            claim_expires_unix: None,
            claim_revoked_unix: None,
        };
        let now = i64::try_from(tensorseed_core::time::unix_now()).unwrap();
        let st = |c: Option<&SigContext>, h: &[(String, String)], b: &[u8], m: &str| {
            check_signature(c, h, b, Some("r1"), m, now).map(|r| r.status)
        };
        assert_eq!(st(None, &[], body, "org/tiny"), None);
        assert_eq!(
            st(None, &signed, body, "org/tiny"),
            Some(SigStatus::Unverifiable)
        );
        assert_eq!(
            st(Some(&ctx), &[], body, "org/tiny"),
            Some(SigStatus::Absent)
        );
        assert_eq!(
            st(Some(&ctx), &signed, body, "org/tiny"),
            Some(SigStatus::Verified)
        );
        assert_eq!(
            st(Some(&ctx), &signed, b"{}", "org/tiny"),
            Some(SigStatus::Invalid)
        );
        assert_eq!(
            st(Some(&ctx), &signed, body, "org/other"),
            Some(SigStatus::Invalid)
        );
        let mut wrong = ctx.clone();
        wrong.pubkey = crate::signing::ResponseKey::from_seed(&[8; 32]).pubkey();
        assert_eq!(
            st(Some(&wrong), &signed, body, "org/tiny"),
            Some(SigStatus::Invalid)
        );
    }

    #[test]
    fn revocations_decide_the_run() {
        use crate::claim_check::RevocationInput;
        use crate::signing::Revocations;
        use crate::signing::tests::{claim, key};

        let c = claim();
        let k = key();
        let now = i64::try_from(tensorseed_core::time::unix_now()).unwrap();
        let iso = |t: i64| {
            chrono::DateTime::from_timestamp(t, 0)
                .unwrap()
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string()
        };
        let list = |reason: &str, at: i64| {
            let mut l = Revocations::default();
            l.upsert(
                k.sign_revocation(&c.claim_sha256, &iso(at), reason)
                    .unwrap(),
            );
            RevocationInput::List {
                list: l,
                from: "test".into(),
            }
        };
        let pass = || {
            let d = SetDecision {
                rule: DECISION_RULE.into(),
                scored: 4,
                failed: 0,
                fail_fraction: 0.0,
                median_mean_abs_dev: 0.0,
                signature_failures: 0,
                reason: "ok".into(),
            };
            (Verdict::Pass, d)
        };
        // A response signed at `t`, checked at `t`.
        let signed_at = |t: i64, revoked: Option<i64>| {
            let body = br#"{"ok":true}"#;
            let f = k.sign_response(&c.claim_sha256, "r1", "org/tiny", &iso(t), body);
            let h: Vec<(String, String)> = f
                .headers()
                .iter()
                .map(|(a, b)| ((*a).to_string(), (*b).to_string()))
                .collect();
            let ctx = SigContext {
                pubkey: k.pubkey(),
                claim_sha256: c.claim_sha256.clone(),
                served_model_id: "org/tiny".into(),
                max_skew_secs: 300,
                claim_expires_unix: None,
                claim_revoked_unix: revoked,
            };
            check_signature(Some(&ctx), &h, body, Some("r1"), "org/tiny", t).unwrap()
        };

        // Revoked in the past: the run fails, and so does a fresh response.
        let (r, from) = assess_revocations(&c, &list("superseded", now - 3600), None, now);
        assert_eq!(r.status, RevocationStatus::Revoked);
        assert_eq!(r.reason.as_deref(), Some("superseded"));
        assert_eq!(r.revoked_by.as_deref(), Some("response_key"));
        assert_eq!(from, Some(now - 3600));
        let (v, mut d) = pass();
        assert_eq!(apply_revocations(v, &mut d, &r), Verdict::Fail);
        assert!(d.reason.starts_with("the claim is revoked"), "{}", d.reason);
        let s = signed_at(now, from);
        assert_eq!(s.status, SigStatus::Invalid);
        assert!(s.detail.unwrap().contains("revoked"));
        // A response signed before that time still verifies.
        assert_eq!(signed_at(now - 7200, from).status, SigStatus::Verified);

        // key_compromised voids every response, even one signed before it
        // and even when it is dated in the future.
        let (r, from) = assess_revocations(&c, &list("key_compromised", now + 3600), None, now);
        assert_eq!(r.status, RevocationStatus::Revoked);
        assert_eq!(from, Some(i64::MIN));
        assert_eq!(signed_at(now - 7200, from).status, SigStatus::Invalid);
        let (v, mut d) = pass();
        assert_eq!(apply_revocations(v, &mut d, &r), Verdict::Fail);

        // Dated in the future: a warning; responses signed now verify.
        let (r, from) = assess_revocations(&c, &list("superseded", now + 3600), None, now);
        assert_eq!(r.status, RevocationStatus::Pending);
        assert!(r.detail.contains("takes effect in"), "{}", r.detail);
        assert_eq!(signed_at(now, from).status, SigStatus::Verified);
        let (v, mut d) = pass();
        assert_eq!(apply_revocations(v, &mut d, &r), Verdict::Pass);
        assert_eq!(d.reason, "ok");

        // No list (404): fine, and reported.
        let (r, from) = assess_revocations(
            &c,
            &RevocationInput::Absent("https://llm.example.com/x".into()),
            None,
            now,
        );
        assert_eq!(r.status, RevocationStatus::NoList);
        assert_eq!(from, None);
        assert!(r.detail.contains("publishes no revocations"));
        let (v, mut d) = pass();
        assert_eq!(apply_revocations(v, &mut d, &r), Verdict::Pass);

        // Unreadable or malformed: a pass becomes inconclusive; a fail stays.
        let (r, _) = assess_revocations(
            &c,
            &RevocationInput::Unavailable("malformed ts.claim-revocations/1".into()),
            None,
            now,
        );
        assert_eq!(r.status, RevocationStatus::Unreadable);
        let (v, mut d) = pass();
        assert_eq!(apply_revocations(v, &mut d, &r), Verdict::Inconclusive);
        let (_, mut d) = pass();
        assert_eq!(apply_revocations(Verdict::Fail, &mut d, &r), Verdict::Fail);
        // The seller's list failed while another list was read: unreadable
        // unless that list revokes the claim.
        let (r, _) = assess_revocations(
            &c,
            &RevocationInput::List {
                list: Revocations::default(),
                from: "mine.json".into(),
            },
            Some("HTTP 500"),
            now,
        );
        assert_eq!(r.status, RevocationStatus::Unreadable);
        let (r, _) = assess_revocations(&c, &list("stopped", now - 60), Some("HTTP 500"), now);
        assert_eq!(r.status, RevocationStatus::Revoked);

        // An entry for another claim is ignored; a forged one for this claim
        // is counted and ignored.
        let mut other = claim();
        other.served_model_id = "org/other".into();
        other.seal().unwrap();
        let mut l = Revocations::default();
        l.upsert(
            k.sign_revocation(&other.claim_sha256, &iso(now - 60), "stopped")
                .unwrap(),
        );
        let mut forged = k
            .sign_revocation(&c.claim_sha256, &iso(now - 60), "stopped")
            .unwrap();
        forged.reason = "other".into();
        l.revocations.push(forged);
        let input = RevocationInput::List {
            list: l,
            from: "test".into(),
        };
        let (r, from) = assess_revocations(&c, &input, None, now);
        assert_eq!(r.status, RevocationStatus::NotRevoked);
        assert_eq!(r.ignored, 1);
        assert_eq!(from, None);
        let (v, mut d) = pass();
        assert_eq!(apply_revocations(v, &mut d, &r), Verdict::Pass);
        let (r, _) = assess_revocations(&c, &list("superseded", now - 60), None, now);
        assert_eq!(
            assess_revocations(&other, &list("superseded", now - 60), None, now)
                .0
                .status,
            RevocationStatus::NotRevoked,
            "a revocation names one claim"
        );
        assert_eq!(r.status, RevocationStatus::Revoked);

        // Not looked at.
        let (r, _) = assess_revocations(
            &c,
            &RevocationInput::NotChecked("--no-revocations was given".into()),
            None,
            now,
        );
        assert_eq!(r.status, RevocationStatus::NotChecked);

        // The report field is optional: a 0.3.0 style summary loads, and the
        // new one round trips.
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(j["status"], "not_checked");
        assert!(j.get("ignored").is_none());
        let back: RevocationSummary = serde_json::from_value(j).unwrap();
        assert_eq!(back, r);
    }
}
