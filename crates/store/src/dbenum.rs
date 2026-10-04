//! The slug a `#[serde(rename_all = "snake_case")]` fieldless enum serializes to — also exactly
//! the label its matching Postgres enum type uses, since every migration enum was written to
//! match its Rust counterpart's serde representation.

use serde::Serialize;

pub(crate) fn enum_slug<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v).expect("enum always serializes") {
        serde_json::Value::String(s) => s,
        other => panic!("expected a fieldless enum to serialize to a JSON string, got {other}"),
    }
}
