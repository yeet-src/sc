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
#[cfg(test)]
mod tests;

pub use types::*;

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use minijinja::Value;

use crate::inputs::{Inputs, Source};
use crate::manifest::{CONTROL_DIR, Hook, Manifest, Phase};
use crate::render::{Renderer, ShellState};
use crate::shell::{self, Program};

use hooks::{hook_enabled, hook_name, resolve_hook};
use rules::{MergeRules, Renderers, Rules};
use walk::{Planner, looks_binary};

pub use hooks::program;
pub use walk::file_mode;

/// Written into the destination so a later run can see which template and
/// inputs produced it.
pub const ANSWERS_FILE: &str = ".sc.toml";

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
        let out = shell::capture(program(&hook), &hook.name, &ctx).map_err(|source| PlanError::Shell { what: what.clone(), source })?;
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
    let mut planner = Planner {
        root: template_root,
        extra_control: &opts.extra_control,
        renderers: Renderers::new(template_root, manifest, &shell_state)?,
        dot_prefix: &manifest.files.dot_prefix,
        ctx: inputs.context(),
        exclude: Rules::new(&manifest.files.exclude)?,
        raw: Rules::new(&manifest.files.raw)?,
        ops: Vec::new(),
        dropped: Vec::new(),
        warnings: Vec::new(),
    };
    planner.walk(Path::new(""), Path::new(""))?;

    for g in &manifest.files.generate {
        let path = planner.renderers.default.render("generate.path", &g.path, &planner.ctx)?;
        let command = planner.renderers.default.render(&format!("generate {path}"), &g.run, &planner.ctx)?;
        let out = shell::capture(Program::Inline { text: &command, shell: &g.shell }, &command, &shell_state.ctx)
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
    if ops.iter().any(|o| o.path == Path::new(ANSWERS_FILE)) {
        return Err(PlanError::ReservedPath);
    }
    ops.push(Op {
        template_path: PathBuf::from(ANSWERS_FILE),
        path: PathBuf::from(ANSWERS_FILE),
        mount: None,
        kind: OpKind::WriteFile {
            content: Content::Text(answers_file(template_root, manifest, &inputs, opts.git.as_ref())?),
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
            dest: dest.map(Path::to_path_buf),
        },
        inputs,
        ops,
        dropped: planner.dropped,
        warnings: planner.warnings,
        hooks,
        executed,
        mounts: Vec::new(),
    })
}

/// The answers file records provenance and every input, in manifest order,
/// so an update can re-render with the same values. Nothing time-dependent
/// goes in here: the same template and inputs must produce the same bytes.
fn answers_file(
    template_root: &Path,
    manifest: &Manifest,
    inputs: &Inputs,
    git: Option<&crate::source::GitInfo>,
) -> Result<String, toml::ser::Error> {
    let template = match git {
        Some(g) => crate::answers::Template {
            name: manifest.template.name.clone(),
            source: g.url.clone(),
            reference: g.reference.clone(),
            commit: Some(g.commit.clone()),
            subdir: g.subdir.clone(),
        },
        None => crate::answers::Template {
            name: manifest.template.name.clone(),
            source: std::fs::canonicalize(template_root)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| template_root.display().to_string()),
            reference: None,
            commit: None,
            subdir: None,
        },
    };
    crate::answers::Answers {
        template,
        inputs: inputs.0.iter().map(|i| (i.name.clone(), toml_value(&i.value))).collect(),
    }
    .to_string()
}

/// minijinja values serialize through serde; TOML needs a concrete value.
fn toml_value(v: &Value) -> toml::Value {
    toml::Value::try_from(v).unwrap_or_else(|_| toml::Value::String(v.to_string()))
}
