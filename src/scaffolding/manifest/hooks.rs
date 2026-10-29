//! `[[hooks]]`: commands that run before planning or after apply.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    pub phase: Phase,
    /// Label shown in plans and output; defaults to the first script line.
    pub name: Option<String>,
    /// Inline script text. Exactly one of `run` and `script` is required.
    pub run: Option<String>,
    /// Path of a script under `.sc/`, run via its shebang.
    pub script: Option<String>,
    /// Interpreter for `run`.
    #[serde(default = "default_shell")]
    pub shell: String,
    /// Template expression; the hook is skipped when it renders to
    /// "", "false", "0" or "no".
    pub when: Option<String>,
    /// Render the script text as a template before running it.
    #[serde(default = "default_true")]
    pub render: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Runs in the template directory before planning. May print a JSON
    /// object on stdout to add or override inputs.
    Before,
    /// Runs in the destination after apply commits.
    After,
}

pub fn default_shell() -> String {
    "sh".into()
}

pub fn default_true() -> bool {
    true
}
