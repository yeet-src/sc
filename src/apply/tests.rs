use super::*;
use crate::manifest::Manifest;
use crate::plan::ANSWERS_FILE;
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
    let inputs = crate::inputs::resolve(&manifest, &vars).unwrap();
    crate::plan::plan(tpl, Some(dest), &manifest, inputs, &crate::plan::PlanOptions { allow_shell: true, git: None }).unwrap()
}

fn run(tpl: &Path, dest: &Path, existing: Existing) -> Result<Outcome, ApplyError> {
    apply(&plan_for(tpl, dest, &[("name", "demo")]), &Options { existing, hooks: Hooks::Capture }, &mut |_| {})
}

const TPL: &[(&str, &[u8])] = &[
    ("scaffold.toml", b"[template]\nname = \"t\"\n[vars.name]\n"),
    ("{{ name }}/main.txt", b"hi {{ name }}\n"),
    ("empty/.keep", b""),
    ("README.md", b"# {{ name }}\n"),
];

#[test]
fn creates_everything_then_is_idempotent() {
    let tpl = fixture(TPL);
    let dest = tempfile::tempdir().unwrap();
    let out = run(tpl.path(), dest.path(), Existing::Fail).unwrap();
    assert_eq!(out.created.len(), 6, "{out:?}");
    assert_eq!(fs::read_to_string(dest.path().join("demo/main.txt")).unwrap(), "hi demo\n");
    let answers = fs::read_to_string(dest.path().join(ANSWERS_FILE)).unwrap();
    assert!(answers.contains("name = \"demo\""), "{answers}");
    assert!(!fs::read_dir(dest.path()).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().ends_with(".sc-tmp")));

    let again = run(tpl.path(), dest.path(), Existing::Fail).unwrap();
    assert!(again.created.is_empty() && again.modified.is_empty());
    assert_eq!(again.unchanged.len(), 6);
}

#[test]
fn existing_policy() {
    let tpl = fixture(TPL);
    let dest = fixture(&[("README.md", b"mine\n"), ("demo/main.txt", b"hi demo\n")]);

    let err = run(tpl.path(), dest.path(), Existing::Fail).unwrap_err();
    assert!(matches!(err, ApplyError::Blocked(1)), "{err}");
    assert!(!dest.path().join("empty").exists(), "nothing written when blocked");

    let out = run(tpl.path(), dest.path(), Existing::Skip).unwrap();
    assert_eq!(out.skipped.len(), 1);
    assert_eq!(fs::read_to_string(dest.path().join("README.md")).unwrap(), "mine\n");
    assert!(dest.path().join("empty/.keep").exists());

    let out = run(tpl.path(), dest.path(), Existing::Overwrite).unwrap();
    assert_eq!(out.modified, [PathBuf::from("README.md")]);
    assert_eq!(fs::read_to_string(dest.path().join("README.md")).unwrap(), "# demo\n");
}

#[test]
fn conflicts_are_never_overwritten() {
    let tpl = fixture(TPL);
    let dest = fixture(&[("README.md/inner", b"")]);
    let err = run(tpl.path(), dest.path(), Existing::Overwrite).unwrap_err();
    assert!(matches!(err, ApplyError::Conflict(p) if p == Path::new("README.md")));
    let out = run(tpl.path(), dest.path(), Existing::Skip).unwrap();
    assert_eq!(out.skipped[0].0, Path::new("README.md"));
}

#[test]
fn detects_change_since_plan_and_rolls_back() {
    let tpl = fixture(TPL);
    let dest = fixture(&[("README.md", b"old\n")]);
    let plan = plan_for(tpl.path(), dest.path(), &[("name", "demo")]);
    fs::write(dest.path().join("README.md"), "newer\n").unwrap();
    let err = apply(&plan, &Options { existing: Existing::Overwrite, hooks: Hooks::Skip }, &mut |_| {}).unwrap_err();
    assert!(matches!(&err, ApplyError::Changed(p) if p == Path::new("README.md")), "{err}");
    // Ops before README.md in plan order were staged and must be gone.
    let left: Vec<_> = fs::read_dir(dest.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(left, ["README.md"], "{left:?}");
    assert_eq!(fs::read_to_string(dest.path().join("README.md")).unwrap(), "newer\n");
}

#[test]
fn refuses_overlapping_paths() {
    let tpl = fixture(TPL);
    let inside = tpl.path().join("out");
    let err = run(tpl.path(), &inside, Existing::Fail).unwrap_err();
    assert!(matches!(err, ApplyError::Overlap(_)));
}

#[test]
fn merge_writes_and_keep_leaves_alone_regardless_of_policy() {
    let tpl = fixture(&[
        ("scaffold.toml", b"[files]\nkeep = [\"README.md\"]\n[files.merge]\n\".gitignore\" = \"append-lines\"\n"),
        ("README.md", b"template"),
        (".gitignore", b"target/\n"),
    ]);
    let dest = fixture(&[("README.md", b"mine"), (".gitignore", b"node_modules/\n")]);
    let out = run(tpl.path(), dest.path(), Existing::Fail).unwrap();
    assert_eq!(out.modified, [PathBuf::from(".gitignore")]);
    assert_eq!(out.kept, [PathBuf::from("README.md")]);
    assert_eq!(fs::read_to_string(dest.path().join(".gitignore")).unwrap(), "node_modules/\ntarget/\n");
    assert_eq!(fs::read_to_string(dest.path().join("README.md")).unwrap(), "mine");

    let out = run(tpl.path(), dest.path(), Existing::Overwrite).unwrap();
    assert_eq!(out.kept, [PathBuf::from("README.md")]);
    assert_eq!(fs::read_to_string(dest.path().join("README.md")).unwrap(), "mine");
    assert!(out.modified.is_empty(), "second merge is a no-op");
}

#[test]
fn after_hooks_run_in_dest_after_commit() {
    let tpl = fixture(&[
        ("scaffold.toml", b"[vars.name]\n[[hooks]]\nphase = \"after\"\nrun = \"cat a.txt > hooked-$SC_VAR_NAME.txt\"\n[[hooks]]\nphase = \"after\"\nrun = \"exit 4\"\nwhen = \"{{ name == 'fail' }}\"\n"),
        ("a.txt", b"{{ name }}"),
    ]);
    let dest = tempfile::tempdir().unwrap();
    let mut seen = vec![];
    let plan = plan_for(tpl.path(), dest.path(), &[("name", "demo")]);
    let out = apply(&plan, &Options { existing: Existing::Fail, hooks: Hooks::Capture }, &mut |h| seen.push(h.name.clone())).unwrap();
    assert_eq!(out.hooks, ["cat a.txt > hooked-$SC_VAR_NAME.txt"]);
    assert_eq!(seen, out.hooks);
    assert_eq!(fs::read_to_string(dest.path().join("hooked-demo.txt")).unwrap(), "demo");

    let dest = tempfile::tempdir().unwrap();
    let plan = plan_for(tpl.path(), dest.path(), &[("name", "fail")]);
    let err = apply(&plan, &Options { existing: Existing::Fail, hooks: Hooks::Capture }, &mut |_| {}).unwrap_err();
    assert!(matches!(err, ApplyError::Hook { .. }), "{err}");
    assert!(dest.path().join("a.txt").exists(), "files stay written when a hook fails");

    let plan = plan_for(tpl.path(), dest.path(), &[("name", "fail")]);
    let out = apply(&plan, &Options { existing: Existing::Overwrite, hooks: Hooks::Skip }, &mut |_| {}).unwrap();
    assert!(out.hooks.is_empty());
}

#[cfg(unix)]
#[test]
fn preserves_mode_and_symlinks() {
    use std::os::unix::fs::PermissionsExt;
    let tpl = fixture(&[("run.sh", b"#!/bin/sh\n"), ("real", b"r")]);
    fs::set_permissions(tpl.path().join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("real", tpl.path().join("link")).unwrap();
    let dest = tempfile::tempdir().unwrap();
    run(tpl.path(), dest.path(), Existing::Fail).unwrap();
    assert_eq!(fs::metadata(dest.path().join("run.sh")).unwrap().permissions().mode() & 0o777, 0o755);
    assert_eq!(fs::read_link(dest.path().join("link")).unwrap(), Path::new("real"));
    assert_eq!(fs::read_to_string(dest.path().join("link")).unwrap(), "r");
}
