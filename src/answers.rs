//! The answers file written into every destination: which template, at
//! which commit, with which inputs. `update` reads it back.

use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::plan::ANSWERS_FILE;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Answers {
    pub template: Template,
    #[serde(default)]
    pub inputs: IndexMap<String, toml::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Template {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// A local directory, or a git URL when `commit` is set.
    pub source: String,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subdir: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum AnswersError {
    #[error("{0} has no {ANSWERS_FILE}; was it created by sc?")]
    NotFound(PathBuf),
    #[error("reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

impl Answers {
    pub fn load(dest: &Path) -> Result<Self, AnswersError> {
        let path = dest.join(ANSWERS_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(AnswersError::NotFound(dest.to_path_buf())),
            Err(source) => return Err(AnswersError::Io { path, source }),
        };
        toml::from_str(&text).map_err(|source| AnswersError::Parse { path, source })
    }

    pub fn to_string(&self) -> Result<String, toml::ser::Error> {
        toml::to_string(self)
    }

    /// Inputs as `--var`-style overrides, so they resolve through the same
    /// typed path as command-line values.
    pub fn overrides(&self) -> Vec<(String, String)> {
        self.inputs.iter().map(|(k, v)| (k.clone(), crate::inputs::override_text(v))).collect()
    }
}
