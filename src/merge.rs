//! Merge strategies: pure functions from (existing, rendered) to the merged
//! text. Nothing here touches the filesystem, so each strategy is testable in
//! isolation and the planner can show the result as a diff before apply.

use crate::manifest::{MergeSpec, MergeStrategy};

#[derive(Debug, thiserror::Error)]
pub enum MergeError {
    #[error("existing file is not valid {format}: {detail}")]
    Existing { format: &'static str, detail: String },
    #[error("rendered template is not valid {format}: {detail}")]
    Rendered { format: &'static str, detail: String },
    #[error("marker `{0}` not found in existing file")]
    MarkerNotFound(String),
}

pub fn merge(spec: &MergeSpec, existing: &str, rendered: &str) -> Result<String, MergeError> {
    match spec.strategy {
        MergeStrategy::Toml => merge_toml(existing, rendered),
        MergeStrategy::Json => merge_json(existing, rendered),
        MergeStrategy::Yaml => merge_yaml(existing, rendered),
        MergeStrategy::AppendLines => Ok(append_lines(existing, rendered)),
        MergeStrategy::Insert => insert_after_marker(existing, rendered, spec.marker.as_deref().unwrap_or_default()),
    }
}

fn merge_yaml(existing: &str, rendered: &str) -> Result<String, MergeError> {
    use serde_yaml_ng::Value;

    let mut base: Value = serde_yaml_ng::from_str(existing)
        .map_err(|e| MergeError::Existing { format: "YAML", detail: e.to_string() })?;
    let overlay: Value = serde_yaml_ng::from_str(rendered)
        .map_err(|e| MergeError::Rendered { format: "YAML", detail: e.to_string() })?;

    fn merge_values(base: &mut Value, overlay: Value) {
        match (base, overlay) {
            (Value::Mapping(b), Value::Mapping(o)) => {
                for (k, v) in o {
                    match b.get_mut(&k) {
                        Some(slot) => merge_values(slot, v),
                        None => {
                            b.insert(k, v);
                        }
                    }
                }
            }
            (Value::Sequence(b), Value::Sequence(o)) => {
                for v in o {
                    if !b.contains(&v) {
                        b.push(v);
                    }
                }
            }
            (b, o) => *b = o,
        }
    }

    // An empty existing file parses as null; treat it as an empty mapping.
    if base.is_null() {
        base = Value::Mapping(Default::default());
    }
    merge_values(&mut base, overlay);
    serde_yaml_ng::to_string(&base).map_err(|e| MergeError::Rendered { format: "YAML", detail: e.to_string() })
}

/// Insert `block` after the first line containing `marker`. Idempotent: if
/// the block already follows the marker, nothing changes.
fn insert_after_marker(existing: &str, block: &str, marker: &str) -> Result<String, MergeError> {
    let block = block.strip_suffix('\n').unwrap_or(block);
    let lines: Vec<&str> = existing.split_inclusive('\n').collect();
    let idx = lines
        .iter()
        .position(|l| l.contains(marker))
        .ok_or_else(|| MergeError::MarkerNotFound(marker.to_string()))?;

    let after: String = lines[idx + 1..].concat();
    if after.starts_with(block) && after[block.len()..].starts_with('\n') || (after.is_empty() && block.is_empty()) {
        return Ok(existing.to_string());
    }

    let mut out: String = lines[..=idx].concat();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(block);
    out.push('\n');
    out.push_str(&after);
    Ok(out)
}

fn merge_toml(existing: &str, rendered: &str) -> Result<String, MergeError> {
    use toml_edit::{DocumentMut, Item, Table};

    let mut base: DocumentMut = existing
        .parse()
        .map_err(|e: toml_edit::TomlError| MergeError::Existing { format: "TOML", detail: e.to_string() })?;
    let overlay: DocumentMut = rendered
        .parse()
        .map_err(|e: toml_edit::TomlError| MergeError::Rendered { format: "TOML", detail: e.to_string() })?;

    fn merge_tables(base: &mut Table, overlay: &Table) {
        for (key, item) in overlay.iter() {
            match (base.get_mut(key), item) {
                (Some(Item::Table(b)), Item::Table(o)) => merge_tables(b, o),
                (Some(Item::Value(bv)), Item::Value(ov)) if bv.is_inline_table() && ov.is_inline_table() => {
                    let (b, o) = (bv.as_inline_table_mut().unwrap(), ov.as_inline_table().unwrap());
                    for (k, v) in o.iter() {
                        b.insert(k, v.clone());
                    }
                    b.fmt();
                }
                (Some(Item::Value(bv)), Item::Value(ov)) if bv.is_array() && ov.is_array() => {
                    let b = bv.as_array_mut().unwrap();
                    for v in ov.as_array().unwrap().iter() {
                        if !b.iter().any(|x| x.to_string().trim() == v.to_string().trim()) {
                            b.push(v.clone());
                        }
                    }
                }
                (Some(Item::ArrayOfTables(b)), Item::ArrayOfTables(o)) => {
                    for t in o.iter() {
                        b.push(t.clone());
                    }
                }
                // Scalar replaced by scalar: keep the surrounding whitespace
                // and any trailing comment the user wrote on that line.
                (Some(Item::Value(bv)), Item::Value(ov)) => {
                    let mut nv = ov.clone();
                    *nv.decor_mut() = bv.decor().clone();
                    *bv = nv;
                }
                _ => {
                    let mut item = item.clone();
                    // Keep the document readable: a sub-table from the overlay
                    // stays a proper `[table]`, not an inline one.
                    if let Item::Table(t) = &mut item {
                        t.set_implicit(false);
                    }
                    base[key] = item;
                }
            }
        }
    }

    merge_tables(base.as_table_mut(), overlay.as_table());
    Ok(base.to_string())
}

fn merge_json(existing: &str, rendered: &str) -> Result<String, MergeError> {
    use serde_json::Value;

    let mut base: Value = serde_json::from_str(existing)
        .map_err(|e| MergeError::Existing { format: "JSON", detail: e.to_string() })?;
    let overlay: Value = serde_json::from_str(rendered)
        .map_err(|e| MergeError::Rendered { format: "JSON", detail: e.to_string() })?;

    fn merge_values(base: &mut Value, overlay: Value) {
        match (base, overlay) {
            (Value::Object(b), Value::Object(o)) => {
                for (k, v) in o {
                    match b.get_mut(&k) {
                        Some(slot) => merge_values(slot, v),
                        None => {
                            b.insert(k, v);
                        }
                    }
                }
            }
            (Value::Array(b), Value::Array(o)) => {
                for v in o {
                    if !b.contains(&v) {
                        b.push(v);
                    }
                }
            }
            (b, o) => *b = o,
        }
    }

    merge_values(&mut base, overlay);
    let mut out = serde_json::to_string_pretty(&base).expect("Value always serializes");
    out.push('\n');
    Ok(out)
}

/// Lines of `rendered` that `existing` lacks are appended, in order. Blank
/// lines are kept only between appended lines, so a template section like
/// `\n# Rust\ntarget/\n` lands as a tidy block.
fn append_lines(existing: &str, rendered: &str) -> String {
    let have: std::collections::HashSet<&str> = existing.lines().map(str::trim_end).collect();
    let missing: Vec<&str> = rendered
        .lines()
        .map(str::trim_end)
        .filter(|l| l.is_empty() || !have.contains(l))
        .collect();
    let first = missing.iter().position(|l| !l.is_empty());
    let last = missing.iter().rposition(|l| !l.is_empty());
    let (Some(first), Some(last)) = (first, last) else {
        return existing.to_string();
    };

    let mut out = existing.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    for line in &missing[first..=last] {
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merge(strategy: MergeStrategy, existing: &str, rendered: &str) -> Result<String, MergeError> {
        super::merge(&MergeSpec { strategy, marker: None }, existing, rendered)
    }

    #[test]
    fn yaml_deep_merges() {
        let out = merge(MergeStrategy::Yaml, "a:\n  b: 1\nlist:\n- x\n", "a:\n  c: 2\nlist:\n- x\n- y\n").unwrap();
        assert_eq!(out, "a:\n  b: 1\n  c: 2\nlist:\n- x\n- y\n");
        assert_eq!(merge(MergeStrategy::Yaml, "", "k: v\n").unwrap(), "k: v\n");
    }

    #[test]
    fn insert_after_marker_is_idempotent() {
        let spec = MergeSpec { strategy: MergeStrategy::Insert, marker: Some("// sc:imports".into()) };
        let existing = "use std::io;\n// sc:imports\nfn main() {}\n";
        let once = super::merge(&spec, existing, "use clap::Parser;\n").unwrap();
        assert_eq!(once, "use std::io;\n// sc:imports\nuse clap::Parser;\nfn main() {}\n");
        assert_eq!(super::merge(&spec, &once, "use clap::Parser;\n").unwrap(), once);
        let err = super::merge(&spec, "nothing here\n", "x").unwrap_err();
        assert!(matches!(err, MergeError::MarkerNotFound(_)));
    }

    #[test]
    fn toml_preserves_comments_and_adds_keys() {
        let existing = "# my project\n[package]\nname = \"mine\" # keep\nversion = \"0.2.0\"\n\n[dependencies]\nserde = \"1\"\n";
        let rendered = "[package]\nname = \"tpl\"\nedition = \"2024\"\n\n[dependencies]\nclap = \"4\"\n\n[dev-dependencies]\ntempfile = \"3\"\n";
        let out = merge(MergeStrategy::Toml, existing, rendered).unwrap();
        assert_eq!(
            out,
            "# my project\n[package]\nname = \"tpl\" # keep\nversion = \"0.2.0\"\nedition = \"2024\"\n\n[dependencies]\nserde = \"1\"\nclap = \"4\"\n\n[dev-dependencies]\ntempfile = \"3\"\n"
        );
    }

    #[test]
    fn toml_unions_arrays_and_merges_inline_tables() {
        let out = merge(
            MergeStrategy::Toml,
            "a = [1, 2]\nd = { x = 1 }\n",
            "a = [2, 3]\nd = { y = 2 }\n",
        )
        .unwrap();
        assert_eq!(out, "a = [1, 2, 3]\nd = { x = 1, y = 2 }\n");
    }

    #[test]
    fn toml_reports_bad_existing() {
        let err = merge(MergeStrategy::Toml, "not = = toml", "a = 1").unwrap_err();
        assert!(matches!(err, MergeError::Existing { format: "TOML", .. }), "{err}");
    }

    #[test]
    fn json_deep_merges() {
        let out = merge(
            MergeStrategy::Json,
            r#"{"name":"mine","scripts":{"test":"jest"},"files":["a"]}"#,
            r#"{"scripts":{"lint":"eslint"},"files":["a","b"],"private":true}"#,
        )
        .unwrap();
        assert_eq!(
            out,
            "{\n  \"name\": \"mine\",\n  \"scripts\": {\n    \"test\": \"jest\",\n    \"lint\": \"eslint\"\n  },\n  \"files\": [\n    \"a\",\n    \"b\"\n  ],\n  \"private\": true\n}\n"
        );
    }

    #[test]
    fn append_lines_dedupes_and_trims_blanks() {
        let out = merge(MergeStrategy::AppendLines, "node_modules/\n.env", "# Rust\ntarget/\n\n.env\n\n").unwrap();
        assert_eq!(out, "node_modules/\n.env\n# Rust\ntarget/\n");
        assert_eq!(merge(MergeStrategy::AppendLines, "a\n", "a\n").unwrap(), "a\n");
    }
}
