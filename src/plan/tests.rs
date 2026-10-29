use super::*;
use crate::manifest::MergeStrategy;
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

fn run(dir: &Path, dest: Option<&Path>, overrides: &[(&str, &str)]) -> Result<Plan, PlanError> {
    let manifest = Manifest::load(dir).unwrap();
    let overrides: Vec<_> = overrides.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let inputs = crate::inputs::resolve(&manifest, &overrides).unwrap();
    plan(dir, dest, &manifest, inputs, &PlanOptions::default())
}

fn run_shell(dir: &Path, dest: Option<&Path>, overrides: &[(&str, &str)]) -> Result<Plan, PlanError> {
    let manifest = Manifest::load(dir).unwrap();
    let overrides: Vec<_> = overrides.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let inputs = crate::inputs::resolve(&manifest, &overrides).unwrap();
    plan(dir, dest, &manifest, inputs, &PlanOptions { allow_shell: true, git: None })
}

fn paths(plan: &Plan) -> Vec<String> {
    plan.ops.iter().map(|o| o.path.display().to_string()).collect()
}

#[test]
fn renders_paths_and_contents_in_sorted_order() {
    let dir = fixture(&[
        ("scaffold.toml", b"[vars.name]\ndefault = \"My App\"\n"),
        ("{{ name | kebab_case }}/b.txt", b"hello {{ name }}\n"),
        ("{{ name | kebab_case }}/a.txt", b"x"),
        ("README.md", b"# {{ name | title_case }}\n"),
    ]);
    let plan = run(dir.path(), None, &[]).unwrap();
    assert_eq!(paths(&plan), [".scaffold.toml", "README.md", "my-app", "my-app/a.txt", "my-app/b.txt"]);
    assert_eq!(plan.ops[4].template_path, Path::new("{{ name | kebab_case }}/b.txt"));
    match &plan.ops[4].kind {
        OpKind::WriteFile { content: Content::Text(t), origin: Origin::Rendered, .. } => {
            assert_eq!(t, "hello My App\n")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(plan.dropped, [Dropped { template_path: "scaffold.toml".into(), reason: DropReason::Control }]);
}

#[test]
fn empty_path_component_drops_subtree() {
    let dir = fixture(&[
        ("scaffold.toml", b"[vars.ci]\ntype = \"bool\"\ndefault = false\n"),
        ("{% if ci %}.github{% endif %}/workflows/ci.yml", b"{{ undefined_var }}"),
        ("keep.txt", b"k"),
    ]);
    let plan = run(dir.path(), None, &[]).unwrap();
    assert_eq!(paths(&plan), [".scaffold.toml", "keep.txt"]);
    assert!(plan.dropped.iter().any(|d| d.reason == DropReason::EmptyPath));
    let err = run(dir.path(), None, &[("ci", "true")]).unwrap_err();
    assert!(matches!(err, PlanError::Render(_)), "subtree is rendered once enabled");
}

#[test]
fn raw_and_binary_are_copied_verbatim() {
    let dir = fixture(&[
        ("scaffold.toml", b"[files]\nraw = [\"charts/**\"]\nexclude = [\"*.orig\"]\n"),
        ("charts/values.yaml", b"x: {{ .Values.x }}\n"),
        ("logo.png", b"\x89PNG\0\0"),
        ("notes.orig", b"{{ broken"),
    ]);
    let plan = run(dir.path(), None, &[]).unwrap();
    assert_eq!(paths(&plan), [".scaffold.toml", "charts", "charts/values.yaml", "logo.png"]);
    assert!(matches!(&plan.ops[2].kind, OpKind::WriteFile { origin: Origin::Raw { rule }, .. } if rule == "charts/**"));
    assert!(matches!(&plan.ops[3].kind, OpKind::WriteFile { origin: Origin::Binary, .. }));
    assert!(plan.dropped.iter().any(|d| d.reason == DropReason::Excluded { rule: "*.orig".into() }));
}

#[test]
fn dot_prefix_includes_and_custom_syntax() {
    let dir = fixture(&[
        ("scaffold.toml", b"[[files.syntax]]\npath = \"charts/**\"\nvariable = [\"[[\", \"]]\"]\n"),
        (".scaffold/partials/hdr", b"# {{ n }}\n"),
        ("dot_github/dot_keep", b""),
        ("dot_", b"not renamed: nothing after the prefix"),
        ("README.md", b"{% include 'partials/hdr' %}body\n"),
        ("charts/v.yaml", b"x: {{ .Values.x }} [[ n ]]\n"),
    ]);
    let plan = run(dir.path(), None, &[("n", "N")]).unwrap();
    assert_eq!(
        paths(&plan),
        [".github", ".github/.keep", ".scaffold.toml", "README.md", "charts", "charts/v.yaml", "dot_"]
    );
    let text = |p: &str| match &plan.ops.iter().find(|o| o.path == Path::new(p)).unwrap().kind {
        OpKind::WriteFile { content: Content::Text(t), .. } => t.clone(),
        _ => panic!(),
    };
    assert_eq!(text("README.md"), "# N\nbody\n");
    assert_eq!(text("charts/v.yaml"), "x: {{ .Values.x }} N\n");
    assert!(plan.dropped.iter().any(|d| d.template_path == Path::new(".scaffold") && d.reason == DropReason::Control));
}

#[test]
fn hooks_and_generate_require_allow_shell() {
    let dir = fixture(&[
        ("scaffold.toml", b"[[hooks]]\nphase = \"before\"\nrun = \"echo '{\\\"from_hook\\\": 7}'\"\n\n[[hooks]]\nphase = \"after\"\nrun = \"echo done\"\nwhen = \"{{ ci }}\"\n\n[[files.generate]]\npath = \"gen/{{ n }}.txt\"\nrun = \"printf 'made %s' \\\"$SC_VAR_N\\\"\"\nonce = true\n"),
        ("a.txt", b"{{ from_hook }} {{ shell('echo sh') }}\n"),
    ]);
    let err = run(dir.path(), None, &[("n", "x"), ("ci", "false")]).unwrap_err();
    let msg = err.to_string();
    assert!(matches!(err, PlanError::ShellNotAllowed { .. }));
    assert!(msg.contains("before hook: echo") && msg.contains("generate gen/{{ n }}.txt"), "{msg}");

    let plan = run_shell(dir.path(), None, &[("n", "x"), ("ci", "false")]).unwrap();
    assert_eq!(plan.inputs.0.iter().find(|i| i.name == "from_hook").map(|i| i.source), Some(Source::Hook));
    let text = |p: &str| match &plan.ops.iter().find(|o| o.path == Path::new(p)).unwrap().kind {
        OpKind::WriteFile { content: Content::Text(t), .. } => t.clone(),
        _ => panic!(),
    };
    assert_eq!(text("a.txt"), "7 sh\n");
    assert_eq!(text("gen/x.txt"), "made x");
    assert!(plan.hooks.is_empty(), "after hook disabled by when");
    let purposes: Vec<_> = plan.executed.iter().map(|e| e.purpose.as_str()).collect();
    assert_eq!(purposes, ["before hook", "generate gen/x.txt", "shell()"]);

    let plan = run_shell(dir.path(), None, &[("n", "x"), ("ci", "yes")]).unwrap();
    assert_eq!(plan.hooks.len(), 1);
    assert_eq!(plan.hooks[0].text, "echo done");

    let dest = fixture(&[("gen/x.txt", b"old")]);
    let plan = run_shell(dir.path(), Some(dest.path()), &[("n", "x"), ("ci", "no")]).unwrap();
    let generated = plan.ops.iter().find(|o| o.path == Path::new("gen/x.txt")).unwrap();
    assert_eq!(generated.status, Status::Kept { rule: "generate once".into() });
}

#[test]
fn rendered_path_cannot_escape() {
    let dir = fixture(&[("{{ p }}", b"")]);
    let err = run(dir.path(), None, &[("p", "../evil")]).unwrap_err();
    assert!(matches!(err, PlanError::BadPath { .. }), "{err}");
}

#[test]
fn warns_on_leftover_syntax_and_empty_output() {
    let dir = fixture(&[
        ("a.txt", b"{{ '{{' }} literal\n"),
        ("b.txt", b"{% if false %}gone{% endif %}"),
    ]);
    let plan = run(dir.path(), None, &[]).unwrap();
    let msgs: Vec<_> = plan.warnings.iter().map(|w| (w.path.display().to_string(), w.message.as_str())).collect();
    assert_eq!(
        msgs,
        [
            ("a.txt".to_string(), "rendered output still contains template syntax"),
            ("b.txt".to_string(), "renders to an empty file"),
        ]
    );
}

#[test]
fn status_against_destination() {
    let tpl = fixture(&[
        ("same.txt", b"same\n"),
        ("changed.txt", b"new\n"),
        ("new.txt", b"n"),
        ("dir/x", b"x"),
        ("clash", b"i am a file"),
    ]);
    let dest = fixture(&[
        ("same.txt", b"same\n"),
        ("changed.txt", b"old\n"),
        ("dir/other", b"o"),
        ("clash/inner", b"i am a directory"),
    ]);
    let plan = run(tpl.path(), Some(dest.path()), &[]).unwrap();
    let by_path = |p: &str| &plan.ops.iter().find(|o| o.path == Path::new(p)).unwrap().status;
    assert_eq!(*by_path("same.txt"), Status::Unchanged);
    assert_eq!(*by_path("new.txt"), Status::Create);
    assert_eq!(*by_path("dir"), Status::Unchanged);
    assert_eq!(*by_path("dir/x"), Status::Create);
    assert!(matches!(by_path("changed.txt"), Status::Modify { mode_only: false, existing } if existing == b"old\n"));
    assert!(matches!(by_path("clash"), Status::Conflict { .. }));
}

#[test]
fn merge_and_keep_rules() {
    let tpl = fixture(&[
        (
            "scaffold.toml",
            b"[files]\nkeep = [\"README.md\", \"config/*\"]\n[files.merge]\n\"Cargo.toml\" = \"toml\"\n\".gitignore\" = \"append-lines\"\n",
        ),
        ("README.md", b"template readme"),
        ("config/app.toml", b"x = 1"),
        ("Cargo.toml", b"[dependencies]\nclap = \"4\"\n"),
        (".gitignore", b"target/\n"),
        ("same.toml", b"a = 1\n"),
    ]);
    let dest = fixture(&[
        ("README.md", b"my readme"),
        ("config/other.toml", b""),
        ("Cargo.toml", b"[package]\nname = \"mine\"\n\n[dependencies]\nserde = \"1\"\n"),
        (".gitignore", b"target/\n"),
    ]);
    let plan = run(tpl.path(), Some(dest.path()), &[]).unwrap();
    let find = |p: &str| plan.ops.iter().find(|o| o.path == Path::new(p)).unwrap();

    assert_eq!(find("README.md").status, Status::Kept { rule: "README.md".into() });
    assert_eq!(find("config/app.toml").status, Status::Create, "keep only applies once the file exists");
    assert_eq!(find(".gitignore").status, Status::Unchanged, "merge producing identical bytes is a no-op");

    let cargo = find("Cargo.toml");
    assert!(matches!(&cargo.status, Status::Merge { strategy: MergeStrategy::Toml, .. }));
    let OpKind::WriteFile { content: Content::Text(t), .. } = &cargo.kind else { panic!() };
    assert_eq!(t, "[package]\nname = \"mine\"\n\n[dependencies]\nserde = \"1\"\nclap = \"4\"\n");

    fs::write(dest.path().join("Cargo.toml"), "broken = = =").unwrap();
    let err = run(tpl.path(), Some(dest.path()), &[]).unwrap_err();
    assert!(matches!(err, PlanError::Merge { .. }), "{err}");
}

#[cfg(unix)]
#[test]
fn preserves_executable_bit_and_detects_mode_only_change() {
    use std::os::unix::fs::PermissionsExt;
    let tpl = fixture(&[("run.sh", b"#!/bin/sh\n")]);
    fs::set_permissions(tpl.path().join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    let plan = run(tpl.path(), None, &[]).unwrap();
    assert!(matches!(&plan.ops[1].kind, OpKind::WriteFile { mode: 0o755, .. }));

    let dest = fixture(&[("run.sh", b"#!/bin/sh\n")]);
    fs::set_permissions(dest.path().join("run.sh"), fs::Permissions::from_mode(0o644)).unwrap();
    let plan = run(tpl.path(), Some(dest.path()), &[]).unwrap();
    assert!(matches!(&plan.ops[1].status, Status::Modify { mode_only: true, .. }));
}

#[cfg(unix)]
#[test]
fn symlink_outside_destination_warns() {
    let tpl = fixture(&[("a/b/keep", b"")]);
    std::os::unix::fs::symlink("../../../etc", tpl.path().join("a/b/esc")).unwrap();
    std::os::unix::fs::symlink("../ok", tpl.path().join("a/b/fine")).unwrap();
    let plan = run(tpl.path(), None, &[]).unwrap();
    assert_eq!(plan.warnings.len(), 1);
    assert_eq!(plan.warnings[0].path, Path::new("a/b/esc"));
}

#[test]
fn serializes_to_json() {
    let dir = fixture(&[("scaffold.toml", b"[vars.n]\ndefault = \"v\"\n"), ("f", b"{{ n }}"), ("b", b"\0")]);
    let plan = run(dir.path(), None, &[]).unwrap();
    let OpKind::WriteFile { content: Content::Text(answers), .. } = &plan.ops[0].kind else { panic!() };
    assert!(answers.contains("[inputs]\nn = \"v\"\n"), "{answers}");
    let json = serde_json::to_value(&plan).unwrap();
    assert_eq!(json["inputs"][0]["source"], "default");
    assert_eq!(json["ops"][0]["kind"]["origin"]["type"], "generated");
    assert_eq!(json["ops"][1]["kind"]["content"]["binary_size"], 1);
    assert_eq!(json["ops"][2]["kind"]["content"]["text"], "v");
    assert_eq!(json["ops"][2]["status"]["type"], "create");
    assert_eq!(json["dropped"][0]["reason"]["type"], "control");
}
