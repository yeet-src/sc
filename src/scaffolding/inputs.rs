//! Resolve the values every template variable takes for this run.
//!
//! Sources, highest priority first: explicit overrides (`--var k=v`), then the
//! manifest default. A declared variable with neither is an error; interactive
//! prompting is a later layer on top of this and never reaches the planner.

use std::collections::BTreeMap;

use minijinja::Value;
use serde::Serialize;

use crate::scaffolding::manifest::{Manifest, VarKind, VarSpec};

#[derive(Debug, thiserror::Error)]
pub enum InputError {
    #[error("variable `{0}` is required but was not provided")]
    Missing(String),
    #[error("variable `{name}`: {reason}")]
    Invalid { name: String, reason: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct Input {
    pub name: String,
    pub value: Value,
    pub source: Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Given explicitly on the command line.
    Flag,
    /// Taken from the manifest default.
    Default,
    /// Added or overridden by a `before` hook.
    Hook,
    /// Read back from the destination's answers file.
    Answers,
}

/// Resolved inputs in manifest declaration order, followed by undeclared
/// overrides sorted by name.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(transparent)]
pub struct Inputs(pub Vec<Input>);

impl Inputs {
    /// Insert or replace a value, keeping declaration order for existing names.
    pub fn set(&mut self, name: &str, value: Value, source: Source) {
        match self.0.iter_mut().find(|i| i.name == name) {
            Some(i) => {
                i.value = value;
                i.source = source;
            }
            None => self.0.push(Input { name: name.to_string(), value, source }),
        }
    }

    /// The template rendering context.
    pub fn context(&self) -> Value {
        let map: BTreeMap<&str, &Value> = self.0.iter().map(|i| (i.name.as_str(), &i.value)).collect();
        Value::from_serialize(&map)
    }
}

pub fn resolve(manifest: &Manifest, overrides: &[(String, String)]) -> Result<Inputs, InputError> {
    let mut out = Vec::new();
    let overrides: BTreeMap<&str, &str> =
        overrides.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();

    for (name, spec) in &manifest.vars {
        let (value, source) = match overrides.get(name.as_str()) {
            Some(raw) => (parse(name, spec, raw)?, Source::Flag),
            None => match &spec.default {
                Some(d) => (from_default(name, spec, d)?, Source::Default),
                None => return Err(InputError::Missing(name.clone())),
            },
        };
        out.push(Input { name: name.clone(), value, source });
    }

    // Overrides not declared in the manifest are accepted as plain strings so
    // that manifest-less templates are usable.
    for (k, v) in overrides {
        if !manifest.vars.contains_key(k) {
            out.push(Input { name: k.to_string(), value: Value::from(v), source: Source::Flag });
        }
    }
    Ok(Inputs(out))
}

fn parse(name: &str, spec: &VarSpec, raw: &str) -> Result<Value, InputError> {
    let invalid = |reason: String| InputError::Invalid { name: name.to_string(), reason };
    let value = match spec.kind {
        VarKind::String => Value::from(raw),
        VarKind::Bool => match raw {
            "true" | "yes" | "y" | "1" => Value::from(true),
            "false" | "no" | "n" | "0" => Value::from(false),
            _ => return Err(invalid(format!("expected a boolean, got `{raw}`"))),
        },
        VarKind::Int => Value::from(
            raw.parse::<i64>().map_err(|_| invalid(format!("expected an integer, got `{raw}`")))?,
        ),
        VarKind::Choice => {
            if !spec.choices.iter().any(|c| c == raw) {
                return Err(invalid(format!(
                    "`{raw}` is not one of: {}",
                    spec.choices.join(", ")
                )));
            }
            Value::from(raw)
        }
        VarKind::List => parse_list(raw).ok_or_else(|| invalid(format!("expected a JSON or TOML array, got `{raw}`")))?,
    };
    check_pattern(name, spec, raw)?;
    Ok(value)
}

/// Lists arrive as text: JSON first, then TOML (`[{ name = "a" }]`).
fn parse_list(raw: &str) -> Option<Value> {
    if let Ok(serde_json::Value::Array(a)) = serde_json::from_str::<serde_json::Value>(raw) {
        return Some(Value::from_serialize(&a));
    }
    let wrapped: toml::Table = toml::from_str(&format!("v = {raw}")).ok()?;
    match wrapped.get("v") {
        Some(toml::Value::Array(a)) => Some(Value::from_serialize(a)),
        _ => None,
    }
}

fn from_default(name: &str, spec: &VarSpec, d: &toml::Value) -> Result<Value, InputError> {
    let invalid = |reason: String| InputError::Invalid { name: name.to_string(), reason };
    match (spec.kind, d) {
        (VarKind::String, toml::Value::String(s)) => parse(name, spec, s),
        (VarKind::Choice, toml::Value::String(s)) => parse(name, spec, s),
        (VarKind::Bool, toml::Value::Boolean(b)) => Ok(Value::from(*b)),
        (VarKind::Int, toml::Value::Integer(i)) => Ok(Value::from(*i)),
        (VarKind::List, toml::Value::Array(a)) => Ok(Value::from_serialize(a)),
        (kind, other) => Err(invalid(format!("default `{other}` does not match type {kind:?}"))),
    }
}

fn check_pattern(name: &str, spec: &VarSpec, raw: &str) -> Result<(), InputError> {
    if let Some(p) = &spec.pattern {
        // Validated at manifest load; anchor so the whole value must match.
        let re = regex::Regex::new(&format!("^(?:{p})$")).expect("pattern validated by manifest");
        if !re.is_match(raw) {
            return Err(InputError::Invalid {
                name: name.to_string(),
                reason: format!("`{raw}` does not match pattern `{p}`"),
            });
        }
    }
    Ok(())
}

/// The text form of a value, suitable as an override: strings as-is,
/// everything else as JSON so lists and tables round-trip.
pub fn override_text<T: serde::Serialize>(v: &T) -> String {
    let json = serde_json::to_value(v).unwrap_or(serde_json::Value::Null);
    match json {
        serde_json::Value::String(s) => s,
        other => other.to_string(),
    }
}

/// Parse a `key=value` command-line argument.
pub fn parse_override(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((k, v)) if !k.is_empty() => Ok((k.to_string(), v.to_string())),
        _ => Err(format!("expected KEY=VALUE, got `{s}`")),
    }
}
