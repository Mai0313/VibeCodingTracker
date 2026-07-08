//! Renderers for `vct fetch`.
//!
//! The provider quota APIs return nested JSON dicts. `--json` (the default)
//! pretty-prints the body as-is; `--text` and `--table` flatten it to
//! dotted-path `key -> value` rows. Every renderer falls back to printing the
//! raw body verbatim when it is not valid JSON (e.g. an HTML error page).

use comfy_table::{Cell, CellAlignment, Color, ContentArrangement, Table, presets::UTF8_FULL};
use serde_json::Value;

/// Prints the response body as pretty JSON, or verbatim if it is not JSON.
pub fn print_fetch_json(body: &str) {
    match serde_json::from_str::<Value>(body) {
        Ok(value) => match serde_json::to_string_pretty(&value) {
            Ok(pretty) => println!("{pretty}"),
            Err(_) => println!("{body}"),
        },
        Err(_) => println!("{body}"),
    }
}

/// Prints the response as flattened `key: value` lines, or verbatim if it is
/// not JSON.
pub fn display_fetch_text(body: &str) {
    match serde_json::from_str::<Value>(body) {
        Ok(value) => {
            let mut rows = Vec::new();
            flatten(&value, String::new(), &mut rows);
            for (key, val) in rows {
                println!("{key}: {val}");
            }
        }
        Err(_) => println!("{body}"),
    }
}

/// Prints the response as a flattened Field/Value table, or the raw body
/// verbatim if it is not JSON.
pub fn display_fetch_table(body: &str) {
    let value = match serde_json::from_str::<Value>(body) {
        Ok(v) => v,
        Err(_) => {
            println!("{body}");
            return;
        }
    };

    let mut rows = Vec::new();
    flatten(&value, String::new(), &mut rows);

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("Field")
                .fg(Color::Green)
                .set_alignment(CellAlignment::Left),
            Cell::new("Value")
                .fg(Color::Green)
                .set_alignment(CellAlignment::Left),
        ]);
    for (key, val) in rows {
        table.add_row(vec![
            Cell::new(key)
                .fg(Color::Cyan)
                .set_alignment(CellAlignment::Left),
            Cell::new(val)
                .fg(Color::White)
                .set_alignment(CellAlignment::Left),
        ]);
    }

    println!("{table}");
}

/// Recursively flattens a JSON value into `(dotted-key, value)` rows.
///
/// Objects recurse as `prefix.key`, arrays as `prefix[i]`, and scalars emit one
/// row. An empty container or a scalar root still yields a single row so the
/// output is never silently empty.
fn flatten(value: &Value, prefix: String, out: &mut Vec<(String, String)>) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            for (k, v) in map {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(v, key, out);
            }
        }
        Value::Array(arr) if !arr.is_empty() => {
            for (i, v) in arr.iter().enumerate() {
                flatten(v, format!("{prefix}[{i}]"), out);
            }
        }
        _ => {
            let key = if prefix.is_empty() {
                "(root)".to_string()
            } else {
                prefix
            };
            out.push((key, scalar_to_string(value)));
        }
    }
}

/// Renders a JSON scalar (or empty container) as a plain display string,
/// unquoting strings so `"pro"` shows as `pro`.
fn scalar_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        Value::Object(_) => "{}".to_string(),
        Value::Array(_) => "[]".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatten_nested_object_uses_dotted_keys() {
        let value = serde_json::json!({
            "plan_type": "pro",
            "rate_limit": { "primary": { "used_percent": 42.5 } },
            "windows": [ { "kind": "weekly" } ],
            "empty": {}
        });
        let mut rows = Vec::new();
        flatten(&value, String::new(), &mut rows);
        let map: std::collections::HashMap<_, _> = rows.into_iter().collect();

        assert_eq!(map.get("plan_type").map(String::as_str), Some("pro"));
        assert_eq!(
            map.get("rate_limit.primary.used_percent")
                .map(String::as_str),
            Some("42.5")
        );
        assert_eq!(
            map.get("windows[0].kind").map(String::as_str),
            Some("weekly")
        );
        assert_eq!(map.get("empty").map(String::as_str), Some("{}"));
    }

    #[test]
    fn flatten_scalar_root_yields_single_row() {
        let value = serde_json::json!("hello");
        let mut rows = Vec::new();
        flatten(&value, String::new(), &mut rows);
        assert_eq!(rows, vec![("(root)".to_string(), "hello".to_string())]);
    }
}
