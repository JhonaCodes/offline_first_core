//! Order-preserving key encoding.
//!
//! Primary keys and index keys are stored as LMDB keys, which LMDB sorts
//! byte-wise. The encoding makes that byte order follow the total order of
//! [`value::total_cmp`](super::value::total_cmp), so range scans and ordered
//! scans over an index return rows in value order:
//!
//! | Value | Encoding |
//! |---|---|
//! | `null` | `0x01` |
//! | `false` / `true` | `0x02` / `0x03` |
//! | number | `0x04` + the `f64` value in 8 order-preserving bytes |
//! | string | `0x05` + UTF-8 bytes, `0x00` escaped as `0x00 0xFF`, then `0x00 0x00` |
//! | array / object | `0x06` / `0x07` + canonical JSON escaped like a string |
//!
//! Every encoded value is self-delimiting, so a composite key is the plain
//! concatenation of its values and still sorts as a tuple.
//!
//! Numbers are encoded through `f64`: integers beyond ±2^53 that share the
//! same `f64` share the same key prefix. Lookups stay exact because every
//! candidate row is re-checked against the filter; only the relative order of
//! such integers in an index-ordered scan is approximate.

use serde_json::Value;

use super::value::{canonical, kind_rank};

/// Appends the encoding of `value` to `out`.
pub fn encode_value(value: &Value, out: &mut Vec<u8>) {
    out.push(kind_rank(value));
    match value {
        Value::Null | Value::Bool(_) => {}
        Value::Number(number) => {
            let float = number.as_f64().unwrap_or(0.0);
            // -0.0 and 0.0 are the same key.
            let float = if float == 0.0 { 0.0 } else { float };
            let bits = float.to_bits();
            let ordered = if bits >> 63 == 1 {
                !bits
            } else {
                bits | (1 << 63)
            };
            out.extend_from_slice(&ordered.to_be_bytes());
        }
        Value::String(text) => escape_into(text.as_bytes(), out),
        Value::Array(_) | Value::Object(_) => escape_into(&canonical(value), out),
    }
}

fn escape_into(bytes: &[u8], out: &mut Vec<u8>) {
    for &byte in bytes {
        out.push(byte);
        if byte == 0 {
            out.push(0xFF);
        }
    }
    out.extend_from_slice(&[0, 0]);
}

/// Encodes a tuple of values as one key.
pub fn encode_values<'a>(values: impl IntoIterator<Item = &'a Value>) -> Vec<u8> {
    let mut out = Vec::new();
    for value in values {
        encode_value(value, &mut out);
    }
    out
}

/// Encodes a single value as a key.
pub fn encode_key(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    encode_value(value, &mut out);
    out
}

/// The smallest key greater than every key starting with `prefix`, or `None`
/// when no such key exists (the prefix is empty or all `0xFF`).
pub fn prefix_successor(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut successor = prefix.to_vec();
    while let Some(last) = successor.pop() {
        if last < 0xFF {
            successor.push(last + 1);
            return Some(successor);
        }
    }
    None
}
