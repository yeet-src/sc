//! The plan data model. A [`Plan`] is plain data that serializes to JSON;
//! everything that inspects, tests or displays a run works on this value.

use std::path::PathBuf;

use serde::Serialize;

use crate::inputs::Inputs;
use crate::manifest::{MergeStrategy, Phase};
use crate::merge::MergeError;
use crate::render::RenderError;
use crate::shell::ShellError;

use super::ANSWERS_FILE;

#[derive(Debug, Clone, Default)]
pub struct PlanOptions {
    /// Permit commands to run while planning: `before` hooks, `generate`
    /// rules and the `shell()` template function. Everything that runs is
    /// listed in `Plan::executed`.
    pub allow_shell: bool,
    /// Provenance when the template came from git; recorded in the answers
    /// file so `update` can find the same template again.
    pub git: Option<crate::source::GitInfo>,
}

#[derive(Debug, Serialize)]
pub struct Plan {
    pub template: TemplateInfo,
    /// The inputs the plan was rendered with, recorded so the apply step can
    /// persist them for later updates.
    pub inputs: Inputs,
    /// Operations ordered by output path; a directory precedes its contents.
    pub ops: Vec<Op>,
    /// Template entries that produce no operation, and why.
    pub dropped: Vec<Dropped>,
    pub warnings: Vec<Warning>,
    /// Hooks whose `when` passed, with their text fully rendered. `before`
    /// hooks have already run by the time the plan exists.
    pub hooks: Vec<PlannedHook>,
    /// Commands that ran while planning, in order.
    pub executed: Vec<Executed>,
}

#[derive(Debug, Serialize)]
pub struct PlannedHook {
    pub phase: Phase,
    /// Short label: the first line of an inline script, or the script path.
    pub name: String,
    pub text: String,
    pub exec: Exec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Exec {
    Shell { shell: String },
    Script { path: String },
}

#[derive(Debug, Serialize)]
pub struct Executed {
    pub purpose: String,
    pub command: String,
}

#[derive(Debug, Serialize)]
pub struct TemplateInfo {
    pub name: Option<String>,
    pub description: Option<String>,
    /// The directory planned from (a checkout, for git templates).
    pub source: PathBuf,
    pub git: Option<crate::source::GitInfo>,
    pub dest: Option<PathBuf>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Op {
    /// Where the entry came from, relative to the template root.
    pub template_path: PathBuf,
    /// Where it lands, relative to the destination.
    pub path: PathBuf,
    pub kind: OpKind,
    pub status: Status,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OpKind {
    CreateDir,
    WriteFile { content: Content, mode: u32, origin: Origin },
    Symlink { target: PathBuf },
}

/// What applying the op would do to the destination.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Status {
    /// Nothing exists at the path.
    Create,
    /// Something equivalent already exists.
    Unchanged,
    /// A file exists with different content and/or mode.
    Modify {
        mode_only: bool,
        /// The current bytes, kept so a diff can be shown and so apply can
        /// detect the file changing underneath it. Omitted from JSON.
        #[serde(skip)]
        existing: Vec<u8>,
    },
    /// A file exists and a manifest `merge` rule combined it with the
    /// rendered content; the op's content is the merged result.
    Merge {
        strategy: MergeStrategy,
        #[serde(skip)]
        existing: Vec<u8>,
    },
    /// Something exists and a manifest `keep` rule says to leave it alone.
    Kept { rule: String },
    /// `update` three-way merged the user's edits with the template's
    /// changes since the previous commit. With `conflicts`, the content
    /// carries conflict markers for the user to resolve.
    Update {
        conflicts: bool,
        #[serde(skip)]
        existing: Vec<u8>,
    },
    /// Something of an incompatible kind exists; apply cannot proceed here
    /// without an explicit policy.
    Conflict { reason: String },
}

#[derive(Debug, PartialEq, Eq)]
pub enum Content {
    Text(String),
    Bytes(Vec<u8>),
}

impl Content {
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Content::Text(s) => s.as_bytes(),
            Content::Bytes(b) => b,
        }
    }
}

impl Serialize for Content {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("Content", 1)?;
        match self {
            Content::Text(t) => st.serialize_field("text", t)?,
            Content::Bytes(b) => st.serialize_field("binary_size", &b.len())?,
        }
        st.end()
    }
}

/// Why a file's content took the form it did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Origin {
    Rendered,
    /// Matched this `raw` glob.
    Raw { rule: String },
    /// Detected as binary.
    Binary,
    /// Produced by the engine itself, such as the answers file.
    Generated,
    /// Stdout of a `generate` rule's command.
    Shell { command: String, once: bool },
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Dropped {
    pub template_path: PathBuf,
    pub reason: DropReason,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DropReason {
    /// Consumed by the engine, never emitted.
    Control,
    /// Matched this `exclude` glob.
    Excluded { rule: String },
    /// A path component rendered to the empty string.
    EmptyPath,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Warning {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("{path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("bad glob in manifest: {0}")]
    Glob(#[from] globset::Error),
    #[error(transparent)]
    Render(#[from] RenderError),
    #[error("{template_path}: path rendered to `{rendered}`, which {reason}")]
    BadPath { template_path: String, rendered: String, reason: &'static str },
    #[error("template emits `{ANSWERS_FILE}`, which is reserved for the answers file")]
    ReservedPath,
    #[error("serializing answers file: {0}")]
    Answers(#[from] toml::ser::Error),
    #[error("merging {path} ({}): {source}", strategy.name())]
    Merge {
        path: String,
        strategy: MergeStrategy,
        #[source]
        source: MergeError,
    },
    #[error("{path}: merge rule `{rule}` matched but the file is {what}")]
    Unmergeable { path: String, rule: String, what: &'static str },
    #[error("template runs commands while planning; pass --allow-shell to permit:{}", commands.iter().map(|c| format!("\n  {c}")).collect::<String>())]
    ShellNotAllowed { commands: Vec<String> },
    #[error("{what}: {source}")]
    Shell {
        what: String,
        #[source]
        source: ShellError,
    },
    #[error("before hook `{name}` printed something that is not a JSON object: {detail}")]
    HookOutput { name: String, detail: String },
    #[error("hook script {0}: {1}")]
    HookScript(String, std::io::Error),
}
