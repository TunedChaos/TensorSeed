// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Minimal bencode (BEP 3) encoder and strict decoder.
//!
//! Dictionaries are `BTreeMap<Vec<u8>, Value>`, so encoding always emits keys
//! in raw byte order as the spec requires. The decoder rejects trailing data,
//! leading zeros, negative zero, unsorted or duplicate keys, and nesting deeper
//! than [`MAX_DEPTH`], and it can report the exact byte span of a top level
//! key's value (the `info` dictionary, whose bytes are hashed for info hashes).

use std::collections::BTreeMap;

/// Deepest nesting the decoder accepts.
pub const MAX_DEPTH: usize = 64;

/// A bencoded value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// Integer.
    Int(i64),
    /// Byte string.
    Bytes(Vec<u8>),
    /// List.
    List(Vec<Value>),
    /// Dictionary (keys sorted by raw bytes).
    Dict(BTreeMap<Vec<u8>, Value>),
}

/// Decoding error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("bencode: {msg} at byte {pos}")]
pub struct DecodeError {
    /// Byte offset.
    pub pos: usize,
    /// What went wrong.
    pub msg: &'static str,
}

impl Value {
    /// Byte string value.
    pub fn bytes(b: impl Into<Vec<u8>>) -> Self {
        Self::Bytes(b.into())
    }

    /// Dictionary from `(key, value)` pairs.
    pub fn dict<K: Into<Vec<u8>>>(entries: impl IntoIterator<Item = (K, Value)>) -> Self {
        Self::Dict(entries.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    /// Encode to bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(&mut out);
        out
    }

    /// Append the encoding to `out`.
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        match self {
            Self::Int(i) => {
                out.push(b'i');
                out.extend_from_slice(i.to_string().as_bytes());
                out.push(b'e');
            }
            Self::Bytes(b) => encode_bytes(b, out),
            Self::List(l) => {
                out.push(b'l');
                for v in l {
                    v.encode_into(out);
                }
                out.push(b'e');
            }
            Self::Dict(d) => {
                out.push(b'd');
                for (k, v) in d {
                    encode_bytes(k, out);
                    v.encode_into(out);
                }
                out.push(b'e');
            }
        }
    }

    /// Integer, if this is one.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Byte string, if this is one.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(b) => Some(b),
            _ => None,
        }
    }

    /// UTF-8 string, if this is a valid UTF-8 byte string.
    pub fn as_str(&self) -> Option<&str> {
        self.as_bytes().and_then(|b| std::str::from_utf8(b).ok())
    }

    /// List, if this is one.
    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Self::List(l) => Some(l),
            _ => None,
        }
    }

    /// Dictionary, if this is one.
    pub fn as_dict(&self) -> Option<&BTreeMap<Vec<u8>, Value>> {
        match self {
            Self::Dict(d) => Some(d),
            _ => None,
        }
    }

    /// Dictionary lookup by string key.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_dict().and_then(|d| d.get(key.as_bytes()))
    }
}

fn encode_bytes(b: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(b.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(b);
}

/// Decode exactly one value spanning all of `data`.
///
/// # Errors
///
/// [`DecodeError`] for malformed input or trailing bytes.
///
/// # Examples
///
/// ```
/// use tensorseed_torrent::bencode::{decode, Value};
///
/// let v = decode(b"d3:fooi42ee")?;
/// assert_eq!(v.get("foo").and_then(Value::as_int), Some(42));
/// assert_eq!(v.encode(), b"d3:fooi42ee");
/// // Non canonical integers and trailing bytes are rejected.
/// assert!(decode(b"i042e").is_err());
/// assert!(decode(b"i1ex").is_err());
/// # Ok::<(), tensorseed_torrent::bencode::DecodeError>(())
/// ```
pub fn decode(data: &[u8]) -> Result<Value, DecodeError> {
    let mut p = Parser { data, pos: 0 };
    let v = p.value(0)?;
    if p.pos != data.len() {
        return Err(p.err("trailing data"));
    }
    Ok(v)
}

/// Decode a top level dictionary and return the raw byte span of `key`'s
/// value along with the decoded value.
///
/// # Errors
///
/// [`DecodeError`] for malformed input or when the top level value is not a
/// dictionary.
pub fn decode_with_span<'a>(
    data: &'a [u8],
    key: &str,
) -> Result<(Value, Option<&'a [u8]>), DecodeError> {
    let mut p = Parser { data, pos: 0 };
    if data.first() != Some(&b'd') {
        return Err(p.err("expected a dictionary"));
    }
    p.pos = 1;
    let mut map = BTreeMap::new();
    let mut span = None;
    let mut last: Option<Vec<u8>> = None;
    loop {
        match p.peek()? {
            b'e' => {
                p.pos += 1;
                break;
            }
            _ => {
                let k = p.string()?;
                if last.as_ref().is_some_and(|l| *l >= k) {
                    return Err(p.err("dictionary keys not sorted or duplicated"));
                }
                let start = p.pos;
                let v = p.value(1)?;
                if k == key.as_bytes() {
                    span = Some(&data[start..p.pos]);
                }
                last = Some(k.clone());
                map.insert(k, v);
            }
        }
    }
    if p.pos != data.len() {
        return Err(p.err("trailing data"));
    }
    Ok((Value::Dict(map), span))
}

struct Parser<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn err(&self, msg: &'static str) -> DecodeError {
        DecodeError { pos: self.pos, msg }
    }

    fn peek(&self) -> Result<u8, DecodeError> {
        self.data
            .get(self.pos)
            .copied()
            .ok_or_else(|| self.err("unexpected end"))
    }

    /// Decode one value at the cursor. Integers must be canonical (no leading
    /// zeros, no `-0`) and nesting is capped at [`MAX_DEPTH`].
    fn value(&mut self, depth: usize) -> Result<Value, DecodeError> {
        if depth > MAX_DEPTH {
            return Err(self.err("nested too deeply"));
        }
        match self.peek()? {
            b'i' => {
                self.pos += 1;
                let end = self.find(b'e')?;
                let s = &self.data[self.pos..end];
                let ok = !s.is_empty()
                    && s != b"-"
                    && s != b"-0"
                    && !(s.len() > 1 && s[0] == b'0')
                    && !(s.len() > 2 && s[0] == b'-' && s[1] == b'0');
                let n = std::str::from_utf8(s)
                    .ok()
                    .filter(|_| ok)
                    .and_then(|s| s.parse::<i64>().ok())
                    .ok_or_else(|| self.err("bad integer"))?;
                self.pos = end + 1;
                Ok(Value::Int(n))
            }
            b'l' => {
                self.pos += 1;
                let mut l = Vec::new();
                while self.peek()? != b'e' {
                    l.push(self.value(depth + 1)?);
                }
                self.pos += 1;
                Ok(Value::List(l))
            }
            b'd' => {
                self.pos += 1;
                let mut d = BTreeMap::new();
                let mut last: Option<Vec<u8>> = None;
                while self.peek()? != b'e' {
                    let k = self.string()?;
                    if last.as_ref().is_some_and(|l| *l >= k) {
                        return Err(self.err("dictionary keys not sorted or duplicated"));
                    }
                    let v = self.value(depth + 1)?;
                    last = Some(k.clone());
                    d.insert(k, v);
                }
                self.pos += 1;
                Ok(Value::Dict(d))
            }
            b'0'..=b'9' => Ok(Value::Bytes(self.string()?)),
            _ => Err(self.err("unexpected byte")),
        }
    }

    fn find(&self, b: u8) -> Result<usize, DecodeError> {
        self.data[self.pos..]
            .iter()
            .position(|&c| c == b)
            .map(|i| self.pos + i)
            .ok_or_else(|| self.err("unterminated"))
    }

    fn string(&mut self) -> Result<Vec<u8>, DecodeError> {
        let colon = self.find(b':')?;
        let s = &self.data[self.pos..colon];
        if s.is_empty() || (s.len() > 1 && s[0] == b'0') || !s.iter().all(u8::is_ascii_digit) {
            return Err(self.err("bad string length"));
        }
        let n: usize = std::str::from_utf8(s)
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| self.err("bad string length"))?;
        let start = colon + 1;
        let end = start
            .checked_add(n)
            .filter(|&e| e <= self.data.len())
            .ok_or_else(|| self.err("string past end"))?;
        self.pos = end;
        Ok(self.data[start..end].to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let v = Value::dict([
            ("b", Value::List(vec![Value::Int(-3), Value::bytes("x")])),
            ("a", Value::Int(0)),
            ("c", Value::dict([("z", Value::bytes(vec![0u8, 255]))])),
        ]);
        let e = v.encode();
        assert_eq!(e, b"d1:ai0e1:bli-3e1:xe1:cd1:z2:\x00\xffee".to_vec());
        assert_eq!(decode(&e).unwrap(), v);
    }

    #[test]
    fn rejects_malformed() {
        for bad in [
            &b"i01e"[..],
            b"i-0e",
            b"ie",
            b"i-e",
            b"02:ab",
            b"3:ab",
            b"d1:bi0e1:ai0ee",
            b"d1:ai0e1:ai0ee",
            b"i1ei2e",
            b"l",
            b"x",
        ] {
            assert!(decode(bad).is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
        let deep = format!("{}{}", "l".repeat(100), "e".repeat(100));
        assert!(decode(deep.as_bytes()).is_err());
    }

    #[test]
    fn span_of_info() {
        let data = b"d8:announce3:url4:infod4:name1:xe5:otheri1ee";
        let (v, span) = decode_with_span(data, "info").unwrap();
        assert_eq!(span.unwrap(), b"d4:name1:xe");
        assert_eq!(v.get("other").unwrap().as_int(), Some(1));
    }
}
