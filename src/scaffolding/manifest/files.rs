//! `[files]`: which template entries are emitted, how their contents are
//! treated, and how they combine with files already at the destination.

use indexmap::IndexMap;
use serde::Deserialize;

use super::hooks::default_shell;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRules {
    /// Globs (matched against the template-relative path) that are never emitted.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Globs whose contents are copied byte-for-byte instead of rendered.
    /// Paths of raw files are still rendered.
    #[serde(default)]
    pub raw: Vec<String>,
    /// Output-path globs that are created when absent but never touched once
    /// they exist, whatever the `--existing` policy says.
    #[serde(default)]
    pub keep: Vec<String>,
    /// Output-path globs combined with an existing file instead of replacing
    /// it. The first matching rule wins.
    #[serde(default)]
    pub merge: IndexMap<String, MergeRule>,
    /// A path component starting with this prefix is emitted with a leading
    /// dot instead, so `dot_gitignore` becomes `.gitignore`. Templates kept
    /// in git or published to registries need this because real dotfiles get
    /// ignored or stripped. Set to "" to disable.
    #[serde(default = "default_dot_prefix")]
    pub dot_prefix: String,
    /// Alternative delimiters for files whose own syntax clashes with the
    /// default `{{ }}`, matched by template-path glob.
    #[serde(default)]
    pub syntax: Vec<Syntax>,
    #[serde(default)]
    pub generate: Vec<Generate>,
}

fn default_dot_prefix() -> String {
    "dot_".into()
}

impl Default for FileRules {
    fn default() -> Self {
        Self {
            exclude: vec![],
            raw: vec![],
            keep: vec![],
            merge: IndexMap::new(),
            dot_prefix: default_dot_prefix(),
            syntax: vec![],
            generate: vec![],
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Syntax {
    pub path: String,
    #[serde(default = "default_block")]
    pub block: (String, String),
    #[serde(default = "default_variable")]
    pub variable: (String, String),
    #[serde(default = "default_comment")]
    pub comment: (String, String),
}

fn default_block() -> (String, String) {
    ("{%".into(), "%}".into())
}

fn default_variable() -> (String, String) {
    ("{{".into(), "}}".into())
}

fn default_comment() -> (String, String) {
    ("{#".into(), "#}".into())
}

/// `"toml"` or `{ strategy = "insert", marker = "// sc:deps" }`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum MergeRule {
    Simple(MergeStrategy),
    Detailed {
        strategy: MergeStrategy,
        #[serde(default)]
        marker: Option<String>,
    },
}

impl MergeRule {
    pub fn spec(&self) -> MergeSpec {
        match self {
            MergeRule::Simple(strategy) => MergeSpec { strategy: *strategy, marker: None },
            MergeRule::Detailed { strategy, marker } => MergeSpec { strategy: *strategy, marker: marker.clone() },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeSpec {
    pub strategy: MergeStrategy,
    pub marker: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergeStrategy {
    /// Deep-merge tables; the template's scalars win, arrays are unioned.
    /// Comments and formatting of the existing file are preserved.
    Toml,
    /// Deep-merge objects; the template's scalars win, arrays are unioned.
    Json,
    /// Deep-merge mappings; the template's scalars win, sequences are unioned.
    /// Comments in the existing file are not preserved.
    Yaml,
    /// Append lines of the template that the existing file lacks.
    AppendLines,
    /// Insert the rendered content after the line containing `marker`,
    /// unless it is already present.
    Insert,
}

impl MergeStrategy {
    pub fn name(self) -> &'static str {
        match self {
            MergeStrategy::Toml => "toml",
            MergeStrategy::Json => "json",
            MergeStrategy::Yaml => "yaml",
            MergeStrategy::AppendLines => "append-lines",
            MergeStrategy::Insert => "insert",
        }
    }
}

/// A file whose content is a command's stdout.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Generate {
    /// Output path; rendered.
    pub path: String,
    /// Command; rendered, run with `shell` in the template directory.
    pub run: String,
    #[serde(default = "default_shell")]
    pub shell: String,
    /// Only generate when the destination file does not exist yet.
    #[serde(default)]
    pub once: bool,
}
