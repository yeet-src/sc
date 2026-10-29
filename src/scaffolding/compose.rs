//! Loading a template together with the templates it `uses` and `mounts`,
//! and planning them as one.
//!
//! `uses` layers whole templates at the same root: bases are planned first,
//! depth-first in declaration order, and a later layer's op replaces an
//! earlier one at the same path. `mounts` place sub-templates at a path
//! inside this one, once or once per element of a list input; the outer
//! template wins where paths collide.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;

use minijinja::Value;

use crate::scaffolding::inputs::{self, InputError, Inputs};
use crate::scaffolding::manifest::{CONTROL_DIR, Manifest, ManifestError, MergeSpec, Mount};
use crate::scaffolding::merge::{self, MergeError};
use crate::scaffolding::plan::{self, ANSWERS_FILE, Content, MountInfo, Op, OpKind, Plan, PlanError, PlanOptions, Status};
use crate::scaffolding::render::{RenderError, Renderer};
use crate::scaffolding::source::{self, GitInfo, Resolved, Source, SourceError};

const MAX_DEPTH: usize = 8;

/// A template with everything it refers to already fetched, so planning
/// needs no network. Loading is async; planning is synchronous.
#[derive(Debug)]
pub struct Loaded {
    pub root: PathBuf,
    pub manifest: Manifest,
    pub git: Option<GitInfo>,
    pub bases: Vec<Loaded>,
    /// Templates layered under this one from the command line, after
    /// `bases`. Recorded in the answers file.
    pub with: Vec<Loaded>,
    pub with_specs: Vec<String>,
    /// One per `[[mounts]]` entry, in order.
    pub mounted: Vec<Loaded>,
    pub depth: usize,
    #[allow(dead_code)] // keeps a git checkout alive
    resolved: Resolved,
}

#[derive(Debug, thiserror::Error)]
pub enum ComposeError {
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Render(#[from] RenderError),
    #[error("templates nest deeper than {MAX_DEPTH}; is there a `uses` or `mounts` cycle?")]
    TooDeep,
    #[error("in template used from {parent}: {source}")]
    InBase {
        parent: PathBuf,
        #[source]
        source: Box<ComposeError>,
    },
    #[error("mount `{path}`: {source}")]
    InMount {
        path: String,
        #[source]
        source: Box<ComposeError>,
    },
    #[error("mount `{path}`: inputs: {source}")]
    MountInputs {
        path: String,
        #[source]
        source: InputError,
    },
    #[error("mount `{path}`: `for = \"{var}\"` is not a list")]
    ForNotList { path: String, var: String },
    #[error("mount path rendered to `{0}`, which is not a plain relative path")]
    BadMountPath(String),
    #[error("in template given with --with {spec}: {source}")]
    InWith {
        spec: String,
        #[source]
        source: Box<ComposeError>,
    },
    #[error("merging {path} across layers: {source}")]
    LayerMerge {
        path: PathBuf,
        #[source]
        source: MergeError,
    },
}

pub async fn load(spec: &str, subdir: Option<PathBuf>, fetch: &source::Options) -> Result<Loaded, ComposeError> {
    load_resolved(Source::parse(spec, subdir).resolve(fetch).await?, fetch, 0).await
}

/// Load `spec` with extra templates layered under it, as if its manifest
/// listed them in `[[uses]]` after its own entries.
pub async fn load_with(
    spec: &str,
    subdir: Option<PathBuf>,
    with: &[String],
    fetch: &source::Options,
) -> Result<Loaded, ComposeError> {
    let mut loaded = load(spec, subdir, fetch).await?;
    loaded.add_with(with, fetch).await?;
    Ok(loaded)
}

pub fn load_resolved(
    resolved: Resolved,
    fetch: &source::Options,
    depth: usize,
) -> Pin<Box<dyn Future<Output = Result<Loaded, ComposeError>> + Send + '_>> {
    Box::pin(async move {
        if depth > MAX_DEPTH {
            return Err(ComposeError::TooDeep);
        }
        let root = resolved.root.clone();
        let manifest = Manifest::load(&root)?;
        let mut bases = Vec::new();
        for u in &manifest.uses {
            let base = load_child(&root, &u.source, u.subdir.clone(), fetch, depth)
                .await
                .map_err(|e| ComposeError::InBase { parent: root.clone(), source: Box::new(e) })?;
            bases.push(base);
        }
        let mut mounted = Vec::new();
        for m in &manifest.mounts {
            let sub = load_child(&root, &m.template, m.subdir.clone(), fetch, depth)
                .await
                .map_err(|e| ComposeError::InMount { path: m.path.clone(), source: Box::new(e) })?;
            mounted.push(sub);
        }
        Ok(Loaded { root, manifest, git: resolved.git.clone(), bases, with: vec![], with_specs: vec![], mounted, depth, resolved })
    })
}

async fn load_child(
    root: &Path,
    spec: &str,
    subdir: Option<PathBuf>,
    fetch: &source::Options,
    depth: usize,
) -> Result<Loaded, ComposeError> {
    let resolved = relative_to(root, spec, subdir).resolve(fetch).await?;
    load_resolved(resolved, fetch, depth + 1).await
}

/// A relative local spec is relative to the template that names it.
fn relative_to(root: &Path, spec: &str, subdir: Option<PathBuf>) -> Source {
    match Source::parse(spec, subdir) {
        Source::Local(p) if p.is_relative() => Source::Local(root.join(p)),
        other => other,
    }
}

impl Loaded {
    /// Layer `specs` under this template. Relative paths are relative to the
    /// working directory, since they come from the command line.
    pub async fn add_with(&mut self, specs: &[String], fetch: &source::Options) -> Result<(), ComposeError> {
        for spec in specs {
            let layer = load(spec, None, fetch)
                .await
                .map_err(|e| ComposeError::InWith { spec: spec.clone(), source: Box::new(e) })?;
            self.with.push(layer);
            self.with_specs.push(spec.clone());
        }
        Ok(())
    }

    /// The merge rule this template (or one of its layers, nearest first)
    /// declares for an output path.
    fn merge_spec_for(&self, path: &Path) -> Option<MergeSpec> {
        for (glob, rule) in &self.manifest.files.merge {
            if globset::Glob::new(glob).ok()?.compile_matcher().is_match(path) {
                return Some(rule.spec());
            }
        }
        self.with.iter().rev().chain(self.bases.iter().rev()).find_map(|l| l.merge_spec_for(path))
    }

    /// This template's manifest with every layer's keep and merge rules
    /// added (nearest layer first, first match wins), for re-checking a
    /// folded op against the destination.
    fn rules_manifest(&self) -> Manifest {
        let mut m = self.manifest.clone();
        for layer in self.with.iter().rev().chain(self.bases.iter().rev()) {
            let lm = layer.rules_manifest();
            for (k, v) in lm.files.merge {
                m.files.merge.entry(k).or_insert(v);
            }
            for k in lm.files.keep {
                if !m.files.keep.contains(&k) {
                    m.files.keep.push(k);
                }
            }
        }
        m
    }

    /// Manifest to resolve inputs against: every layer's variables.
    pub fn combined_manifest(&self) -> Manifest {
        let mut all = Vec::new();
        self.collect_manifests(&mut all);
        let own = all.pop().expect("self is last");
        own.with_base_vars(all)
    }

    fn collect_manifests<'a>(&'a self, out: &mut Vec<&'a Manifest>) {
        for b in self.bases.iter().chain(&self.with) {
            b.collect_manifests(out);
        }
        out.push(&self.manifest);
    }

    /// Directories inside this template that are mounted sub-templates and
    /// must not be walked as content.
    pub fn local_mount_dirs(&self) -> Vec<PathBuf> {
        self.manifest
            .mounts
            .iter()
            .filter_map(|m| match Source::parse(&m.template, None) {
                Source::Local(p) if p.is_relative() => Some(p),
                _ => None,
            })
            .collect()
    }

    /// Plan every layer and every mount, folded into one plan.
    pub fn plan(&self, dest: Option<&Path>, inputs: Inputs, opts: &PlanOptions) -> Result<Plan, ComposeError> {
        let mut layers = Vec::new();
        for b in self.bases.iter().chain(&self.with) {
            layers.push((b, b.plan(dest, inputs.clone(), opts)?));
        }
        let own_opts = PlanOptions {
            allow_shell: opts.allow_shell,
            git: self.git.clone(),
            extra_control: self.local_mount_dirs(),
            with: self.with_specs.clone(),
        };
        let mut top = plan::plan(&self.root, dest, &self.manifest, inputs.clone(), &own_opts)?;

        // Bases first, each later layer folded over the earlier ones path by
        // path: merged where a merge rule covers the path, replaced otherwise.
        let mut ops: Vec<Op> = Vec::new();
        let mut base_hooks = Vec::new();
        for (layer, plan) in layers {
            for op in plan.ops.into_iter().filter(|o| o.path != Path::new(ANSWERS_FILE)) {
                fold(&mut ops, op, layer, dest)?;
            }
            top.dropped.splice(0..0, plan.dropped);
            top.warnings.splice(0..0, plan.warnings);
            base_hooks.extend(plan.hooks);
            top.executed.splice(0..0, plan.executed);
            top.mounts.splice(0..0, plan.mounts);
        }
        for op in std::mem::take(&mut top.ops) {
            fold(&mut ops, op, self, dest)?;
        }

        // Mounts go under this template's ops: the outer template wins.
        let mut mount_hooks = Vec::new();
        for (m, sub) in self.manifest.mounts.iter().zip(&self.mounted) {
            if let Some(when) = &m.when {
                let renderer = Renderer::new(&self.root.join(CONTROL_DIR));
                if !plan::condition_holds(&renderer, when, &inputs.context())? {
                    continue;
                }
            }
            self.expand_mount(m, sub, dest, &inputs, opts, &mut ops, &mut top, &mut mount_hooks)
                .map_err(|e| ComposeError::InMount { path: m.path.clone(), source: Box::new(e) })?;
        }

        ops.sort_by(|a, b| a.path.cmp(&b.path));
        top.ops = ops;
        let own_hooks = std::mem::take(&mut top.hooks);
        top.hooks = base_hooks.into_iter().chain(mount_hooks).chain(own_hooks).collect();
        Ok(top)
    }

    #[allow(clippy::too_many_arguments)]
    fn expand_mount(
        &self,
        m: &Mount,
        sub: &Loaded,
        dest: Option<&Path>,
        inputs: &Inputs,
        opts: &PlanOptions,
        ops: &mut Vec<Op>,
        top: &mut Plan,
        hooks: &mut Vec<plan::PlannedHook>,
    ) -> Result<(), ComposeError> {
        let elements: Vec<Option<Value>> = match &m.each {
            None => vec![None],
            Some(var) => {
                let value = inputs.0.iter().find(|i| i.name == *var).map(|i| i.value.clone()).unwrap_or_default();
                let iter = value
                    .try_iter()
                    .map_err(|_| ComposeError::ForNotList { path: m.path.clone(), var: var.clone() })?;
                iter.map(Some).collect()
            }
        };
        let renderer = Renderer::new(&self.root.join(CONTROL_DIR));
        let sub_manifest = sub.combined_manifest();

        for (index, item) in elements.into_iter().enumerate() {
            let mut ctx: BTreeMap<&str, Value> = inputs.0.iter().map(|i| (i.name.as_str(), i.value.clone())).collect();
            if let Some(item) = item {
                ctx.insert("item", item);
                ctx.insert("index", Value::from(index));
            }
            let ctx = Value::from_serialize(&ctx);
            let name = format!("mount `{}`", m.path);
            let mount_path = PathBuf::from(renderer.render(&format!("{name} path"), &m.path, &ctx)?);
            if mount_path.as_os_str().is_empty() || mount_path.components().any(|c| !matches!(c, Component::Normal(_))) {
                return Err(ComposeError::BadMountPath(mount_path.display().to_string()));
            }
            let mut overrides = Vec::new();
            for (k, v) in &m.inputs {
                overrides.push((k.clone(), renderer.render(&format!("{name} input `{k}`"), v, &ctx)?));
            }
            let sub_inputs = inputs::resolve(&sub_manifest, &overrides)
                .map_err(|source| ComposeError::MountInputs { path: mount_path.display().to_string(), source })?;

            let sub_dest = dest.map(|d| d.join(&mount_path));
            let sub_plan = sub.plan(sub_dest.as_deref(), sub_inputs, opts)?;

            let mut count = 0;
            for mut op in sub_plan.ops {
                if op.path == Path::new(ANSWERS_FILE) {
                    continue;
                }
                op.path = mount_path.join(&op.path);
                op.mount = Some(mount_path.clone());
                if let Some(i) = ops.iter().position(|o| o.path == op.path) {
                    // The outer template already provides this path. It wins,
                    // unless a merge rule says to combine the two.
                    let Some(spec) = self.merge_spec_for(&op.path).or_else(|| sub.merge_spec_for(&op.path)) else { continue };
                    let outer = ops.remove(i);
                    let combined = merge_ops(op, outer, &spec)?;
                    let mut combined = combined;
                    if let Some(d) = dest {
                        plan::restatus(d, &mut combined, &self.rules_manifest())?;
                    }
                    ops.push(combined);
                    continue;
                }
                count += 1;
                ops.push(op);
            }
            if !ops.iter().any(|o| o.path == mount_path) {
                ops.push(Op {
                    template_path: PathBuf::from(&m.template),
                    path: mount_path.clone(),
                    mount: Some(mount_path.clone()),
                    kind: OpKind::CreateDir,
                    status: dir_status(dest, &mount_path),
                });
                count += 1;
            }
            top.dropped.extend(sub_plan.dropped);
            top.warnings.extend(sub_plan.warnings.into_iter().map(|mut w| {
                w.path = mount_path.join(&w.path);
                w
            }));
            top.executed.extend(sub_plan.executed);
            hooks.extend(sub_plan.hooks.into_iter().map(|mut h| {
                h.cwd = if h.cwd.as_os_str().is_empty() { mount_path.clone() } else { mount_path.join(&h.cwd) };
                h
            }));
            top.mounts.extend(sub_plan.mounts.into_iter().map(|mut i| {
                i.path = mount_path.join(&i.path);
                i
            }));
            top.mounts.push(MountInfo { path: mount_path, template: m.template.clone(), ops: count });
        }
        Ok(())
    }
}

/// Add `op` from `layer` to `ops`. An earlier op at the same path is merged
/// into it when either side declares a merge rule for the path, and the
/// result is re-checked against the destination; otherwise it is replaced.
fn fold(ops: &mut Vec<Op>, op: Op, layer: &Loaded, dest: Option<&Path>) -> Result<(), ComposeError> {
    let Some(i) = ops.iter().position(|o| o.path == op.path) else {
        ops.push(op);
        return Ok(());
    };
    let earlier = ops.remove(i);
    match layer.merge_spec_for(&op.path) {
        Some(spec) => {
            let mut merged = merge_ops(earlier, op, &spec)?;
            if let Some(d) = dest {
                plan::restatus(d, &mut merged, &layer.rules_manifest())?;
            }
            ops.push(merged);
        }
        None => ops.push(op),
    }
    Ok(())
}

/// `later`'s content merged over `earlier`'s with `spec`, when both are text
/// files; otherwise `later` as is.
fn merge_ops(earlier: Op, mut later: Op, spec: &MergeSpec) -> Result<Op, ComposeError> {
    if let (
        OpKind::WriteFile { content: Content::Text(base), .. },
        OpKind::WriteFile { content: Content::Text(over), .. },
    ) = (&earlier.kind, &mut later.kind)
    {
        let merged = merge::merge(spec, base, over).map_err(|source| ComposeError::LayerMerge { path: later.path.clone(), source })?;
        *over = merged;
    }
    Ok(later)
}

fn dir_status(dest: Option<&Path>, rel: &Path) -> Status {
    let Some(dest) = dest else { return Status::Create };
    match std::fs::symlink_metadata(dest.join(rel)) {
        Ok(m) if m.is_dir() => Status::Unchanged,
        Ok(_) => Status::Conflict { reason: "a file already exists there".into() },
        Err(_) => Status::Create,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scaffolding::plan::{Content, OpKind};
    use std::fs;

    fn fixture(root: &Path, files: &[(&str, &str)]) {
        for (rel, text) in files {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }
    }

    #[tokio::test]
    async fn layers_override_by_path_and_share_vars() {
        let dir = tempfile::tempdir().unwrap();
        fixture(
            dir.path(),
            &[
                ("base/sc.toml", "[vars.license]\ndefault = \"MIT\"\n[[hooks]]\nphase = \"after\"\nrun = \"echo base\"\n"),
                ("base/LICENSE", "{{ license }}\n"),
                ("base/README.md", "base readme for {{ name }}\n"),
                ("base/.gitignore", "target/\n"),
                ("child/sc.toml", "[vars.name]\n[[uses]]\nsource = \"../base\"\n[[hooks]]\nphase = \"after\"\nrun = \"echo child\"\n"),
                ("child/README.md", "child readme for {{ name }} ({{ license }})\n"),
                ("child/src/main.rs", "fn main() {}\n"),
            ],
        );
        let loaded = load(&dir.path().join("child").display().to_string(), None, &source::Options::default()).await.unwrap();
        assert_eq!(loaded.bases.len(), 1);
        let manifest = loaded.combined_manifest();
        assert_eq!(manifest.vars.keys().collect::<Vec<_>>(), ["license", "name"]);
        let inputs = crate::scaffolding::inputs::resolve(&manifest, &[("name".into(), "app".into())]).unwrap();
        let plan = loaded.plan(None, inputs, &PlanOptions::default()).unwrap();

        let paths: Vec<_> = plan.ops.iter().map(|o| o.path.display().to_string()).collect();
        assert_eq!(paths, [".gitignore", ".sc.toml", "LICENSE", "README.md", "src", "src/main.rs"]);
        let text = |p: &str| match &plan.ops.iter().find(|o| o.path == Path::new(p)).unwrap().kind {
            OpKind::WriteFile { content: Content::Text(t), .. } => t.clone(),
            _ => panic!(),
        };
        assert_eq!(text("README.md"), "child readme for app (MIT)\n");
        assert_eq!(text("LICENSE"), "MIT\n");
        assert!(text(".sc.toml").contains("license = \"MIT\""), "answers include base vars");
        assert_eq!(plan.hooks.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(), ["echo base", "echo child"]);
    }

    #[tokio::test]
    async fn mounts_expand_per_item_and_outer_template_wins() {
        let dir = tempfile::tempdir().unwrap();
        fixture(
            dir.path(),
            &[
                (
                    "mono/sc.toml",
                    "[vars.org]\n[vars.services]\ntype = \"list\"\ndefault = [{ name = \"api\", port = 8080 }, { name = \"web\", port = 3000 }]\n\n[[mounts]]\npath = \"services/{{ item.name }}\"\ntemplate = \"parts/service\"\nfor = \"services\"\ninputs = { name = \"{{ item.name }}\", port = \"{{ item.port }}\", org = \"{{ org }}\" }\n\n[[mounts]]\npath = \"infra\"\ntemplate = \"parts/infra\"\n",
                ),
                ("mono/README.md", "{{ org }}: {% for s in services %}{{ s.name }} {% endfor %}\n"),
                ("mono/services/api/OVERRIDE.md", "outer\n"),
                ("mono/parts/service/sc.toml", "[vars.name]\n[vars.port]\ntype = \"int\"\n[vars.org]\n[[hooks]]\nphase = \"after\"\nrun = \"pwd\"\n"),
                ("mono/parts/service/main.go", "// {{ org }}/{{ name }} on {{ port }}\n"),
                ("mono/parts/service/OVERRIDE.md", "inner\n"),
                ("mono/parts/infra/sc.toml", ""),
                ("mono/parts/infra/main.tf", "tf\n"),
            ],
        );
        let loaded = load(&dir.path().join("mono").display().to_string(), None, &source::Options::default()).await.unwrap();
        let inputs = crate::scaffolding::inputs::resolve(&loaded.combined_manifest(), &[("org".into(), "acme".into())]).unwrap();
        let plan = loaded.plan(None, inputs, &PlanOptions::default()).unwrap();

        let paths: Vec<_> = plan.ops.iter().map(|o| o.path.display().to_string()).collect();
        assert_eq!(
            paths,
            [
                ".sc.toml", "README.md", "infra", "infra/main.tf",
                "services", "services/api", "services/api/OVERRIDE.md", "services/api/main.go",
                "services/web", "services/web/OVERRIDE.md", "services/web/main.go",
            ]
        );
        let text = |p: &str| match &plan.ops.iter().find(|o| o.path == Path::new(p)).unwrap().kind {
            OpKind::WriteFile { content: Content::Text(t), .. } => t.clone(),
            _ => panic!("{p}"),
        };
        assert_eq!(text("README.md"), "acme: api web \n");
        assert_eq!(text("services/api/main.go"), "// acme/api on 8080\n");
        assert_eq!(text("services/web/main.go"), "// acme/web on 3000\n");
        assert_eq!(text("services/api/OVERRIDE.md"), "outer\n", "outer template wins");
        assert_eq!(text("services/web/OVERRIDE.md"), "inner\n");
        assert!(plan.ops.iter().find(|o| o.path == Path::new("services/web/main.go")).unwrap().mount.as_deref() == Some(Path::new("services/web")));
        assert!(!paths.iter().any(|p| p.starts_with("parts")), "mounted sub-template dirs are control material");
        let hook_cwds: Vec<_> = plan.hooks.iter().map(|h| h.cwd.display().to_string()).collect();
        assert_eq!(hook_cwds, ["services/api", "services/web"]);
        let mounts: Vec<_> = plan.mounts.iter().map(|m| (m.path.display().to_string(), m.ops)).collect();
        assert_eq!(mounts, [("services/api".into(), 1), ("services/web".into(), 3), ("infra".into(), 2)], "api dir and override come from the outer template");
        assert!(text(".sc.toml").contains("[[inputs.services]]"), "list inputs land in the answers file");
    }

    #[tokio::test]
    async fn with_layers_union_inputs_and_merge_across_layers() {
        let dir = tempfile::tempdir().unwrap();
        fixture(
            dir.path(),
            &[
                ("ci/sc.toml", "[vars.runner]\ndefault = \"ubuntu-latest\"\n[files.merge]\n\".gitignore\" = \"append-lines\"\n\"Cargo.toml\" = \"toml\"\n"),
                ("ci/.gitignore", "ci-cache/\n"),
                ("ci/Cargo.toml", "[workspace.metadata.ci]\nrunner = \"{{ runner }}\"\n"),
                ("ci/.github/workflows/ci.yml", "runs-on: {{ runner }}\n"),
                ("project/sc.toml", "[vars.name]\n"),
                ("project/.gitignore", "target/\n"),
                ("project/Cargo.toml", "[package]\nname = \"{{ name }}\"\n"),
                ("project/README.md", "{{ name }} on {{ runner }}\n"),
            ],
        );
        let project = dir.path().join("project").display().to_string();
        let ci = dir.path().join("ci").display().to_string();
        let loaded = load_with(&project, None, std::slice::from_ref(&ci), &source::Options::default()).await.unwrap();
        let manifest = loaded.combined_manifest();
        assert_eq!(manifest.vars.keys().collect::<Vec<_>>(), ["runner", "name"], "inputs are the union");
        let inputs = crate::scaffolding::inputs::resolve(&manifest, &[("name".into(), "app".into())]).unwrap();
        let plan = loaded.plan(None, inputs, &PlanOptions::default()).unwrap();
        let text = |p: &str| match &plan.ops.iter().find(|o| o.path == Path::new(p)).unwrap().kind {
            OpKind::WriteFile { content: Content::Text(t), .. } => t.clone(),
            _ => panic!("{p}"),
        };
        assert_eq!(text(".gitignore"), "ci-cache/\ntarget/\n", "merged with the ci layer's rule");
        assert_eq!(text("Cargo.toml"), "[workspace.metadata.ci]\nrunner = \"ubuntu-latest\"\n\n[package]\nname = \"app\"\n");
        assert_eq!(text("README.md"), "app on ubuntu-latest\n");
        assert_eq!(text(".github/workflows/ci.yml"), "runs-on: ubuntu-latest\n");
        assert!(text(".sc.toml").contains(&format!("with = [\"{ci}\"]")), "answers record the layer");

        // Against a destination the merged content is what gets compared.
        let dest = tempfile::tempdir().unwrap();
        fs::write(dest.path().join(".gitignore"), "ci-cache/\ntarget/\n").unwrap();
        let inputs = crate::scaffolding::inputs::resolve(&manifest, &[("name".into(), "app".into())]).unwrap();
        let plan = loaded.plan(Some(dest.path()), inputs, &PlanOptions::default()).unwrap();
        assert_eq!(plan.ops.iter().find(|o| o.path == Path::new(".gitignore")).unwrap().status, Status::Unchanged);
    }

    #[tokio::test]
    async fn cycles_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path(), &[("a/sc.toml", "[[uses]]\nsource = \"../b\"\n"), ("b/sc.toml", "[[uses]]\nsource = \"../a\"\n")]);
        let err = load(&dir.path().join("a").display().to_string(), None, &source::Options::default()).await.unwrap_err();
        assert!(err.to_string().contains("cycle"), "{err}");
    }
}
