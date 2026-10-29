//! `[tree]`: per-path rules keyed by output path, globs allowed. A key with
//! a trailing slash covers a directory and everything under it.
//!
//! `variants = "VAR"` makes the key a variant group: its candidates live
//! under `.sc/variants/<key>/`, named by the value of input `VAR` they
//! serve, and the one matching the current value is emitted at the key.
//! In the template's working tree the key is a symlink to the checked-out
//! candidate, so the listing looks like the output and editing the file
//! edits that variant. `sc checkout` switches it.
//!
//! ```toml
//! [tree]
//! ".github/"   = { when = "ci" }
//! "Dockerfile" = { when = "docker" }
//! "Makefile"   = { when = "not docker" }
//! "Cargo.toml" = { merge = "toml" }
//! "README.md"  = { keep = true }
//! "assets/**"  = { raw = true }
//! "src/lib.rs" = { merge = "insert", marker = "// sc:modules" }
//! ```
//!
//! `when` is a template expression over the inputs; an entry whose condition
//! is false is dropped from the plan. The other fields are the same rules as
//! `[files]` keep, merge and raw, in one place.

use serde::Deserialize;

use super::MergeStrategy;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeEntry {
    pub when: Option<String>,
    /// Name of the input whose value selects this path's variant.
    pub variants: Option<String>,
    pub merge: Option<MergeStrategy>,
    pub marker: Option<String>,
    #[serde(default)]
    pub keep: bool,
    #[serde(default)]
    pub raw: bool,
}

/// A directory key (`".github/"`) matches the directory and its contents;
/// anything else is a glob over output paths.
pub fn glob_for(key: &str) -> Vec<String> {
    match key.strip_suffix('/') {
        Some(dir) => vec![dir.to_string(), format!("{dir}/**")],
        None => vec![key.to_string()],
    }
}
