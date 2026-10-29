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

use std::path::{Path, PathBuf};

use crate::scaffolding::plan::{Plan, PlannedHook};
use crate::scaffolding::shell::ShellError;

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
    /// Files deleted because the render no longer produces them.
    pub removed: Vec<PathBuf>,
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
///
/// Staging and commit are synchronous local filesystem work done inline;
/// hooks run as async subprocesses.
pub async fn apply(plan: &Plan, opts: &Options, on_hook: &mut dyn FnMut(&PlannedHook)) -> Result<Outcome, ApplyError> {
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

    // Dropped files go only if they still hold what the previous render
    // wrote; anything edited since stays, like every other local edit.
    for r in &plan.removals {
        let abs = dest.join(&r.path);
        match std::fs::read(&abs) {
            Ok(bytes) if bytes == r.expected => {
                std::fs::remove_file(&abs).map_err(|source| ApplyError::Io { path: abs.clone(), source })?;
                outcome.removed.push(r.path.clone());
                let mut dir = abs.parent();
                while let Some(d) = dir.filter(|d| *d != dest) {
                    if std::fs::remove_dir(d).is_err() {
                        break; // not empty, or not ours
                    }
                    dir = d.parent();
                }
            }
            _ => {}
        }
    }

    hooks::run_after(plan, dest, opts.hooks, on_hook, &mut outcome).await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scaffolding::manifest::Manifest;
    use crate::scaffolding::plan::ANSWERS_FILE;
    use std::fs;

    fn fixture(files: &[(&str, &[u8])]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (rel, bytes) in files {
            let p = dir.path().join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, bytes).unwrap();
        }
        dir
    }

    fn plan_for(tpl: &Path, dest: &Path, vars: &[(&str, &str)]) -> Plan {
        let manifest = Manifest::load(tpl).unwrap();
        let vars: Vec<_> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let inputs = crate::scaffolding::inputs::resolve(&manifest, &vars).unwrap();
        crate::scaffolding::plan::plan(tpl, Some(dest), &manifest, inputs, &crate::scaffolding::plan::PlanOptions { allow_shell: true, ..Default::default() }).unwrap()
    }

    async fn run(tpl: &Path, dest: &Path, existing: Existing) -> Result<Outcome, ApplyError> {
        apply(&plan_for(tpl, dest, &[("name", "demo")]), &Options { existing, hooks: Hooks::Capture }, &mut |_| {}).await
    }

    const TPL: &[(&str, &[u8])] = &[
        ("sc.toml", b"[template]\nname = \"t\"\n[vars.name]\n"),
        ("{{ name }}/main.txt", b"hi {{ name }}\n"),
        ("empty/.keep", b""),
        ("README.md", b"# {{ name }}\n"),
    ];

    #[tokio::test]
    async fn creates_everything_then_is_idempotent() {
        let tpl = fixture(TPL);
        let dest = tempfile::tempdir().unwrap();
        let out = run(tpl.path(), dest.path(), Existing::Fail).await.unwrap();
        assert_eq!(out.created.len(), 6, "{out:?}");
        assert_eq!(fs::read_to_string(dest.path().join("demo/main.txt")).unwrap(), "hi demo\n");
        let answers = fs::read_to_string(dest.path().join(ANSWERS_FILE)).unwrap();
        assert!(answers.contains("name = \"demo\""), "{answers}");
        assert!(!fs::read_dir(dest.path()).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().ends_with(".sc-tmp")));

        let again = run(tpl.path(), dest.path(), Existing::Fail).await.unwrap();
        assert!(again.created.is_empty() && again.modified.is_empty());
        assert_eq!(again.unchanged.len(), 6);
    }

    #[tokio::test]
    async fn existing_policy() {
        let tpl = fixture(TPL);
        let dest = fixture(&[("README.md", b"mine\n"), ("demo/main.txt", b"hi demo\n")]);

        let err = run(tpl.path(), dest.path(), Existing::Fail).await.unwrap_err();
        assert!(matches!(err, ApplyError::Blocked(1)), "{err}");
        assert!(!dest.path().join("empty").exists(), "nothing written when blocked");

        let out = run(tpl.path(), dest.path(), Existing::Skip).await.unwrap();
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(fs::read_to_string(dest.path().join("README.md")).unwrap(), "mine\n");
        assert!(dest.path().join("empty/.keep").exists());

        let out = run(tpl.path(), dest.path(), Existing::Overwrite).await.unwrap();
        assert_eq!(out.modified, [PathBuf::from("README.md")]);
        assert_eq!(fs::read_to_string(dest.path().join("README.md")).unwrap(), "# demo\n");
    }

    #[tokio::test]
    async fn conflicts_are_never_overwritten() {
        let tpl = fixture(TPL);
        let dest = fixture(&[("README.md/inner", b"")]);
        let err = run(tpl.path(), dest.path(), Existing::Overwrite).await.unwrap_err();
        assert!(matches!(err, ApplyError::Conflict(p) if p == Path::new("README.md")));
        let out = run(tpl.path(), dest.path(), Existing::Skip).await.unwrap();
        assert_eq!(out.skipped[0].0, Path::new("README.md"));
    }

    #[tokio::test]
    async fn detects_change_since_plan_and_rolls_back() {
        let tpl = fixture(TPL);
        let dest = fixture(&[("README.md", b"old\n")]);
        let plan = plan_for(tpl.path(), dest.path(), &[("name", "demo")]);
        fs::write(dest.path().join("README.md"), "newer\n").unwrap();
        let err = apply(&plan, &Options { existing: Existing::Overwrite, hooks: Hooks::Skip }, &mut |_| {}).await.unwrap_err();
        assert!(matches!(&err, ApplyError::Changed(p) if p == Path::new("README.md")), "{err}");
        // Ops before README.md in plan order were staged and must be gone.
        let left: Vec<_> = fs::read_dir(dest.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left, ["README.md"], "{left:?}");
        assert_eq!(fs::read_to_string(dest.path().join("README.md")).unwrap(), "newer\n");
    }

    #[tokio::test]
    async fn refuses_overlapping_paths() {
        let tpl = fixture(TPL);
        let inside = tpl.path().join("out");
        let err = run(tpl.path(), &inside, Existing::Fail).await.unwrap_err();
        assert!(matches!(err, ApplyError::Overlap(_)));
    }

    #[tokio::test]
    async fn merge_writes_and_keep_leaves_alone_regardless_of_policy() {
        let tpl = fixture(&[
            ("sc.toml", b"[files]\nkeep = [\"README.md\"]\n[files.merge]\n\".gitignore\" = \"append-lines\"\n"),
            ("README.md", b"template"),
            (".gitignore", b"target/\n"),
        ]);
        let dest = fixture(&[("README.md", b"mine"), (".gitignore", b"node_modules/\n")]);
        let out = run(tpl.path(), dest.path(), Existing::Fail).await.unwrap();
        assert_eq!(out.modified, [PathBuf::from(".gitignore")]);
        assert_eq!(out.kept, [PathBuf::from("README.md")]);
        assert_eq!(fs::read_to_string(dest.path().join(".gitignore")).unwrap(), "node_modules/\ntarget/\n");
        assert_eq!(fs::read_to_string(dest.path().join("README.md")).unwrap(), "mine");

        let out = run(tpl.path(), dest.path(), Existing::Overwrite).await.unwrap();
        assert_eq!(out.kept, [PathBuf::from("README.md")]);
        assert_eq!(fs::read_to_string(dest.path().join("README.md")).unwrap(), "mine");
        assert!(out.modified.is_empty(), "second merge is a no-op");
    }

    #[tokio::test]
    async fn after_hooks_run_in_dest_after_commit() {
        let tpl = fixture(&[
            ("sc.toml", b"[vars.name]\n[[hooks]]\nphase = \"after\"\nrun = \"cat a.txt > hooked-$SC_VAR_NAME.txt\"\n[[hooks]]\nphase = \"after\"\nrun = \"exit 4\"\nwhen = \"{{ name == 'fail' }}\"\n"),
            ("a.txt", b"{{ name }}"),
        ]);
        let dest = tempfile::tempdir().unwrap();
        let mut seen = vec![];
        let plan = plan_for(tpl.path(), dest.path(), &[("name", "demo")]);
        let out = apply(&plan, &Options { existing: Existing::Fail, hooks: Hooks::Capture }, &mut |h| seen.push(h.name.clone())).await.unwrap();
        assert_eq!(out.hooks, ["cat a.txt > hooked-$SC_VAR_NAME.txt"]);
        assert_eq!(seen, out.hooks);
        assert_eq!(fs::read_to_string(dest.path().join("hooked-demo.txt")).unwrap(), "demo");

        let dest = tempfile::tempdir().unwrap();
        let plan = plan_for(tpl.path(), dest.path(), &[("name", "fail")]);
        let err = apply(&plan, &Options { existing: Existing::Fail, hooks: Hooks::Capture }, &mut |_| {}).await.unwrap_err();
        assert!(matches!(err, ApplyError::Hook { .. }), "{err}");
        assert!(dest.path().join("a.txt").exists(), "files stay written when a hook fails");

        let plan = plan_for(tpl.path(), dest.path(), &[("name", "fail")]);
        let out = apply(&plan, &Options { existing: Existing::Overwrite, hooks: Hooks::Skip }, &mut |_| {}).await.unwrap();
        assert!(out.hooks.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn preserves_mode_and_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let tpl = fixture(&[("run.sh", b"#!/bin/sh\n"), ("real", b"r")]);
        fs::set_permissions(tpl.path().join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("real", tpl.path().join("link")).unwrap();
        let dest = tempfile::tempdir().unwrap();
        run(tpl.path(), dest.path(), Existing::Fail).await.unwrap();
        assert_eq!(fs::metadata(dest.path().join("run.sh")).unwrap().permissions().mode() & 0o777, 0o755);
        assert_eq!(fs::read_link(dest.path().join("link")).unwrap(), Path::new("real"));
        assert_eq!(fs::read_to_string(dest.path().join("link")).unwrap(), "r");
    }
}
