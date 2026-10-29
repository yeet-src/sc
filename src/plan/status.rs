//! Comparing each op against what is already at the destination.

use std::path::Path;

use super::rules::{MergeRules, Rules};
use super::walk::file_mode;
use super::{Content, Op, OpKind, Origin, PlanError, Status};

/// Compare an op against what is currently at the destination. A `merge`
/// rule replaces the op's content with the merged result.
pub fn status(dest: &Path, op: &mut Op, keep: &Rules, merge: &MergeRules) -> Result<Status, PlanError> {
    let abs = dest.join(&op.path);
    let io = |source| PlanError::Io { path: abs.display().to_string(), source };
    let meta = match std::fs::symlink_metadata(&abs) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Status::Create),
        Err(e) => return Err(io(e)),
    };
    let ft = meta.file_type();
    let conflict = |what: &str| Status::Conflict { reason: format!("{what} already exists there") };

    // Author intent about existing files comes before any comparison.
    if let Some(rule) = keep.matched(&op.path) {
        return Ok(Status::Kept { rule: rule.to_string() });
    }
    if let OpKind::WriteFile { origin: Origin::Shell { once: true, .. }, .. } = &op.kind {
        return Ok(Status::Kept { rule: "generate once".into() });
    }
    if let (Some((rule, spec)), OpKind::WriteFile { content, .. }) = (merge.matched(&op.path), &mut op.kind) {
        let strategy = spec.strategy;
        let path = op.path.display().to_string();
        let unmergeable = |what| PlanError::Unmergeable { path: path.clone(), rule: rule.to_string(), what };
        if !ft.is_file() {
            return Err(unmergeable(if ft.is_dir() { "a directory" } else { "a symlink" }));
        }
        let Content::Text(rendered) = content else {
            return Err(unmergeable("binary"));
        };
        let existing = std::fs::read(&abs).map_err(io)?;
        let existing_text = std::str::from_utf8(&existing).map_err(|_| unmergeable("not valid UTF-8"))?;
        let merged = crate::merge::merge(spec, existing_text, rendered)
            .map_err(|source| PlanError::Merge { path: path.clone(), strategy, source })?;
        if merged.as_bytes() == existing {
            return Ok(Status::Unchanged);
        }
        *rendered = merged;
        return Ok(Status::Merge { strategy, existing });
    }

    Ok(match &op.kind {
        OpKind::CreateDir => {
            if ft.is_dir() {
                Status::Unchanged
            } else if ft.is_symlink() {
                conflict("a symlink")
            } else {
                conflict("a file")
            }
        }
        OpKind::WriteFile { content, mode, .. } => {
            if ft.is_dir() {
                conflict("a directory")
            } else if ft.is_symlink() {
                conflict("a symlink")
            } else {
                let existing = std::fs::read(&abs).map_err(io)?;
                let same_content = existing == content.as_bytes();
                let same_mode = file_mode(&abs).map_err(io)? == *mode;
                if same_content && same_mode {
                    Status::Unchanged
                } else {
                    Status::Modify { mode_only: same_content, existing }
                }
            }
        }
        OpKind::Symlink { target } => {
            if !ft.is_symlink() {
                conflict(if ft.is_dir() { "a directory" } else { "a file" })
            } else if std::fs::read_link(&abs).map_err(io)? == *target {
                Status::Unchanged
            } else {
                Status::Modify { mode_only: false, existing: Vec::new() }
            }
        }
    })
}
