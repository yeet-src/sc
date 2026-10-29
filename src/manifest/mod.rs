//! The template manifest: `scaffold.toml` at the root of a template directory.
//!
//! The manifest is optional. A bare directory with no manifest is a valid
//! template whose every text file is rendered and whose variables are supplied
//! entirely on the command line.
//!
//! Sections live in their own files: [`vars`] for variable declarations,
//! [`files`] for file rules, [`hooks`] for hooks. Validation is here.

mod files;
mod hooks;
mod vars;

pub use files::*;
pub use hooks::*;
pub use vars::*;

use std::path::Path;

use indexmap::IndexMap;
use serde::Deserialize;

pub const MANIFEST_FILE: &str = "scaffold.toml";
/// Directory holding partials, hook scripts and golden tests. Never emitted.
pub const CONTROL_DIR: &str = ".scaffold";

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    #[serde(default)]
    pub template: TemplateMeta,
    #[serde(default)]
    pub files: FileRules,
    /// Declaration order is preserved so prompts appear in the author's order.
    #[serde(default)]
    pub vars: IndexMap<String, VarSpec>,
    #[serde(default)]
    pub hooks: Vec<Hook>,
    /// Templates this one builds on. Their files are planned first and this
    /// template's files override them path by path; their variables are
    /// declared before this template's.
    #[serde(default)]
    pub uses: Vec<Uses>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateMeta {
    pub name: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Uses {
    /// A path relative to this template, or a git spec like `gh:org/base#v2`.
    pub source: String,
    pub subdir: Option<std::path::PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("variable `{name}`: {reason}")]
    Invalid { name: String, reason: String },
    #[error("merge rule `{path}`: {reason}")]
    InvalidMerge { path: String, reason: String },
    #[error("hook #{index}: {reason}")]
    InvalidHook { index: usize, reason: String },
}

impl Manifest {
    /// This manifest with the variables of `bases` declared first, so
    /// inputs for a composed template resolve against every layer.
    pub fn with_base_vars<'a>(&self, bases: impl IntoIterator<Item = &'a Manifest>) -> Manifest {
        let mut vars = IndexMap::new();
        for b in bases {
            for (k, v) in &b.vars {
                vars.insert(k.clone(), v.clone());
            }
        }
        for (k, v) in &self.vars {
            vars.insert(k.clone(), v.clone());
        }
        Manifest { vars, ..self.clone() }
    }

    /// Load `scaffold.toml` from `template_root`, or return the default
    /// manifest when the file does not exist.
    pub fn load(template_root: &Path) -> Result<Self, ManifestError> {
        let path = template_root.join(MANIFEST_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(ManifestError::Io { path: path.display().to_string(), source });
            }
        };
        let manifest: Manifest = toml::from_str(&text)
            .map_err(|source| ManifestError::Parse { path: path.display().to_string(), source })?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<(), ManifestError> {
        for (i, h) in self.hooks.iter().enumerate() {
            let bad = |reason: &str| ManifestError::InvalidHook { index: i + 1, reason: reason.into() };
            match (&h.run, &h.script) {
                (Some(_), Some(_)) => return Err(bad("`run` and `script` are mutually exclusive")),
                (None, None) => return Err(bad("one of `run` or `script` is required")),
                (None, Some(s)) if Path::new(s).is_absolute() || s.contains("..") => {
                    return Err(bad("`script` must be a relative path under .scaffold/"));
                }
                _ => {}
            }
        }
        for (path, rule) in &self.files.merge {
            let spec = rule.spec();
            let bad = |reason: &str| ManifestError::InvalidMerge { path: path.clone(), reason: reason.into() };
            match (spec.strategy, &spec.marker) {
                (MergeStrategy::Insert, None) => return Err(bad("strategy \"insert\" requires `marker`")),
                (MergeStrategy::Insert, Some(m)) if m.trim().is_empty() => return Err(bad("`marker` is empty")),
                (MergeStrategy::Insert, _) => {}
                (_, Some(_)) => return Err(bad("`marker` is only valid for strategy \"insert\"")),
                _ => {}
            }
        }
        for (name, spec) in &self.vars {
            let invalid = |reason: String| ManifestError::Invalid { name: name.clone(), reason };
            if !is_identifier(name) {
                return Err(invalid("name must be a valid identifier".into()));
            }
            match spec.kind {
                VarKind::Choice if spec.choices.is_empty() => {
                    return Err(invalid("type = \"choice\" requires `choices`".into()));
                }
                VarKind::Choice => {}
                _ if !spec.choices.is_empty() => {
                    return Err(invalid("`choices` is only valid for type = \"choice\"".into()));
                }
                _ => {}
            }
            if let Some(p) = &spec.pattern {
                regex::Regex::new(p).map_err(|e| invalid(format!("bad pattern: {e}")))?;
            }
        }
        Ok(())
    }
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}
