//! Comparison semantics of stored values.
//!
//! Rows are JSON objects. Two orders exist:
//!
//! - **SQL comparison** ([`compare`]), used by filters: only values of the same
//!   kind compare (numbers with numbers, strings with strings, booleans with
//!   booleans). `NULL`, a missing field, or values of different kinds are not
//!   comparable, so `eq`, `gt`, ... are false for them (three-valued logic;
//!   `is_null` is the way to match them).
//! - **Total order** ([`total_cmp`]), used by `order` and by index keys:
//!   `null < false < true < numbers < strings < arrays < objects`. Numbers
//!   compare by value, strings byte-wise (binary collation).

use std::cmp::Ordering;

use serde_json::Value;

/// Returns the value at the dot-separated `path` of `row` (`"address.city"`),
/// or `None` when a segment is missing.
pub fn field<'a>(row: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .try_fold(row, |value, segment| value.get(segment))
}

/// Rank of a value in the total order; also the tag of its key encoding.
pub(crate) fn kind_rank(value: &Value) -> u8 {
    match value {
        Value::Null => 1,
        Value::Bool(false) => 2,
        Value::Bool(true) => 3,
        Value::Number(_) => 4,
        Value::String(_) => 5,
        Value::Array(_) => 6,
        Value::Object(_) => 7,
    }
}

/// Compares two JSON numbers exactly when both are integers, by `f64`
/// otherwise.
pub(crate) fn number_cmp(a: &serde_json::Number, b: &serde_json::Number) -> Ordering {
    if let (Some(x), Some(y)) = (a.as_i64(), b.as_i64()) {
        return x.cmp(&y);
    }
    if let (Some(x), Some(y)) = (a.as_u64(), b.as_u64()) {
        return x.cmp(&y);
    }
    let x = a.as_f64().unwrap_or(0.0);
    let y = b.as_f64().unwrap_or(0.0);
    x.total_cmp(&y)
}

/// SQL comparison: `None` when the values are not comparable.
pub fn compare(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => Some(number_cmp(x, y)),
        (Value::String(x), Value::String(y)) => Some(x.as_bytes().cmp(y.as_bytes())),
        (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
        (Value::Array(_), Value::Array(_)) | (Value::Object(_), Value::Object(_)) => {
            (a == b).then_some(Ordering::Equal)
        }
        _ => None,
    }
}

/// SQL equality: numbers compare by value (`1 == 1.0`), `NULL` equals nothing.
pub fn sql_eq(a: &Value, b: &Value) -> bool {
    compare(a, b) == Some(Ordering::Equal)
}

/// Total order of values, consistent with their index key encoding.
pub fn total_cmp(a: &Value, b: &Value) -> Ordering {
    let rank = kind_rank(a).cmp(&kind_rank(b));
    if rank != Ordering::Equal {
        return rank;
    }
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => number_cmp(x, y),
        (Value::String(x), Value::String(y)) => x.as_bytes().cmp(y.as_bytes()),
        (Value::Array(_), Value::Array(_)) | (Value::Object(_), Value::Object(_)) => {
            canonical(a).cmp(&canonical(b))
        }
        _ => Ordering::Equal,
    }
}

/// Canonical bytes of a value (object keys are sorted by `serde_json`).
pub(crate) fn canonical(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_default()
}

/// SQL `LIKE`: `%` matches any sequence, `_` exactly one character. With
/// `case_insensitive`, ASCII letters match regardless of case (`ILIKE`).
pub fn like(text: &str, pattern: &str, case_insensitive: bool) -> bool {
    let normalize = |c: char| {
        if case_insensitive {
            c.to_ascii_lowercase()
        } else {
            c
        }
    };
    let text: Vec<char> = text.chars().map(normalize).collect();
    let pattern: Vec<char> = pattern.chars().map(normalize).collect();
    // Iterative wildcard matching with backtracking to the last `%`.
    let (mut t, mut p) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some('%') => {
                star = Some((p, t));
                p += 1;
            }
            Some('_') => {
                t += 1;
                p += 1;
            }
            Some(c) if *c == text[t] => {
                t += 1;
                p += 1;
            }
            _ => match star {
                Some((star_p, star_t)) => {
                    p = star_p + 1;
                    t = star_t + 1;
                    star = Some((star_p, star_t + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|c| *c == '%')
}
