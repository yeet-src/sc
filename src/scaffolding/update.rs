//! Re-apply a newer version of the template a project was generated from.
//!
//! The answers file names the template and the inputs. For a git template
//! it also names the commit, which makes a three-way merge possible: the
//! previous commit rendered with the same inputs is the common ancestor,
//! the file on disk is "ours", and the new render is "theirs". A file the
//! user never touched simply takes the new render; a file the template did
//! not change keeps the user's edits; a file both sides changed is merged,
//! with conflict markers when the hunks overlap.
//!
//! A local-path template has no recorded ancestor, so update degrades to a
//! plain re-plan and the usual `--existing` policy decides.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::scaffolding::answers::{Answers, AnswersError};
use crate::scaffolding::compose::{self, ComposeError, Loaded};
use crate::scaffolding::inputs::{self, Inputs, Source};
use crate::scaffolding::plan::{self, Content, OpKind, Plan, PlanOptions, Status};
use crate::scaffolding::source::{self, GitInfo, SourceError};

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Move to this ref instead of the one recorded in the answers file.
    pub reference: Option<String>,
    /// Inputs to change; everything else comes from the answers file.
    pub vars: Vec<(String, String)>,
    pub allow_shell: bool,
    pub fetch: source::Options,
    /// Stay on the recorded commit: `set` changes inputs, not the template.
    /// A local-path template is re-read either way.
    pub pin: bool,
    /// `vars` is the complete input set (a golden case): recorded answers
    /// are not consulted, missing inputs take manifest defaults.
    pub replace_inputs: bool,
}

pub struct Prepared {
    pub plan: Plan,
    /// The commit the project was last generated from, when known.
    pub previous: Option<String>,
    /// The loaded template, kept alive while the plan is used.
    pub loaded: Loaded,
}

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error(transparent)]
    Answers(#[from] AnswersError),
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error(transparent)]
    Inputs(#[from] inputs::InputError),
    #[error(transparent)]
    Compose(#[from] ComposeError),
    #[error("planning the previous version ({commit}): {source}")]
    Previous {
        commit: String,
        #[source]
        source: Box<UpdateError>,
    },
}

pub async fn prepare(dest: &Path, opts: &Options) -> Result<Prepared, UpdateError> {
    let answers = Answers::load(dest)?;
    let t = &answers.template;
    let src = match &t.commit {
        Some(commit) => source::Source::Git {
            url: t.source.clone(),
            reference: if opts.pin { Some(commit.clone()) } else { opts.reference.clone().or_else(|| t.reference.clone()) },
            subdir: t.subdir.clone(),
        },
        None => source::Source::Local(PathBuf::from(&t.source)),
    };
    let mut loaded = compose::load_resolved(src.resolve(&opts.fetch).await?, &opts.fetch, 0).await?;
    loaded.add_with(&t.with, &opts.fetch).await?;
    let manifest = loaded.combined_manifest();

    // Answers first, then explicit changes; later entries win in `resolve`.
    let mut overrides = if opts.replace_inputs { Vec::new() } else { answers.overrides() };
    overrides.extend(opts.vars.iter().cloned());
    let mut inputs = inputs::resolve(&manifest, &overrides)?;
    for i in &mut inputs.0 {
        if i.source == Source::Flag && !opts.vars.iter().any(|(k, _)| *k == i.name) {
            i.source = Source::Answers;
        }
    }

    let plan_opts = PlanOptions { allow_shell: opts.allow_shell, ..Default::default() };
    let mut plan = loaded.plan(Some(dest), inputs.clone(), &plan_opts)?;

    // The ancestor: what the previous render produced. For a git template
    // that is the recorded commit with the recorded inputs. For a local
    // template there is no recorded version, so the template as it is now
    // with the recorded inputs stands in; exact when only inputs changed.
    let previous = t.commit.clone();
    let old_inputs = inputs::resolve(&manifest, &answers.overrides()).ok();
    let ancestor = match (&previous, &loaded.git, &old_inputs) {
        (Some(old), Some(new), Some(old_inputs)) if *old != new.commit => Some(
            render_previous(new, old, &t.with, old_inputs, opts)
                .await
                .map_err(|e| UpdateError::Previous { commit: old.clone(), source: Box::new(e) })?,
        ),
        (_, _, Some(old_inputs)) => Some(loaded.plan(None, old_inputs.clone(), &PlanOptions { allow_shell: opts.allow_shell, ..Default::default() })?),
        _ => None,
    };
    if let Some(ancestor) = &ancestor {
        reconcile(&mut plan, Some(ancestor));
        let (removals, local_edits) = drift(dest, ancestor, &plan);
        plan.removals = removals;
        plan.local_edits = local_edits;
    }
    Ok(Prepared { plan, previous, loaded })
}

/// What changed on disk relative to the previous render: files it produced
/// that the new render does not and that are unmodified (to remove), and
/// files that differ from what it wrote (local edits).
fn drift(dest: &Path, ancestor: &Plan, plan: &Plan) -> (Vec<plan::Removal>, Vec<PathBuf>) {
    let now: std::collections::HashSet<&Path> = plan.ops.iter().map(|o| o.path.as_path()).collect();
    let mut removals = Vec::new();
    let mut edits = Vec::new();
    for op in &ancestor.ops {
        let OpKind::WriteFile { content, origin, .. } = &op.kind else { continue };
        if matches!(origin, plan::Origin::Generated) {
            continue;
        }
        let Ok(on_disk) = std::fs::read(dest.join(&op.path)) else { continue };
        let untouched = on_disk == content.as_bytes();
        if !untouched {
            edits.push(op.path.clone());
        }
        if !now.contains(op.path.as_path()) && untouched {
            removals.push(plan::Removal { path: op.path.clone(), expected: on_disk });
        }
    }
    (removals, edits)
}

/// Inputs as recorded in the answers file, resolved against the template.
pub fn recorded_inputs(dest: &Path, loaded: &Loaded) -> Result<Inputs, UpdateError> {
    let answers = Answers::load(dest)?;
    Ok(inputs::resolve(&loaded.combined_manifest(), &answers.overrides())?)
}

/// Render the previously used commit with the same inputs, from the mirror
/// that `prepare` just synced.
async fn render_previous(new: &GitInfo, commit: &str, with: &[String], inputs: &Inputs, opts: &Options) -> Result<Plan, UpdateError> {
    let src = source::Source::Git { url: new.url.clone(), reference: Some(commit.to_string()), subdir: new.subdir.clone() };
    let fetch = source::Options { offline: true, ..opts.fetch.clone() };
    let mut loaded = compose::load_resolved(src.resolve(&fetch).await?, &fetch, 0).await?;
    loaded.add_with(with, &fetch).await?;
    let plan_opts = PlanOptions { allow_shell: opts.allow_shell, ..Default::default() };
    Ok(loaded.plan(None, inputs.clone(), &plan_opts)?)
}

/// Turn `Modify` ops into three-way merges against the ancestor render.
/// With no ancestor plan, the current render is the ancestor.
fn reconcile(plan: &mut Plan, ancestor: Option<&Plan>) {
    let old: HashMap<&Path, &str> = ancestor
        .map(|a| {
            a.ops
                .iter()
                .filter_map(|o| match &o.kind {
                    OpKind::WriteFile { content: Content::Text(t), .. } => Some((o.path.as_path(), t.as_str())),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    for op in &mut plan.ops {
        let (Status::Modify { existing, .. }, OpKind::WriteFile { content: Content::Text(theirs), origin, .. }) =
            (&op.status, &mut op.kind)
        else {
            continue;
        };
        if matches!(origin, plan::Origin::Generated) {
            continue;
        }
        let base: &str = match ancestor {
            Some(_) => match old.get(op.path.as_path()) {
                Some(b) => b,
                None => {
                    // Not in the previous render, yet something is on disk
                    // that differs: the user's file. Theirs, as always.
                    op.status = Status::Kept { rule: "edited locally; not in the previous render".into() };
                    continue;
                }
            },
            None => theirs,
        };
        let base = base.to_string();
        let base = base.as_str();
        let Ok(ours) = std::str::from_utf8(existing) else { continue };
        if ours == base {
            // Never touched locally: the new render replaces it outright.
            op.status = Status::Update { conflicts: false, existing: existing.clone() };
            continue;
        }
        if theirs == base {
            op.status = Status::Kept { rule: "edited locally, unchanged in template".into() };
            continue;
        }
        let existing = existing.clone();
        match diffy::merge(base, ours, theirs) {
            Ok(merged) => {
                *theirs = merged;
                op.status = Status::Update { conflicts: false, existing };
            }
            Err(marked) => {
                *theirs = marked;
                op.status = Status::Update { conflicts: true, existing };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scaffolding::apply::{self, Existing, Hooks};
    use std::fs;
    use std::process::Command;

    struct Repo {
        dir: tempfile::TempDir,
    }

    impl Repo {
        fn new() -> Self {
            let r = Self { dir: tempfile::tempdir().unwrap() };
            r.git(&["init", "-q", "-b", "main"]);
            r
        }
        fn git(&self, args: &[&str]) -> String {
            let out = Command::new("git")
                .arg("-C")
                .arg(self.dir.path())
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
        fn commit(&self, files: &[(&str, &str)], msg: &str) -> String {
            for (p, c) in files {
                let path = self.dir.path().join(p);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, c).unwrap();
            }
            self.git(&["add", "-A"]);
            self.git(&["commit", "-qm", msg]);
            self.git(&["rev-parse", "HEAD"])
        }
        fn url(&self) -> String {
            format!("file://{}", self.dir.path().display())
        }
    }

    fn fetch(cache: &Path) -> source::Options {
        source::Options { offline: false, cache_dir: Some(cache.to_path_buf()) }
    }

    async fn apply_first(repo: &Repo, dest: &Path, cache: &Path) {
        let loaded = compose::load(&repo.url(), None, &fetch(cache)).await.unwrap();
        let inputs = inputs::resolve(&loaded.combined_manifest(), &[("name".into(), "demo".into())]).unwrap();
        let plan = loaded.plan(Some(dest), inputs, &PlanOptions::default()).unwrap();
        apply::apply(&plan, &apply::Options { existing: Existing::Fail, hooks: Hooks::Skip }, &mut |_| {}).await.unwrap();
    }

    const V1_MAIN: &str = "fn main() {\n    // setup\n    println!(\"{{ name }}\");\n    // teardown\n}\n";

    #[tokio::test]
    async fn three_way_merge_keeps_user_edits_and_takes_template_changes() {
        let repo = Repo::new();
        let v1 = repo.commit(&[("sc.toml", "[vars.name]\n"), ("src/main.rs", V1_MAIN), ("untouched.txt", "same\n"), ("mine.txt", "v1\n")], "v1");
        let cache = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        apply_first(&repo, dest.path(), cache.path()).await;
        let answers = Answers::load(dest.path()).unwrap();
        assert_eq!(answers.template.commit.as_deref(), Some(v1.as_str()));

        // User edits the setup line; template later changes the teardown line and a file the user edited elsewhere.
        fs::write(dest.path().join("src/main.rs"), V1_MAIN.replace("// setup", "// my setup").replace("{{ name }}", "demo")).unwrap();
        fs::write(dest.path().join("mine.txt"), "edited by user\n").unwrap();
        let v2 = repo.commit(
            &[("src/main.rs", &V1_MAIN.replace("// teardown", "// new teardown")), ("untouched.txt", "changed\n"), ("new.txt", "n\n")],
            "v2",
        );

        let prepared = prepare(dest.path(), &Options { fetch: fetch(cache.path()), ..Default::default() }).await.unwrap();
        assert_eq!(prepared.previous.as_deref(), Some(v1.as_str()));
        assert_eq!(prepared.plan.local_edits, [PathBuf::from("mine.txt"), PathBuf::from("src/main.rs")]);
        assert_eq!(prepared.plan.template.git.as_ref().unwrap().commit, v2);
        let status = |p: &str| &prepared.plan.ops.iter().find(|o| o.path == Path::new(p)).unwrap().status;
        let text = |p: &str| match &prepared.plan.ops.iter().find(|o| o.path == Path::new(p)).unwrap().kind {
            OpKind::WriteFile { content: Content::Text(t), .. } => t.clone(),
            _ => panic!(),
        };
        assert!(matches!(status("src/main.rs"), Status::Update { conflicts: false, .. }));
        assert_eq!(text("src/main.rs"), "fn main() {\n    // my setup\n    println!(\"demo\");\n    // new teardown\n}\n");
        assert!(matches!(status("untouched.txt"), Status::Update { conflicts: false, .. }), "user never touched it");
        assert!(matches!(status("mine.txt"), Status::Kept { .. }), "template did not change it");
        assert_eq!(*status("new.txt"), Status::Create);
        assert!(prepared.plan.inputs.0.iter().all(|i| i.source == Source::Answers));

        let out = apply::apply(&prepared.plan, &apply::Options { existing: Existing::Fail, hooks: Hooks::Skip }, &mut |_| {}).await.unwrap();
        assert!(out.modified.iter().any(|p| p == Path::new("src/main.rs")));
        assert_eq!(fs::read_to_string(dest.path().join("mine.txt")).unwrap(), "edited by user\n");
        assert_eq!(Answers::load(dest.path()).unwrap().template.commit.as_deref(), Some(v2.as_str()));

        // A second update is a no-op.
        let again = prepare(dest.path(), &Options { fetch: fetch(cache.path()), ..Default::default() }).await.unwrap();
        assert!(again.plan.ops.iter().all(|o| matches!(o.status, Status::Unchanged | Status::Kept { .. })), "{:?}", again.plan.ops.iter().map(|o| (&o.path, &o.status)).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn overlapping_edits_conflict_and_need_overwrite() {
        let repo = Repo::new();
        repo.commit(&[("sc.toml", "[vars.name]\n"), ("f.txt", "line\n")], "v1");
        let cache = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        apply_first(&repo, dest.path(), cache.path()).await;
        fs::write(dest.path().join("f.txt"), "user line\n").unwrap();
        repo.commit(&[("f.txt", "template line\n")], "v2");

        let prepared = prepare(dest.path(), &Options { fetch: fetch(cache.path()), ..Default::default() }).await.unwrap();
        let op = prepared.plan.ops.iter().find(|o| o.path == Path::new("f.txt")).unwrap();
        assert!(matches!(op.status, Status::Update { conflicts: true, .. }));
        let OpKind::WriteFile { content: Content::Text(t), .. } = &op.kind else { panic!() };
        assert!(t.contains("<<<<<<<") && t.contains("user line") && t.contains("template line"), "{t}");

        let err = apply::apply(&prepared.plan, &apply::Options { existing: Existing::Fail, hooks: Hooks::Skip }, &mut |_| {}).await.unwrap_err();
        assert!(matches!(err, apply::ApplyError::Blocked(1)));
        let out = apply::apply(&prepared.plan, &apply::Options { existing: Existing::Overwrite, hooks: Hooks::Skip }, &mut |_| {}).await.unwrap();
        assert!(out.modified.iter().any(|p| p == Path::new("f.txt")));
        assert!(fs::read_to_string(dest.path().join("f.txt")).unwrap().contains(">>>>>>>"));
    }

    #[tokio::test]
    async fn set_removes_untouched_dropped_files_and_keeps_edited_ones() {
        let repo = Repo::new();
        repo.commit(
            &[
                ("sc.toml", "[vars.name]\n[vars.ci]\ntype = \"bool\"\ndefault = true\n[tree]\n\".github/\" = { when = \"ci\" }\n"),
                (".github/workflows/ci.yml", "on: push\n"),
                (".github/CODEOWNERS", "* @me\n"),
                ("f.txt", "{{ name }}\n"),
            ],
            "v1",
        );
        let cache = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        apply_first(&repo, dest.path(), cache.path()).await;
        fs::write(dest.path().join(".github/CODEOWNERS"), "* @me @you\n").unwrap();

        let opts = Options { vars: vec![("ci".into(), "false".into())], pin: true, fetch: fetch(cache.path()), ..Default::default() };
        let prepared = prepare(dest.path(), &opts).await.unwrap();
        assert_eq!(prepared.plan.removals.iter().map(|r| r.path.clone()).collect::<Vec<_>>(), [PathBuf::from(".github/workflows/ci.yml")]);
        assert_eq!(prepared.plan.local_edits, [PathBuf::from(".github/CODEOWNERS")]);
        let out = apply::apply(&prepared.plan, &apply::Options { existing: Existing::Fail, hooks: Hooks::Skip }, &mut |_| {}).await.unwrap();
        assert_eq!(out.removed, [PathBuf::from(".github/workflows/ci.yml")]);
        assert!(!dest.path().join(".github/workflows").exists(), "emptied directory is removed");
        assert!(dest.path().join(".github/CODEOWNERS").exists(), "edited file stays");
        assert!(Answers::load(dest.path()).unwrap().inputs["ci"] == toml::Value::Boolean(false));

        // Flip it back: the workflow returns, the edit is untouched.
        let opts = Options { vars: vec![("ci".into(), "true".into())], pin: true, fetch: fetch(cache.path()), ..Default::default() };
        let prepared = prepare(dest.path(), &opts).await.unwrap();
        assert!(prepared.plan.removals.is_empty());
        apply::apply(&prepared.plan, &apply::Options { existing: Existing::Fail, hooks: Hooks::Skip }, &mut |_| {}).await.unwrap();
        assert_eq!(fs::read_to_string(dest.path().join(".github/workflows/ci.yml")).unwrap(), "on: push\n");
        assert_eq!(fs::read_to_string(dest.path().join(".github/CODEOWNERS")).unwrap(), "* @me @you\n");
    }

    #[tokio::test]
    async fn ref_override_and_input_changes() {
        let repo = Repo::new();
        repo.commit(&[("sc.toml", "[vars.name]\n"), ("f.txt", "{{ name }}\n")], "v1");
        repo.git(&["tag", "v1"]);
        let cache = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        apply_first(&repo, dest.path(), cache.path()).await;
        repo.commit(&[("f.txt", "{{ name }}!\n")], "v2");

        let opts = Options { reference: Some("v1".into()), vars: vec![("name".into(), "renamed".into())], fetch: fetch(cache.path()), ..Default::default() };
        let prepared = prepare(dest.path(), &opts).await.unwrap();
        assert_eq!(prepared.plan.template.git.as_ref().unwrap().reference.as_deref(), Some("v1"));
        let op = prepared.plan.ops.iter().find(|o| o.path == Path::new("f.txt")).unwrap();
        let OpKind::WriteFile { content: Content::Text(t), .. } = &op.kind else { panic!() };
        assert_eq!(t, "renamed\n");
        let name = prepared.plan.inputs.0.iter().find(|i| i.name == "name").unwrap();
        assert_eq!(name.source, Source::Flag);
    }
}
