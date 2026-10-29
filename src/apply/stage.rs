//! Staging and commit: every write goes to a temporary sibling first, then
//! everything is renamed into place. Staging can be rolled back; commit
//! reports how far it got if a rename fails.

use std::path::{Path, PathBuf};

use super::decide::Decision;
use super::{ApplyError, Outcome};
use crate::plan::{Content, Op, OpKind, Status};

/// Everything staging has done, so it can be undone or committed.
#[derive(Default)]
pub struct Stage {
    /// Directories this run created, in creation order.
    pub created_dirs: Vec<PathBuf>,
    /// (temporary, final) pairs awaiting rename.
    pub pending: Vec<(PathBuf, PathBuf)>,
}

impl Stage {
    pub fn rollback(self) {
        for (tmp, _) in &self.pending {
            let _ = std::fs::remove_file(tmp);
        }
        for dir in self.created_dirs.iter().rev() {
            let _ = std::fs::remove_dir(dir); // only succeeds if empty, which is the point
        }
    }

    pub fn commit(self) -> Result<(), ApplyError> {
        let total = self.pending.len();
        for (committed, (tmp, path)) in self.pending.iter().enumerate() {
            std::fs::rename(tmp, path).map_err(|source| ApplyError::Partial {
                path: path.clone(),
                source,
                committed,
                total,
            })?;
        }
        Ok(())
    }
}

pub fn stage_all(dest: &Path, decisions: &[Decision], stage: &mut Stage, outcome: &mut Outcome) -> Result<(), ApplyError> {
    for d in decisions {
        match d {
            Decision::Unchanged(op) => outcome.unchanged.push(op.path.clone()),
            Decision::Skip(op, why) => outcome.skipped.push((op.path.clone(), why.clone())),
            Decision::Keep(op) => outcome.kept.push(op.path.clone()),
            Decision::Write(op) => {
                let abs = dest.join(&op.path);
                check_precondition(&abs, op)?;
                match &op.kind {
                    OpKind::CreateDir => {
                        create_dirs(&abs, stage)?;
                    }
                    OpKind::WriteFile { content, mode, .. } => {
                        if let Some(parent) = abs.parent() {
                            create_dirs(parent, stage)?;
                        }
                        let tmp = temp_path(&abs);
                        write_file(&tmp, content, *mode)?;
                        stage.pending.push((tmp, abs));
                    }
                    OpKind::Symlink { target } => {
                        if let Some(parent) = abs.parent() {
                            create_dirs(parent, stage)?;
                        }
                        let tmp = temp_path(&abs);
                        make_symlink(target, &tmp)?;
                        stage.pending.push((tmp, abs));
                    }
                }
                match op.status {
                    Status::Create => outcome.created.push(op.path.clone()),
                    _ => outcome.modified.push(op.path.clone()),
                }
            }
        }
    }
    Ok(())
}

/// The destination must still be as the plan saw it.
fn check_precondition(abs: &Path, op: &Op) -> Result<(), ApplyError> {
    let io = |source| ApplyError::Io { path: abs.to_path_buf(), source };
    let changed = || ApplyError::Changed(op.path.clone());
    match (&op.status, std::fs::symlink_metadata(abs)) {
        (Status::Create, Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        (Status::Create, Err(e)) => Err(io(e)),
        (Status::Create, Ok(_)) => Err(changed()),
        (Status::Modify { existing, .. } | Status::Merge { existing, .. } | Status::Update { existing, .. }, Ok(meta)) => {
            match &op.kind {
                OpKind::WriteFile { .. } if meta.is_file() => {
                    if std::fs::read(abs).map_err(io)? == *existing { Ok(()) } else { Err(changed()) }
                }
                OpKind::Symlink { .. } if meta.is_symlink() => Ok(()),
                _ => Err(changed()),
            }
        }
        (Status::Modify { .. } | Status::Merge { .. } | Status::Update { .. }, Err(_)) => Err(changed()),
        // Only Create, Modify and Merge reach the writer.
        (Status::Unchanged | Status::Conflict { .. } | Status::Kept { .. }, _) => Ok(()),
    }
}

/// `create_dir_all` that remembers which directories it actually made.
fn create_dirs(path: &Path, stage: &mut Stage) -> Result<(), ApplyError> {
    let mut missing = Vec::new();
    let mut cur = Some(path);
    while let Some(p) = cur {
        if p.exists() || p.as_os_str().is_empty() {
            break;
        }
        missing.push(p.to_path_buf());
        cur = p.parent();
    }
    for dir in missing.into_iter().rev() {
        std::fs::create_dir(&dir).map_err(|source| ApplyError::Io { path: dir.clone(), source })?;
        stage.created_dirs.push(dir);
    }
    Ok(())
}

fn temp_path(abs: &Path) -> PathBuf {
    let name = abs.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    abs.with_file_name(format!(".{name}.{}.sc-tmp", std::process::id()))
}

fn write_file(tmp: &Path, content: &Content, mode: u32) -> Result<(), ApplyError> {
    let io = |source| ApplyError::Io { path: tmp.to_path_buf(), source };
    std::fs::write(tmp, content.as_bytes()).map_err(io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp, std::fs::Permissions::from_mode(mode)).map_err(io)?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    Ok(())
}

#[cfg(unix)]
fn make_symlink(target: &Path, tmp: &Path) -> Result<(), ApplyError> {
    std::os::unix::fs::symlink(target, tmp).map_err(|source| ApplyError::Io { path: tmp.to_path_buf(), source })
}

#[cfg(not(unix))]
fn make_symlink(_target: &Path, tmp: &Path) -> Result<(), ApplyError> {
    Err(ApplyError::Symlink(tmp.to_path_buf()))
}
