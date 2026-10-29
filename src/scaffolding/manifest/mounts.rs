//! `[[mounts]]`: sub-templates applied at a path inside this one, once or
//! once per element of a list input.

use std::path::PathBuf;

use indexmap::IndexMap;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mount {
    /// Where the sub-template lands, relative to the destination. Rendered
    /// with this template's inputs plus `item` and `index` under `for`.
    pub path: String,
    /// A directory relative to this template, or a git spec. A directory
    /// inside this template is treated as control material, never emitted.
    pub template: String,
    pub subdir: Option<PathBuf>,
    /// Name of a list input; the mount is expanded once per element, with
    /// the element as `item` and its position as `index`.
    #[serde(rename = "for")]
    pub each: Option<String>,
    /// Inputs for the sub-template; each value is rendered as a template
    /// string and then typed by the sub-template's manifest.
    #[serde(default)]
    pub inputs: IndexMap<String, String>,
    /// Template expression; the mount is skipped when it is false.
    pub when: Option<String>,
}
