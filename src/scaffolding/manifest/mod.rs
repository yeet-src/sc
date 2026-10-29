//! The template manifest: `sc.toml` at the root of a template directory.
//!
//! The manifest is optional. A bare directory with no manifest is a valid
//! template whose every text file is rendered and whose variables are supplied
//! entirely on the command line.
//!
//! Sections live in their own files: [`vars`] for variable declarations,
//! [`files`] for file rules, [`hooks`] for hooks. Validation is here.

mod files;
mod hooks;
mod mounts;
mod tree;
mod vars;

pub use files::*;
pub use hooks::*;
pub use mounts::*;
pub use tree::*;
pub use vars::*;

use std::path::Path;

use indexmap::IndexMap;
use serde::Deserialize;

pub const MANIFEST_FILE: &str = "sc.toml";
/// Directory holding partials, hook scripts and golden tests. Never emitted.
pub const CONTROL_DIR: &str = ".sc";

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
    #[serde(default)]
    pub mounts: Vec<Mount>,
    /// Per-path rules keyed by output path; see [`TreeEntry`].
    #[serde(default)]
    pub tree: IndexMap<String, TreeEntry>,
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
    #[error("mount `{path}`: {reason}")]
    InvalidMount { path: String, reason: String },
    #[error("rename `{from}`: {reason}")]
    InvalidRename { from: String, reason: String },
    #[error("tree entry `{path}`: {reason}")]
    InvalidTree { path: String, reason: String },
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

    /// Load `sc.toml` from `template_root`, or return the default
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
        let mut manifest: Manifest = toml::from_str(&text)
            .map_err(|source| ManifestError::Parse { path: path.display().to_string(), source })?;
        manifest.normalize();
        manifest.validate()?;
        Ok(manifest)
    }

    /// Fold `[tree]` keep, merge and raw rules into the `[files]` rule sets,
    /// so the planner has one place to look. `when` stays on the entry.
    fn normalize(&mut self) {
        for (key, entry) in &self.tree {
            for glob in glob_for(key) {
                if entry.keep && !self.files.keep.contains(&glob) {
                    self.files.keep.push(glob.clone());
                }
                if let Some(strategy) = entry.merge {
                    self.files.merge.entry(glob.clone()).or_insert(MergeRule::Detailed {
                        strategy,
                        marker: entry.marker.clone(),
                    });
                }
                if entry.raw && !self.files.raw_out.contains(&glob) {
                    self.files.raw_out.push(glob);
                }
            }
        }
    }

    /// `[tree]` variant groups as (output path, selecting input) pairs.
    pub fn variant_groups(&self) -> Vec<(String, String)> {
        self.tree
            .iter()
            .filter_map(|(k, e)| e.variants.as_ref().map(|v| (k.trim_end_matches('/').to_string(), v.clone())))
            .collect()
    }

    /// `[tree]` conditions as (output-path globs, expression) pairs.
    pub fn conditions(&self) -> Vec<(Vec<String>, String)> {
        self.tree
            .iter()
            .filter_map(|(k, e)| e.when.as_ref().map(|w| (glob_for(k), w.clone())))
            .collect()
    }

    fn validate(&self) -> Result<(), ManifestError> {
        for (i, h) in self.hooks.iter().enumerate() {
            let bad = |reason: &str| ManifestError::InvalidHook { index: i + 1, reason: reason.into() };
            match (&h.run, &h.script) {
                (Some(_), Some(_)) => return Err(bad("`run` and `script` are mutually exclusive")),
                (None, None) => return Err(bad("one of `run` or `script` is required")),
                (None, Some(s)) if Path::new(s).is_absolute() || s.contains("..") => {
                    return Err(bad("`script` must be a relative path under .sc/"));
                }
                _ => {}
            }
        }
        for (path, e) in &self.tree {
            let bad = |reason: &str| ManifestError::InvalidTree { path: path.clone(), reason: reason.into() };
            if e.marker.is_some() && e.merge != Some(MergeStrategy::Insert) {
                return Err(bad("`marker` is only valid with merge = \"insert\""));
            }
            if e.merge == Some(MergeStrategy::Insert) && e.marker.as_deref().unwrap_or("").trim().is_empty() {
                return Err(bad("merge = \"insert\" requires `marker`"));
            }
            if e.when.as_deref().is_some_and(|w| w.trim().is_empty()) {
                return Err(bad("`when` is empty"));
            }
            if let Some(v) = &e.variants {
                if !is_identifier(v) {
                    return Err(bad("`variants` must name an input"));
                }
                if path.contains('*') || path.contains('?') {
                    return Err(bad("a variant group is one path, not a glob"));
                }
            }
        }
        for (from, to) in &self.files.rename {
            let bad = |reason: &str| ManifestError::InvalidRename { from: from.clone(), reason: reason.into() };
            let to = to.trim_end_matches('/');
            if from.trim_matches('/').is_empty() || to.is_empty() {
                return Err(bad("both sides must be non-empty"));
            }
            if Path::new(to).components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
                return Err(bad("the new name must be a plain relative path"));
            }
        }
        for m in &self.mounts {
            let bad = |reason: &str| ManifestError::InvalidMount { path: m.path.clone(), reason: reason.into() };
            if m.path.trim().is_empty() {
                return Err(bad("`path` is empty"));
            }
            if m.template.trim().is_empty() {
                return Err(bad("`template` is empty"));
            }
            if let Some(v) = &m.each {
                match self.vars.get(v) {
                    Some(spec) if spec.kind == VarKind::List => {}
                    Some(_) => return Err(bad(&format!("`for = \"{v}\"` must name a list variable"))),
                    None => return Err(bad(&format!("`for = \"{v}\"` names an undeclared variable"))),
                }
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
