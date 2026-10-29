//! Loading a template together with the templates it `uses`, and planning
//! them as one. Bases are planned first, depth-first in declaration order;
//! a later layer's op replaces an earlier one at the same output path.

use std::path::{Path, PathBuf};

use crate::inputs::Inputs;
use crate::manifest::{Manifest, ManifestError};
use crate::plan::{self, ANSWERS_FILE, Plan, PlanError, PlanOptions};
use crate::source::{self, GitInfo, Resolved, Source, SourceError};

const MAX_DEPTH: usize = 8;

#[derive(Debug)]
pub struct Loaded {
    pub root: PathBuf,
    pub manifest: Manifest,
    pub git: Option<GitInfo>,
    pub bases: Vec<Loaded>,
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
    #[error("templates nest deeper than {MAX_DEPTH}; is there a `uses` cycle?")]
    TooDeep,
    #[error("in template used from {parent}: {source}")]
    InBase {
        parent: PathBuf,
        #[source]
        source: Box<ComposeError>,
    },
}

pub fn load(spec: &str, subdir: Option<PathBuf>, fetch: &source::Options) -> Result<Loaded, ComposeError> {
    load_resolved(Source::parse(spec, subdir).resolve(fetch)?, fetch, 0)
}

pub fn load_resolved(resolved: Resolved, fetch: &source::Options, depth: usize) -> Result<Loaded, ComposeError> {
    if depth > MAX_DEPTH {
        return Err(ComposeError::TooDeep);
    }
    let root = resolved.root.clone();
    let manifest = Manifest::load(&root)?;
    let mut bases = Vec::new();
    for u in &manifest.uses {
        let src = match Source::parse(&u.source, u.subdir.clone()) {
            Source::Local(p) if p.is_relative() => Source::Local(root.join(p)),
            other => other,
        };
        let base = src
            .resolve(fetch)
            .map_err(ComposeError::from)
            .and_then(|r| load_resolved(r, fetch, depth + 1))
            .map_err(|e| ComposeError::InBase { parent: root.clone(), source: Box::new(e) })?;
        bases.push(base);
    }
    Ok(Loaded { root, manifest, git: resolved.git.clone(), bases, resolved })
}

impl Loaded {
    /// Manifest to resolve inputs against: every layer's variables.
    pub fn combined_manifest(&self) -> Manifest {
        let mut all = Vec::new();
        self.collect_manifests(&mut all);
        let own = all.pop().expect("self is last");
        own.with_base_vars(all)
    }

    fn collect_manifests<'a>(&'a self, out: &mut Vec<&'a Manifest>) {
        for b in &self.bases {
            b.collect_manifests(out);
        }
        out.push(&self.manifest);
    }

    /// Plan every layer and fold them into one plan.
    pub fn plan(&self, dest: Option<&Path>, inputs: Inputs, opts: &PlanOptions) -> Result<Plan, ComposeError> {
        let mut layers = Vec::new();
        self.plan_layers(dest, &inputs, opts, &mut layers)?;
        let mut top = layers.pop().expect("self is last");
        if layers.is_empty() {
            return Ok(top);
        }

        let mut ops = Vec::new();
        for layer in layers {
            for op in layer.ops {
                // Each layer writes its own answers file; only the top one counts.
                if op.path == Path::new(ANSWERS_FILE) {
                    continue;
                }
                ops.push(op);
            }
            top.dropped.splice(0..0, layer.dropped);
            top.warnings.splice(0..0, layer.warnings);
            top.hooks.splice(0..0, layer.hooks);
            top.executed.splice(0..0, layer.executed);
        }
        for op in top.ops {
            ops.retain(|o| o.path != op.path);
            ops.push(op);
        }
        ops.sort_by(|a, b| a.path.cmp(&b.path));
        top.ops = ops;
        Ok(top)
    }

    fn plan_layers(
        &self,
        dest: Option<&Path>,
        inputs: &Inputs,
        opts: &PlanOptions,
        out: &mut Vec<Plan>,
    ) -> Result<(), ComposeError> {
        for b in &self.bases {
            b.plan_layers(dest, inputs, opts, out)?;
        }
        let layer_opts = PlanOptions { allow_shell: opts.allow_shell, git: self.git.clone() };
        out.push(plan::plan(&self.root, dest, &self.manifest, inputs.clone(), &layer_opts)?);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{Content, OpKind};
    use std::fs;

    fn fixture(root: &Path, files: &[(&str, &str)]) {
        for (rel, text) in files {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }
    }

    #[test]
    fn layers_override_by_path_and_share_vars() {
        let dir = tempfile::tempdir().unwrap();
        fixture(
            dir.path(),
            &[
                ("base/scaffold.toml", "[vars.license]\ndefault = \"MIT\"\n[[hooks]]\nphase = \"after\"\nrun = \"echo base\"\n"),
                ("base/LICENSE", "{{ license }}\n"),
                ("base/README.md", "base readme for {{ name }}\n"),
                ("base/dot_gitignore", "target/\n"),
                ("child/scaffold.toml", "[vars.name]\n[[uses]]\nsource = \"../base\"\n[[hooks]]\nphase = \"after\"\nrun = \"echo child\"\n"),
                ("child/README.md", "child readme for {{ name }} ({{ license }})\n"),
                ("child/src/main.rs", "fn main() {}\n"),
            ],
        );
        let loaded = load(&dir.path().join("child").display().to_string(), None, &source::Options::default()).unwrap();
        assert_eq!(loaded.bases.len(), 1);
        let manifest = loaded.combined_manifest();
        assert_eq!(manifest.vars.keys().collect::<Vec<_>>(), ["license", "name"]);
        let inputs = crate::inputs::resolve(&manifest, &[("name".into(), "app".into())]).unwrap();
        let plan = loaded.plan(None, inputs, &PlanOptions::default()).unwrap();

        let paths: Vec<_> = plan.ops.iter().map(|o| o.path.display().to_string()).collect();
        assert_eq!(paths, [".gitignore", ".scaffold.toml", "LICENSE", "README.md", "src", "src/main.rs"]);
        let text = |p: &str| match &plan.ops.iter().find(|o| o.path == Path::new(p)).unwrap().kind {
            OpKind::WriteFile { content: Content::Text(t), .. } => t.clone(),
            _ => panic!(),
        };
        assert_eq!(text("README.md"), "child readme for app (MIT)\n");
        assert_eq!(text("LICENSE"), "MIT\n");
        assert!(text(".scaffold.toml").contains("license = \"MIT\""), "answers include base vars");
        assert_eq!(plan.hooks.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(), ["echo base", "echo child"]);
    }

    #[test]
    fn cycles_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path(), &[("a/scaffold.toml", "[[uses]]\nsource = \"../b\"\n"), ("b/scaffold.toml", "[[uses]]\nsource = \"../a\"\n")]);
        let err = load(&dir.path().join("a").display().to_string(), None, &source::Options::default()).unwrap_err();
        assert!(err.to_string().contains("cycle"), "{err}");
    }
}
