//! `[vars.<name>]`: the typed inputs a template asks for.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VarSpec {
    #[serde(rename = "type", default)]
    pub kind: VarKind,
    /// Question shown when asking for the value interactively.
    #[allow(dead_code)] // read by the prompting layer, not yet written
    pub prompt: Option<String>,
    pub default: Option<toml::Value>,
    /// Allowed values when `kind == Choice`.
    #[serde(default)]
    pub choices: Vec<String>,
    /// Regex the string form must fully match.
    pub pattern: Option<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VarKind {
    #[default]
    String,
    Bool,
    Int,
    Choice,
}
