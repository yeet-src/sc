//! The planner: a pure function from (template directory, inputs, optional
//! destination) to an ordered list of filesystem operations, each annotated
//! with what it would do to the destination. Nothing here writes to disk.
//!
//! Classification of each template entry, in order:
//! 1. the manifest file at the template root is a control file and is skipped;
//! 2. entries matching an `exclude` glob are skipped, subtrees included;
//! 3. each path component is rendered; a component that renders to the empty
//!    string drops the entry (and its subtree), which is how conditional
//!    files and directories work;
//! 4. file contents are copied verbatim when they match a `raw` glob or look
//!    binary, and rendered otherwise.
//!
//! Directory entries are visited in byte order of their names so the plan is
//! deterministic for a given template and set of inputs.
mod hooks;
mod rules;
mod status;
mod types;
mod walk;

pub use types::*;

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use minijinja::Value;

use crate::scaffolding::inputs::{Inputs, Source};
use crate::scaffolding::manifest::{CONTROL_DIR, Hook, Manifest, Phase};
use crate::scaffolding::render::{Renderer, ShellState};
use crate::scaffolding::shell::{self, Program};

use hooks::{hook_enabled, hook_name, resolve_hook};
use rules::{MergeRules, Renderers, Rules};
use walk::{Planner, looks_binary};

pub use hooks::program;
pub use walk::file_mode;

/// Recompute an op's status against the destination under `manifest`'s keep
/// and merge rules. Used after composition changes an op's content.
pub fn restatus(dest: &Path, op: &mut Op, manifest: &Manifest) -> Result<(), PlanError> {
    let keep = Rules::new(&manifest.files.keep)?;
    let merge = MergeRules::new(&manifest.files.merge)?;
    op.status = status::status(dest, op, &keep, &merge)?;
    Ok(())
}

/// Written into the destination so a later run can see which template and
/// inputs produced it.
pub const ANSWERS_FILE: &str = ".sc.toml";

/// Synchronous and CPU-bound apart from any commands the template runs.
/// Embedders on an async runtime should call it under `spawn_blocking`.
pub fn plan(
    template_root: &Path,
    dest: Option<&Path>,
    manifest: &Manifest,
    mut inputs: Inputs,
    opts: &PlanOptions,
) -> Result<Plan, PlanError> {
    let control = template_root.join(CONTROL_DIR);
    let mut executed = Vec::new();

    // Before hooks run first, since they may add inputs. Their `when` is
    // evaluated against the inputs as given.
    let plain = Renderer::new(&control);
    let before: Vec<&Hook> = manifest
        .hooks
        .iter()
        .filter(|h| h.phase == Phase::Before)
        .map(|h| hook_enabled(h, &plain, &inputs.context()).map(|on| on.then_some(h)))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();
    if !opts.allow_shell {
        let mut commands: Vec<String> = before.iter().map(|h| format!("before hook: {}", hook_name(h))).collect();
        commands.extend(manifest.files.generate.iter().map(|g| format!("generate {}: {}", g.path, g.run)));
        if !commands.is_empty() {
            return Err(PlanError::ShellNotAllowed { commands });
        }
    }
    for h in before {
        let ctx = shell::Context::new(template_root, &inputs, template_root, dest);
        let hook = resolve_hook(h, template_root, &plain, &inputs.context(), &inputs)?;
        let what = format!("before hook `{}`", hook.name);
        let out = shell::capture_blocking(program(&hook), &hook.name, &ctx).map_err(|source| PlanError::Shell { what: what.clone(), source })?;
        executed.push(Executed { purpose: "before hook".into(), command: hook.name.clone() });
        if !out.trim().is_empty() {
            let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&out)
                .map_err(|e| PlanError::HookOutput { name: hook.name.clone(), detail: e.to_string() })?;
            for (k, v) in map {
                inputs.set(&k, Value::from_serialize(&v), Source::Hook);
            }
        }
    }

    let shell_state = Arc::new(ShellState {
        allowed: opts.allow_shell,
        ctx: shell::Context::new(template_root, &inputs, template_root, dest),
        log: Mutex::new(Vec::new()),
    });
    let variant_groups = manifest.variant_groups();
    let variant_outputs: Vec<PathBuf> = variant_groups.iter().map(|(k, _)| PathBuf::from(k)).collect();
    let mut planner = Planner {
        root: template_root,
        extra_control: &opts.extra_control,
        renderers: Renderers::new(template_root, manifest, &shell_state)?,
        dot_prefix: &manifest.files.dot_prefix,
        rename: &manifest.files.rename,
        variant_outputs: &variant_outputs,
        ctx: inputs.context(),
        exclude: Rules::new(&manifest.files.exclude)?,
        raw: Rules::new(&manifest.files.raw)?,
        raw_out: Rules::new(&manifest.files.raw_out)?,
        ops: Vec::new(),
        dropped: Vec::new(),
        warnings: Vec::new(),
    };
    planner.walk(Path::new(""), Path::new(""))?;
    for (key, var) in &variant_groups {
        planner.variant(key, var)?;
    }

    for g in &manifest.files.generate {
        let path = planner.renderers.default.render("generate.path", &g.path, &planner.ctx)?;
        let command = planner.renderers.default.render(&format!("generate {path}"), &g.run, &planner.ctx)?;
        let out = shell::capture_blocking(Program::Inline { text: &command, shell: &g.shell }, &command, &shell_state.ctx)
            .map_err(|source| PlanError::Shell { what: format!("generate {path}"), source })?;
        executed.push(Executed { purpose: format!("generate {path}"), command: command.clone() });
        let out_rel = PathBuf::from(&path);
        if out_rel.components().any(|c| !matches!(c, Component::Normal(_))) {
            return Err(PlanError::BadPath { template_path: g.path.clone(), rendered: path, reason: "is not a plain relative path" });
        }
        planner.ops.push(Op {
            template_path: PathBuf::from(format!("generate: {}", g.run)),
            path: out_rel,
            mount: None,
            kind: OpKind::WriteFile {
                content: if looks_binary(out.as_bytes()) { Content::Bytes(out.into_bytes()) } else { Content::Text(out) },
                mode: 0o644,
                origin: Origin::Shell { command, once: g.once },
            },
            status: Status::Create,
        });
    }

    // After hooks, rendered with the final inputs; listed so the user can
    // see them before confirming.
    let mut hooks = Vec::new();
    for h in &manifest.hooks {
        if h.phase == Phase::After && hook_enabled(h, &planner.renderers.default, &planner.ctx)? {
            hooks.push(resolve_hook(h, template_root, &planner.renderers.default, &planner.ctx, &inputs)?);
        }
    }
    executed.extend(
        shell_state.log.lock().unwrap().drain(..).map(|c| Executed { purpose: "shell()".into(), command: c }),
    );

    let mut ops = planner.ops;

    // `[tree]` conditions: evaluate each once, then drop every op whose
    // output path a false one covers.
    let mut off: Vec<(Rules, String)> = Vec::new();
    for (globs, expr) in manifest.conditions() {
        if !condition_holds(&planner.renderers.default, &expr, &planner.ctx)? {
            off.push((Rules::new(&globs)?, expr));
        }
    }
    if !off.is_empty() {
        let (kept, dropped): (Vec<Op>, Vec<Op>) =
            ops.into_iter().partition(|o| !off.iter().any(|(r, _)| r.matched(&o.path).is_some()));
        ops = kept;
        for o in dropped {
            let rule = off.iter().find(|(r, _)| r.matched(&o.path).is_some()).map(|(_, e)| e.clone()).unwrap_or_default();
            planner.dropped.push(Dropped { template_path: o.template_path, reason: DropReason::When { rule } });
        }
    }

    if ops.iter().any(|o| o.path == Path::new(ANSWERS_FILE)) {
        return Err(PlanError::ReservedPath);
    }
    ops.push(Op {
        template_path: PathBuf::from(ANSWERS_FILE),
        path: PathBuf::from(ANSWERS_FILE),
        mount: None,
        kind: OpKind::WriteFile {
            content: Content::Text(answers_file(template_root, manifest, &inputs, opts.git.as_ref(), &opts.with)?),
            mode: 0o644,
            origin: Origin::Generated,
        },
        status: Status::Create,
    });

    // Walk order is by template name; present by output path. `Path`'s
    // ordering is component-wise, so a directory still precedes its contents.
    ops.sort_by(|a, b| a.path.cmp(&b.path));

    if let Some(dest) = dest {
        let keep = Rules::new(&manifest.files.keep)?;
        let merge = MergeRules::new(&manifest.files.merge)?;
        for op in &mut ops {
            op.status = status::status(dest, op, &keep, &merge)?;
        }
    }

    Ok(Plan {
        template: TemplateInfo {
            name: manifest.template.name.clone(),
            description: manifest.template.description.clone(),
            source: template_root.to_path_buf(),
            git: opts.git.clone(),
            with: opts.with.clone(),
            dest: dest.map(Path::to_path_buf),
        },
        inputs,
        ops,
        dropped: planner.dropped,
        warnings: planner.warnings,
        hooks,
        executed,
        mounts: Vec::new(),
        removals: Vec::new(),
        local_edits: Vec::new(),
    })
}

/// Evaluate a `when` expression against the inputs: Jinja truthiness.
pub fn condition_holds(renderer: &Renderer, expr: &str, ctx: &Value) -> Result<bool, PlanError> {
    let out = renderer.render(&format!("when `{expr}`"), &format!("{{% if {expr} %}}1{{% endif %}}"), ctx)?;
    Ok(!out.is_empty())
}

/// The answers file records provenance and every input, in manifest order,
/// so an update can re-render with the same values. Nothing time-dependent
/// goes in here: the same template and inputs must produce the same bytes.
fn answers_file(
    template_root: &Path,
    manifest: &Manifest,
    inputs: &Inputs,
    git: Option<&crate::scaffolding::source::GitInfo>,
    with: &[String],
) -> Result<String, toml::ser::Error> {
    let template = match git {
        Some(g) => crate::scaffolding::answers::Template {
            name: manifest.template.name.clone(),
            source: g.url.clone(),
            reference: g.reference.clone(),
            commit: Some(g.commit.clone()),
            subdir: g.subdir.clone(),
            with: with.to_vec(),
        },
        None => crate::scaffolding::answers::Template {
            name: manifest.template.name.clone(),
            source: std::fs::canonicalize(template_root)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| template_root.display().to_string()),
            reference: None,
            commit: None,
            subdir: None,
            with: with.to_vec(),
        },
    };
    crate::scaffolding::answers::Answers {
        template,
        inputs: inputs.0.iter().map(|i| (i.name.clone(), toml_value(&i.value))).collect(),
    }
    .to_string()
}

/// minijinja values serialize through serde; TOML needs a concrete value.
fn toml_value(v: &Value) -> toml::Value {
    toml::Value::try_from(v).unwrap_or_else(|_| toml::Value::String(v.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scaffolding::manifest::MergeStrategy;
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
        let inputs = crate::scaffolding::inputs::resolve(&manifest, &overrides).unwrap();
        plan(dir, dest, &manifest, inputs, &PlanOptions::default())
    }

    fn run_shell(dir: &Path, dest: Option<&Path>, overrides: &[(&str, &str)]) -> Result<Plan, PlanError> {
        let manifest = Manifest::load(dir).unwrap();
        let overrides: Vec<_> = overrides.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let inputs = crate::scaffolding::inputs::resolve(&manifest, &overrides).unwrap();
        plan(dir, dest, &manifest, inputs, &PlanOptions { allow_shell: true, ..Default::default() })
    }

    fn paths(plan: &Plan) -> Vec<String> {
        plan.ops.iter().map(|o| o.path.display().to_string()).collect()
    }

    #[test]
    fn renders_paths_and_contents_in_sorted_order() {
        let dir = fixture(&[
            ("sc.toml", b"[vars.name]\ndefault = \"My App\"\n"),
            ("{{ name | kebab_case }}/b.txt", b"hello {{ name }}\n"),
            ("{{ name | kebab_case }}/a.txt", b"x"),
            ("README.md", b"# {{ name | title_case }}\n"),
        ]);
        let plan = run(dir.path(), None, &[]).unwrap();
        assert_eq!(paths(&plan), [".sc.toml", "README.md", "my-app", "my-app/a.txt", "my-app/b.txt"]);
        assert_eq!(plan.ops[4].template_path, Path::new("{{ name | kebab_case }}/b.txt"));
        match &plan.ops[4].kind {
            OpKind::WriteFile { content: Content::Text(t), origin: Origin::Rendered, .. } => {
                assert_eq!(t, "hello My App\n")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(plan.dropped, [Dropped { template_path: "sc.toml".into(), reason: DropReason::Control }]);
    }

    #[test]
    fn empty_path_component_drops_subtree() {
        let dir = fixture(&[
            ("sc.toml", b"[vars.ci]\ntype = \"bool\"\ndefault = false\n"),
            ("{% if ci %}.github{% endif %}/workflows/ci.yml", b"{{ undefined_var }}"),
            ("keep.txt", b"k"),
        ]);
        let plan = run(dir.path(), None, &[]).unwrap();
        assert_eq!(paths(&plan), [".sc.toml", "keep.txt"]);
        assert!(plan.dropped.iter().any(|d| d.reason == DropReason::EmptyPath));
        let err = run(dir.path(), None, &[("ci", "true")]).unwrap_err();
        assert!(matches!(err, PlanError::Render(_)), "subtree is rendered once enabled");
    }

    #[test]
    fn raw_and_binary_are_copied_verbatim() {
        let dir = fixture(&[
            ("sc.toml", b"[files]\nraw = [\"charts/**\"]\nexclude = [\"*.orig\"]\n"),
            ("charts/values.yaml", b"x: {{ .Values.x }}\n"),
            ("logo.png", b"\x89PNG\0\0"),
            ("notes.orig", b"{{ broken"),
        ]);
        let plan = run(dir.path(), None, &[]).unwrap();
        assert_eq!(paths(&plan), [".sc.toml", "charts", "charts/values.yaml", "logo.png"]);
        assert!(matches!(&plan.ops[2].kind, OpKind::WriteFile { origin: Origin::Raw { rule }, .. } if rule == "charts/**"));
        assert!(matches!(&plan.ops[3].kind, OpKind::WriteFile { origin: Origin::Binary, .. }));
        assert!(plan.dropped.iter().any(|d| d.reason == DropReason::Excluded { rule: "*.orig".into() }));
    }

    #[test]
    fn dot_prefix_includes_and_custom_syntax() {
        let dir = fixture(&[
            ("sc.toml", b"[files]\ndot_prefix = \"_\"\n[[files.syntax]]\npath = \"charts/**\"\nvariable = [\"[[\", \"]]\"]\n"),
            (".sc/partials/hdr", b"# {{ n }}\n"),
            ("_github/_keep", b""),
            ("_", b"not renamed: nothing after the prefix"),
            ("README.md", b"{% include 'partials/hdr' %}body\n"),
            ("charts/v.yaml", b"x: {{ .Values.x }} [[ n ]]\n"),
        ]);
        let plan = run(dir.path(), None, &[("n", "N")]).unwrap();
        assert_eq!(
            paths(&plan),
            [".github", ".github/.keep", ".sc.toml", "README.md", "_", "charts", "charts/v.yaml"]
        );
        let text = |p: &str| match &plan.ops.iter().find(|o| o.path == Path::new(p)).unwrap().kind {
            OpKind::WriteFile { content: Content::Text(t), .. } => t.clone(),
            _ => panic!(),
        };
        assert_eq!(text("README.md"), "# N\nbody\n");
        assert_eq!(text("charts/v.yaml"), "x: {{ .Values.x }} N\n");
        assert!(plan.dropped.iter().any(|d| d.template_path == Path::new(".sc") && d.reason == DropReason::Control));
    }

    #[test]
    fn hooks_and_generate_require_allow_shell() {
        let dir = fixture(&[
            ("sc.toml", b"[[hooks]]\nphase = \"before\"\nrun = \"echo '{\\\"from_hook\\\": 7}'\"\n\n[[hooks]]\nphase = \"after\"\nrun = \"echo done\"\nwhen = \"{{ ci }}\"\n\n[[files.generate]]\npath = \"gen/{{ n }}.txt\"\nrun = \"printf 'made %s' \\\"$SC_VAR_N\\\"\"\nonce = true\n"),
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
    fn variant_groups_pick_by_input_value() {
        let dir = fixture(&[
            ("sc.toml", b"[vars.public]\ntype = \"bool\"\ndefault = true\n[vars.env]\ntype = \"choice\"\nchoices = [\"dev\", \"prod\", \"test\"]\ndefault = \"dev\"\n[vars.runtime]\ndefault = \"node\"\n[tree]\n\"README.md\" = { variants = \"public\" }\n\"Dockerfile\" = { variants = \"runtime\" }\n\"config/\" = { variants = \"env\" }\n\"config/secrets.yaml\" = { variants = \"public\" }\n"),
            (".sc/variants/README.md/true.md", b"# public {{ env }}\n"),
            (".sc/variants/Dockerfile/node.Dockerfile", b"FROM node\n"),
            (".sc/variants/Dockerfile/python.Dockerfile", b"FROM python\n"),
            (".sc/variants/config/dev/app.yaml", b"debug: true\n"),
            (".sc/variants/config/prod/app.yaml", b"debug: false\n"),
            (".sc/variants/config/secrets.yaml/false.yaml", b"hidden\n"),
            ("plain.txt", b"p"),
        ]);
        // The working tree holds a checked-out symlink, which the planner ignores.
        std::os::unix::fs::symlink(".sc/variants/README.md/true.md", dir.path().join("README.md")).unwrap();

        let plan = run(dir.path(), None, &[]).unwrap();
        assert_eq!(paths(&plan), [".sc.toml", "Dockerfile", "README.md", "config", "config/app.yaml", "plain.txt"]);
        let text = |plan: &Plan, p: &str| match &plan.ops.iter().find(|o| o.path == Path::new(p)).unwrap().kind {
            OpKind::WriteFile { content: Content::Text(t), .. } => t.clone(),
            _ => panic!("{p}"),
        };
        assert_eq!(text(&plan, "README.md"), "# public dev\n");
        assert_eq!(text(&plan, "Dockerfile"), "FROM node\n");
        assert_eq!(plan.ops.iter().find(|o| o.path == Path::new("Dockerfile")).unwrap().template_path, Path::new(".sc/variants/Dockerfile/node.Dockerfile"));
        assert!(!plan.ops.iter().any(|o| matches!(o.kind, OpKind::Symlink { .. })), "the checkout link is not emitted");
        assert!(plan.dropped.iter().any(|d| matches!(&d.reason, DropReason::NoVariant { var, value, available } if var == "public" && value == "true" && available == &["false"])));

        let plan = run(dir.path(), None, &[("public", "false"), ("env", "prod"), ("runtime", "ruby")]).unwrap();
        assert_eq!(paths(&plan), [".sc.toml", "config", "config/app.yaml", "config/secrets.yaml", "plain.txt"]);
        assert_eq!(text(&plan, "config/app.yaml"), "debug: false\n");
        assert_eq!(text(&plan, "config/secrets.yaml"), "hidden\n");
        assert!(plan.dropped.iter().any(|d| matches!(&d.reason, DropReason::NoVariant { var, available, .. } if var == "runtime" && available == &["node", "python"])));

        let bad = fixture(&[("sc.toml", b"[tree]\n\"x\" = { variants = \"nope\" }\n"), (".sc/variants/x/a", b"")]);
        let err = run(bad.path(), None, &[]).unwrap_err();
        assert!(matches!(err, PlanError::UnknownVariantInput { .. }), "{err}");
        let missing = fixture(&[("sc.toml", b"[vars.v]\ndefault = \"a\"\n[tree]\n\"x\" = { variants = \"v\" }\n")]);
        assert!(matches!(run(missing.path(), None, &[]).unwrap_err(), PlanError::NoVariantStore { .. }));
    }

    #[test]
    fn tree_conditions_and_rules() {
        let dir = fixture(&[
            ("sc.toml", b"[vars.ci]\ntype = \"bool\"\ndefault = true\n[vars.deploy]\ndefault = \"none\"\n[tree]\n\".github/\" = { when = \"ci\" }\n\"Dockerfile\" = { when = \"deploy == 'container'\" }\n\"Makefile\" = { when = \"deploy != 'container'\" }\n\"README.md\" = { keep = true }\n\"Cargo.toml\" = { merge = \"toml\" }\n\"assets/**\" = { raw = true }\n"),
            (".github/workflows/ci.yml", b"on: push\n"),
            ("Dockerfile", b"FROM x\n"),
            ("Makefile", b"all:\n"),
            ("README.md", b"r"),
            ("Cargo.toml", b"[package]\n"),
            ("assets/logo.svg", b"<svg>{{ not rendered }}</svg>"),
        ]);
        let plan = run(dir.path(), None, &[]).unwrap();
        assert_eq!(paths(&plan), [".github", ".github/workflows", ".github/workflows/ci.yml", ".sc.toml", "Cargo.toml", "Makefile", "README.md", "assets", "assets/logo.svg"]);
        assert!(plan.dropped.iter().any(|d| d.template_path == Path::new("Dockerfile") && matches!(&d.reason, DropReason::When { rule } if rule == "deploy == 'container'")));
        assert!(matches!(&plan.ops.iter().find(|o| o.path == Path::new("assets/logo.svg")).unwrap().kind, OpKind::WriteFile { origin: Origin::Raw { .. }, .. }));

        let plan = run(dir.path(), None, &[("ci", "false"), ("deploy", "container")]).unwrap();
        assert_eq!(paths(&plan), [".sc.toml", "Cargo.toml", "Dockerfile", "README.md", "assets", "assets/logo.svg"]);

        let m = Manifest::load(dir.path()).unwrap();
        assert!(m.files.keep.contains(&"README.md".to_string()));
        assert!(m.files.merge.contains_key("Cargo.toml"));
        let bad = fixture(&[("sc.toml", b"[tree]\n\"x\" = { marker = \"m\" }\n")]);
        assert!(Manifest::load(bad.path()).is_err());
    }

    #[test]
    fn rename_map_overrides_names() {
        let dir = fixture(&[
            ("sc.toml", b"[files]\ndot_prefix = \"_\"\n[files.rename]\n\"gitignore\" = \".gitignore\"\n\"github/\" = \".github/\"\n\"_keep\" = \"plain\"\n"),
            ("gitignore", b"target/\n"),
            ("github/workflows/ci.yml", b"on: push\n"),
            ("_keep", b""),
            ("_other", b""),
        ]);
        let plan = run(dir.path(), None, &[]).unwrap();
        assert_eq!(
            paths(&plan),
            [".github", ".github/workflows", ".github/workflows/ci.yml", ".gitignore", ".other", ".sc.toml", "plain"]
        );
        let bad = fixture(&[("sc.toml", b"[files.rename]\n\"x\" = \"../y\"\n")]);
        assert!(Manifest::load(bad.path()).is_err());
    }

    #[test]
    fn git_directory_is_control() {
        let dir = fixture(&[(".git/HEAD", b"ref: refs/heads/main\n"), ("f", b"")]);
        let plan = run(dir.path(), None, &[]).unwrap();
        assert_eq!(paths(&plan), [".sc.toml", "f"]);
        assert!(plan.dropped.iter().any(|d| d.template_path == Path::new(".git") && d.reason == DropReason::Control));
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
                "sc.toml",
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
        let dir = fixture(&[("sc.toml", b"[vars.n]\ndefault = \"v\"\n"), ("f", b"{{ n }}"), ("b", b"\0")]);
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
}
