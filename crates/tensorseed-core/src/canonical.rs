// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Canonical JSON (spec section 6.4): UTF-8, object keys sorted, no
//! insignificant whitespace. The same content always yields the same bytes,
//! and therefore the same catalog leaf hash.

use serde::Serialize;

use crate::Result;

/// Serialize `value` to canonical JSON bytes.
///
/// Values are first converted to `serde_json::Value`, whose object map is a
/// `BTreeMap` (the `preserve_order` feature is not enabled), so keys come out
/// sorted at every nesting level.
///
/// # Errors
///
/// [`Error::Json`](crate::Error::Json) if `value` cannot be represented as JSON
/// (for example a map with non string keys).
pub fn to_canonical_vec<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>> {
    let v = serde_json::to_value(value)?;
    Ok(serde_json::to_vec(&v)?)
}

/// Serialize `value` to a canonical JSON string.
///
/// # Errors
///
/// Same as [`to_canonical_vec`].
pub fn to_canonical_string<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    let v = serde_json::to_value(value)?;
    Ok(serde_json::to_string(&v)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_sorted_and_compact() {
        let v = json!({"b": 1, "a": {"z": true, "y": [3, 2]}});
        assert_eq!(
            to_canonical_string(&v).unwrap(),
            r#"{"a":{"y":[3,2],"z":true},"b":1}"#
        );
    }

    #[derive(Serialize)]
    struct Unsorted {
        zeta: u8,
        alpha: u8,
    }

    #[test]
    fn struct_fields_sorted() {
        let s = to_canonical_string(&Unsorted { zeta: 1, alpha: 2 }).unwrap();
        assert_eq!(s, r#"{"alpha":2,"zeta":1}"#);
    }
}
