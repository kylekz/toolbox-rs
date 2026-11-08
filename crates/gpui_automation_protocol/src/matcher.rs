//! JSON-pointer expectations over a `state` document.
//!
//! One matcher backs the in-app `wait_for` / `assert` commands and the e2e
//! runner's `"expect"` lines, so a scenario asserts the same way whichever
//! side evaluates it.
//!
//! An expectation is an object with a `pointer` (RFC 6901, `""` is the whole
//! document) and one or more checks, all of which must hold:
//!
//! | key          | holds when                                                        |
//! |--------------|-------------------------------------------------------------------|
//! | `equals`     | the value is deep-equal (numbers compare with a 1e-6 tolerance)   |
//! | `not_equals` | the value is missing or not deep-equal                            |
//! | `contains`   | substring of a string, element of an array, or subset of an object|
//! | `exists`     | `true`: the pointer resolves (a `null` counts); `false`: it doesn't|
//! | `gte`/`lte`  | the value is a number `>=` / `<=` the operand                     |
//! | `len`        | array length, object key count or string length in chars          |

use std::fmt;

use serde_json::{Map, Value};

/// A parsed expectation: where to look and what must hold there.
#[derive(Debug, Clone, PartialEq)]
pub struct Expectation {
    pub pointer: String,
    pub checks: Vec<Check>,
}

/// A single condition on the value at an expectation's pointer.
#[derive(Debug, Clone, PartialEq)]
pub enum Check {
    Equals(Value),
    NotEquals(Value),
    Contains(Value),
    Exists(bool),
    Gte(f64),
    Lte(f64),
    Len(usize),
}

/// Why an expectation did not hold.
#[derive(Debug, Clone, PartialEq)]
pub struct Mismatch {
    pub pointer: String,
    /// Human-readable description of the failed check, e.g. `equals 2`.
    pub check: String,
    /// The value found at the pointer, or `None` when it didn't resolve.
    pub actual: Option<Value>,
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let pointer = if self.pointer.is_empty() {
            "(root)"
        } else {
            &self.pointer
        };
        match &self.actual {
            Some(actual) => write!(
                f,
                "{pointer}: expected {}, got {}",
                self.check,
                compact(actual)
            ),
            None => write!(f, "{pointer}: expected {}, but it is missing", self.check),
        }
    }
}

impl std::error::Error for Mismatch {}

/// Keys an expectation object may carry besides `pointer`.
const CHECK_KEYS: &[&str] = &[
    "equals",
    "not_equals",
    "contains",
    "exists",
    "gte",
    "lte",
    "len",
];

impl Expectation {
    /// Parse an expectation object, rejecting unknown keys so a typo such as
    /// `"equal"` fails loudly instead of silently asserting nothing.
    pub fn parse(value: &Value) -> Result<Self, String> {
        let map = value
            .as_object()
            .ok_or_else(|| format!("an expectation must be an object, got {}", compact(value)))?;
        Self::parse_map(map, &[])
    }

    /// Parse from an object that also carries other fields (such as a
    /// command's `cmd` and `timeout_ms`); keys in `extra` are skipped.
    pub fn parse_map(map: &Map<String, Value>, extra: &[&str]) -> Result<Self, String> {
        let pointer = match map.get("pointer") {
            Some(Value::String(pointer)) => pointer.clone(),
            Some(other) => return Err(format!("pointer must be a string, got {}", compact(other))),
            None => return Err("expectation is missing \"pointer\"".into()),
        };
        if !pointer.is_empty() && !pointer.starts_with('/') {
            return Err(format!(
                "pointer {pointer:?} must be empty or start with '/' (RFC 6901)"
            ));
        }

        let mut checks = Vec::new();
        for (key, operand) in map {
            if key == "pointer" || extra.contains(&key.as_str()) {
                continue;
            }
            let check = match key.as_str() {
                "equals" => Check::Equals(operand.clone()),
                "not_equals" => Check::NotEquals(operand.clone()),
                "contains" => Check::Contains(operand.clone()),
                "exists" => Check::Exists(operand.as_bool().ok_or_else(|| {
                    format!("exists must be a boolean, got {}", compact(operand))
                })?),
                "gte" => Check::Gte(number_operand(key, operand)?),
                "lte" => Check::Lte(number_operand(key, operand)?),
                "len" => Check::Len(
                    operand
                        .as_u64()
                        .and_then(|len| usize::try_from(len).ok())
                        .ok_or_else(|| {
                            format!(
                                "len must be a non-negative integer, got {}",
                                compact(operand)
                            )
                        })?,
                ),
                other => {
                    return Err(format!(
                        "unknown expectation key {other:?} (expected one of pointer, {})",
                        CHECK_KEYS.join(", ")
                    ));
                }
            };
            checks.push(check);
        }
        if checks.is_empty() {
            return Err(format!(
                "expectation on {pointer:?} has no check (use one of {})",
                CHECK_KEYS.join(", ")
            ));
        }
        Ok(Self { pointer, checks })
    }

    /// Parse either a single expectation object or an array of them.
    pub fn parse_list(value: &Value) -> Result<Vec<Self>, String> {
        match value {
            Value::Array(items) if items.is_empty() => Err("empty expectation list".into()),
            Value::Array(items) => items.iter().map(Self::parse).collect(),
            other => Ok(vec![Self::parse(other)?]),
        }
    }

    /// Check every condition against `state`, reporting the first that fails.
    pub fn evaluate(&self, state: &Value) -> Result<(), Mismatch> {
        let actual = state.pointer(&self.pointer);
        for check in &self.checks {
            if !check.holds(actual) {
                return Err(Mismatch {
                    pointer: self.pointer.clone(),
                    check: check.to_string(),
                    actual: actual.cloned(),
                });
            }
        }
        Ok(())
    }
}

impl Check {
    fn holds(&self, actual: Option<&Value>) -> bool {
        match self {
            Check::Exists(expected) => actual.is_some() == *expected,
            Check::NotEquals(expected) => {
                actual.is_none_or(|actual| !values_equal(actual, expected))
            }
            Check::Equals(expected) => actual.is_some_and(|actual| values_equal(actual, expected)),
            Check::Contains(expected) => actual.is_some_and(|actual| contains(actual, expected)),
            Check::Gte(bound) => actual.and_then(Value::as_f64).is_some_and(|n| n >= *bound),
            Check::Lte(bound) => actual.and_then(Value::as_f64).is_some_and(|n| n <= *bound),
            Check::Len(expected) => actual.and_then(length).is_some_and(|len| len == *expected),
        }
    }
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Check::Equals(value) => write!(f, "equals {}", compact(value)),
            Check::NotEquals(value) => write!(f, "not_equals {}", compact(value)),
            Check::Contains(value) => write!(f, "contains {}", compact(value)),
            Check::Exists(true) => f.write_str("to exist"),
            Check::Exists(false) => f.write_str("not to exist"),
            Check::Gte(bound) => write!(f, ">= {bound}"),
            Check::Lte(bound) => write!(f, "<= {bound}"),
            Check::Len(len) => write!(f, "len {len}"),
        }
    }
}

fn number_operand(key: &str, operand: &Value) -> Result<f64, String> {
    operand
        .as_f64()
        .ok_or_else(|| format!("{key} must be a number, got {}", compact(operand)))
}

fn length(value: &Value) -> Option<usize> {
    match value {
        Value::Array(items) => Some(items.len()),
        Value::Object(map) => Some(map.len()),
        Value::String(text) => Some(text.chars().count()),
        _ => None,
    }
}

/// Deep equality where numbers compare by value: `1`, `1.0` and a float
/// that went through `f32` (`0.10000000149`) all equal their intended value.
pub fn values_equal(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Number(a), Value::Number(b)) => {
            if let (Some(a), Some(b)) = (a.as_i64(), b.as_i64()) {
                return a == b;
            }
            if let (Some(a), Some(b)) = (a.as_u64(), b.as_u64()) {
                return a == b;
            }
            match (a.as_f64(), b.as_f64()) {
                (Some(a), Some(b)) => approx_eq(a, b),
                _ => false,
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| values_equal(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, a)| b.get(key).is_some_and(|b| values_equal(a, b)))
        }
        _ => actual == expected,
    }
}

fn approx_eq(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6 * a.abs().max(b.abs()).max(1.0)
}

/// `contains`: substring for strings, "some element matches" for arrays and
/// subset for objects. Array elements and object members match by subset, so
/// `{"contains": {"name": "a.png"}}` finds `{"name": "a.png", "size": 3}`.
fn contains(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::String(haystack), Value::String(needle)) => haystack.contains(needle.as_str()),
        (Value::Array(items), _) => items.iter().any(|item| subset(item, expected)),
        (Value::Object(_), Value::Object(_)) => subset(actual, expected),
        _ => false,
    }
}

fn subset(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Object(actual), Value::Object(expected)) => {
            expected.iter().all(|(key, expected)| {
                actual
                    .get(key)
                    .is_some_and(|actual| subset(actual, expected))
            })
        }
        _ => values_equal(actual, expected),
    }
}

/// Single-line JSON for messages, truncated so a huge state can't flood a
/// terminal.
pub fn compact(value: &Value) -> String {
    const LIMIT: usize = 300;
    let text = value.to_string();
    if text.chars().count() <= LIMIT {
        text
    } else {
        let head: String = text.chars().take(LIMIT).collect();
        format!("{head}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn expect(value: Value) -> Expectation {
        Expectation::parse(&value).unwrap()
    }

    fn state() -> Value {
        json!({
            "counter": 3,
            "ratio": 0.10000000149011612,
            "title": "The Salt Road",
            "empty": null,
            "files": [
                {"name": "a.png", "size": 10},
                {"name": "b.jpg", "size": 20}
            ],
            "tags": ["x", "y"],
            "window": {"w": 1440.0, "h": 900.0, "fullscreen": false},
        })
    }

    #[test]
    fn equals_compares_numbers_by_value() {
        assert!(
            expect(json!({"pointer": "/counter", "equals": 3}))
                .evaluate(&state())
                .is_ok()
        );
        assert!(
            expect(json!({"pointer": "/counter", "equals": 3.0}))
                .evaluate(&state())
                .is_ok()
        );
        assert!(
            expect(json!({"pointer": "/window/w", "equals": 1440}))
                .evaluate(&state())
                .is_ok()
        );
        assert!(
            expect(json!({"pointer": "/ratio", "equals": 0.1}))
                .evaluate(&state())
                .is_ok()
        );
        assert!(
            expect(json!({"pointer": "/counter", "equals": 4}))
                .evaluate(&state())
                .is_err()
        );
    }

    #[test]
    fn equals_is_deep() {
        let ok = expect(json!({"pointer": "/tags", "equals": ["x", "y"]}));
        assert!(ok.evaluate(&state()).is_ok());
        let wrong_order = expect(json!({"pointer": "/tags", "equals": ["y", "x"]}));
        assert!(wrong_order.evaluate(&state()).is_err());
        let object = expect(json!({
            "pointer": "/window",
            "equals": {"w": 1440, "h": 900, "fullscreen": false}
        }));
        assert!(object.evaluate(&state()).is_ok());
        let partial = expect(json!({"pointer": "/window", "equals": {"w": 1440}}));
        assert!(partial.evaluate(&state()).is_err());
    }

    #[test]
    fn equals_null_is_distinct_from_missing() {
        assert!(
            expect(json!({"pointer": "/empty", "equals": null}))
                .evaluate(&state())
                .is_ok()
        );
        let missing = expect(json!({"pointer": "/nope", "equals": null}))
            .evaluate(&state())
            .unwrap_err();
        assert_eq!(missing.actual, None);
        assert_eq!(
            missing.to_string(),
            "/nope: expected equals null, but it is missing"
        );
    }

    #[test]
    fn not_equals_passes_for_missing_values() {
        assert!(
            expect(json!({"pointer": "/counter", "not_equals": 4}))
                .evaluate(&state())
                .is_ok()
        );
        assert!(
            expect(json!({"pointer": "/nope", "not_equals": 4}))
                .evaluate(&state())
                .is_ok()
        );
        assert!(
            expect(json!({"pointer": "/counter", "not_equals": 3}))
                .evaluate(&state())
                .is_err()
        );
    }

    #[test]
    fn contains_handles_strings_arrays_and_objects() {
        let s = state();
        assert!(
            expect(json!({"pointer": "/title", "contains": "Salt"}))
                .evaluate(&s)
                .is_ok()
        );
        assert!(
            expect(json!({"pointer": "/title", "contains": "salt"}))
                .evaluate(&s)
                .is_err()
        );
        assert!(
            expect(json!({"pointer": "/tags", "contains": "y"}))
                .evaluate(&s)
                .is_ok()
        );
        assert!(
            expect(json!({"pointer": "/tags", "contains": "z"}))
                .evaluate(&s)
                .is_err()
        );
        assert!(
            expect(json!({"pointer": "/files", "contains": {"name": "b.jpg"}}))
                .evaluate(&s)
                .is_ok()
        );
        assert!(
            expect(json!({"pointer": "/files", "contains": {"name": "b.jpg", "size": 10}}))
                .evaluate(&s)
                .is_err()
        );
        assert!(
            expect(json!({"pointer": "/window", "contains": {"fullscreen": false}}))
                .evaluate(&s)
                .is_ok()
        );
        assert!(
            expect(json!({"pointer": "/counter", "contains": 3}))
                .evaluate(&s)
                .is_err()
        );
    }

    #[test]
    fn exists_distinguishes_null_from_missing() {
        let s = state();
        assert!(
            expect(json!({"pointer": "/empty", "exists": true}))
                .evaluate(&s)
                .is_ok()
        );
        assert!(
            expect(json!({"pointer": "/nope", "exists": false}))
                .evaluate(&s)
                .is_ok()
        );
        assert!(
            expect(json!({"pointer": "/files/1/name", "exists": true}))
                .evaluate(&s)
                .is_ok()
        );
        assert!(
            expect(json!({"pointer": "/files/2", "exists": true}))
                .evaluate(&s)
                .is_err()
        );
        assert!(
            expect(json!({"pointer": "/counter", "exists": false}))
                .evaluate(&s)
                .is_err()
        );
    }

    #[test]
    fn range_checks_combine() {
        let s = state();
        assert!(
            expect(json!({"pointer": "/counter", "gte": 3, "lte": 3}))
                .evaluate(&s)
                .is_ok()
        );
        let err = expect(json!({"pointer": "/counter", "gte": 1, "lte": 2}))
            .evaluate(&s)
            .unwrap_err();
        assert_eq!(err.to_string(), "/counter: expected <= 2, got 3");
        assert!(
            expect(json!({"pointer": "/title", "gte": 0}))
                .evaluate(&s)
                .is_err()
        );
    }

    #[test]
    fn len_counts_arrays_objects_and_chars() {
        let s = state();
        assert!(
            expect(json!({"pointer": "/files", "len": 2}))
                .evaluate(&s)
                .is_ok()
        );
        assert!(
            expect(json!({"pointer": "/window", "len": 3}))
                .evaluate(&s)
                .is_ok()
        );
        assert!(
            expect(json!({"pointer": "/title", "len": 13}))
                .evaluate(&s)
                .is_ok()
        );
        assert!(
            expect(json!({"pointer": "/counter", "len": 1}))
                .evaluate(&s)
                .is_err()
        );
    }

    #[test]
    fn root_pointer_addresses_the_whole_document() {
        let e = expect(json!({"pointer": "", "contains": {"counter": 3}}));
        assert!(e.evaluate(&state()).is_ok());
        let err = expect(json!({"pointer": "", "len": 1}))
            .evaluate(&json!([]))
            .unwrap_err();
        assert_eq!(err.to_string(), "(root): expected len 1, got []");
    }

    #[test]
    fn parse_rejects_malformed_expectations() {
        let cases = [
            (json!(3), "must be an object"),
            (json!({"equals": 1}), "missing \"pointer\""),
            (
                json!({"pointer": 1, "equals": 1}),
                "pointer must be a string",
            ),
            (
                json!({"pointer": "counter", "equals": 1}),
                "must be empty or start with '/'",
            ),
            (json!({"pointer": "/a"}), "has no check"),
            (
                json!({"pointer": "/a", "equal": 1}),
                "unknown expectation key \"equal\"",
            ),
            (
                json!({"pointer": "/a", "exists": "yes"}),
                "exists must be a boolean",
            ),
            (json!({"pointer": "/a", "gte": "1"}), "gte must be a number"),
            (
                json!({"pointer": "/a", "len": -1}),
                "len must be a non-negative integer",
            ),
        ];
        for (value, message) in cases {
            let err = Expectation::parse(&value).unwrap_err();
            assert!(err.contains(message), "{value}: {err}");
        }
    }

    #[test]
    fn parse_map_skips_extra_keys() {
        let value = json!({"cmd": "wait_for", "pointer": "/a", "equals": 1, "timeout_ms": 5});
        let map = value.as_object().unwrap();
        let parsed = Expectation::parse_map(map, &["cmd", "timeout_ms"]).unwrap();
        assert_eq!(parsed.pointer, "/a");
        assert_eq!(parsed.checks, vec![Check::Equals(json!(1))]);
        assert!(Expectation::parse_map(map, &["cmd"]).is_err());
    }

    #[test]
    fn parse_list_accepts_object_or_array() {
        assert_eq!(
            Expectation::parse_list(&json!({"pointer": "/a", "exists": true}))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            Expectation::parse_list(&json!([
                {"pointer": "/a", "exists": true},
                {"pointer": "/b", "equals": 2}
            ]))
            .unwrap()
            .len(),
            2
        );
        assert!(Expectation::parse_list(&json!([])).is_err());
        assert!(Expectation::parse_list(&json!([{"pointer": "/a"}])).is_err());
    }

    #[test]
    fn mismatch_messages_are_truncated() {
        let big = Value::String("x".repeat(1000));
        let text = compact(&big);
        assert!(text.len() < 400);
        assert!(text.ends_with("..."));
    }
}
