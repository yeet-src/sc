//! Apply a plan to the destination.
//!
//! Writes happen in two phases so a failure part-way leaves the destination
//! as it was:
//! 1. **stage**: re-check every precondition the plan was computed under
//!    (a file to create is still absent, a file to modify still has the bytes
//!    the plan saw), create directories, and write each file's content to a
//!    temporary sibling with the final mode;
//! 2. **commit**: rename every temporary into place.
//!
//! Any error during staging removes the temporaries and the directories this
//! run created. Renames within one directory are atomic, so a failure during
//! commit is confined to that one path and is reported as such.

mod decide;
mod hooks;
mod stage;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use crate::plan::{Plan, PlannedHook};
use crate::shell::ShellError;

use stage::Stage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum Existing {
    /// Refuse to run if any file would be modified or conflicts.
    #[default]
    Fail,
    /// Leave modified and conflicting paths alone and apply the rest.
    Skip,
    /// Replace modified files. Conflicts (a directory where a file goes, and
    /// the like) are still refused because resolving them means deleting.
    Overwrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hooks {
    /// Run with inherited stdio so output shows live.
    Run,
    /// Run with captured stdio, for machine-readable output.
    Capture,
    Skip,
}

#[derive(Debug, Clone, Copy)]
pub struct Options {
    pub existing: Existing,
    pub hooks: Hooks,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct Outcome {
    pub created: Vec<PathBuf>,
    pub modified: Vec<PathBuf>,
    pub unchanged: Vec<PathBuf>,
    /// Paths left alone under `Existing::Skip`, with the reason.
    pub skipped: Vec<(PathBuf, String)>,
    /// Paths left alone because a manifest `keep` rule matched.
    pub kept: Vec<PathBuf>,
    /// Names of `after` hooks that ran.
    pub hooks: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error("plan was computed without a destination")]
    NoDest,
    #[error("destination {0} is inside the template, or the template is inside it")]
    Overlap(PathBuf),
    #[error("{0} path(s) would be modified or conflict; pass --existing skip or --existing overwrite")]
    Blocked(usize),
    #[error("{0}: conflicts cannot be overwritten; resolve it by hand or pass --existing skip")]
    Conflict(PathBuf),
    #[error("{0}: changed since the plan was computed; re-run plan")]
    Changed(PathBuf),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[cfg_attr(unix, allow(dead_code))]
    #[error("{0}: symlinks are not supported on this platform")]
    Symlink(PathBuf),
    #[error("after hook `{name}` failed: {source} (files were written; only the hook failed)")]
    Hook {
        name: String,
        #[source]
        source: ShellError,
    },
    /// A rename failed after earlier renames had already succeeded.
    #[error("{path}: {source} (destination is partially applied: {committed} of {total} written)")]
    Partial {
        path: PathBuf,
        #[source]
        source: std::io::Error,
        committed: usize,
        total: usize,
    },
}

/// `on_hook` is called just before each `after` hook runs, so a caller can
/// announce it ahead of the hook's own output.
pub fn apply(plan: &Plan, opts: &Options, on_hook: &mut dyn FnMut(&PlannedHook)) -> Result<Outcome, ApplyError> {
    let dest = plan.template.dest.as_deref().ok_or(ApplyError::NoDest)?;
    check_overlap(&plan.template.source, dest)?;

    let decisions = decide::decide(plan, opts)?;
    let mut outcome = Outcome::default();
    let mut stage = Stage::default();

    let staged = match stage::stage_all(dest, &decisions, &mut stage, &mut outcome) {
        Ok(()) => stage,
        Err(e) => {
            stage.rollback();
            return Err(e);
        }
    };
    staged.commit()?;

    hooks::run_after(plan, dest, opts.hooks, on_hook, &mut outcome)?;
    Ok(outcome)
}

fn check_overlap(template: &Path, dest: &Path) -> Result<(), ApplyError> {
    let (t, d) = (canonicalize_lenient(template), canonicalize_lenient(dest));
    if t.starts_with(&d) || d.starts_with(&t) {
        return Err(ApplyError::Overlap(dest.to_path_buf()));
    }
    Ok(())
}

/// Canonicalize the nearest existing ancestor and re-append the rest, so a
/// destination that does not exist yet still resolves symlinks like `/tmp`.
fn canonicalize_lenient(path: &Path) -> PathBuf {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map(|c| c.join(path)).unwrap_or_else(|_| path.to_path_buf())
    };
    let mut rest = Vec::new();
    let mut cur = abs.as_path();
    loop {
        if let Ok(c) = std::fs::canonicalize(cur) {
            return rest.iter().rev().fold(c, |acc, seg| acc.join(seg));
        }
        match (cur.file_name(), cur.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name.to_os_string());
                cur = parent;
            }
            _ => return abs,
        }
    }
}
