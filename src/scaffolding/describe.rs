//! `sc tree`: describe a template without rendering it. The input schema
//! comes from the manifest (every layer's, for composed templates); the tree
//! is the template directory as it is on disk, annotated with how each entry
//! will be treated.

use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Serialize;

use crate::scaffolding::compose::Loaded;
use crate::scaffolding::inputs::Inputs;
use crate::scaffolding::manifest::{CONTROL_DIR, MANIFEST_FILE, Manifest, MergeStrategy, VarKind};
use crate::scaffolding::plan::{PlanError, condition_holds};
use crate::scaffolding::render::Renderer;

#[derive(Debug, Serialize)]
pub struct Description {
    pub name: Option<String>,
    pub description: Option<String>,
    pub source: PathBuf,
    pub inputs: Vec<InputSchema>,
    pub uses: Vec<String>,
    pub mounts: Vec<MountSchema>,
    pub hooks: Vec<HookSchema>,
    pub keep: Vec<String>,
    pub merge: Vec<(String, MergeStrategy)>,
    pub tree: Vec<Entry>,
}

#[derive(Debug, Serialize)]
pub struct InputSchema {
    pub name: String,
    pub kind: VarKind,
    pub required: bool,
    pub default: Option<toml::Value>,
    pub prompt: Option<String>,
    pub choices: Vec<String>,
    pub pattern: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MountSchema {
    pub path: String,
    pub template: String,
    pub each: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct HookSchema {
    pub phase: crate::scaffolding::manifest::Phase,
    pub name: String,
    pub when: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Entry {
    /// Template-relative path, as on disk.
    pub path: PathBuf,
    pub depth: usize,
    pub kind: EntryKind,
    /// How the planner will treat it.
    pub tags: Vec<Tag>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Dir,
    File,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Tag {
    /// Consumed by the engine, never emitted.
    Control,
    /// Matched an `exclude` glob.
    Excluded { rule: String },
    /// Matched a `raw` glob: copied without rendering.
    Raw { rule: String },
    /// The name contains a block tag, so it may be dropped.
    Conditional,
    /// The name contains an expression and is rendered.
    Templated,
    /// Emitted under a different name, by `dot_prefix` or `[files.rename]`.
    Renamed { to: String },
    /// Covered by a `[tree]` condition. `holds` is known only when inputs
    /// were given.
    When { rule: String, holds: Option<bool> },
    /// A variant group directory: emits `output` from the child chosen by `var`.
    VariantGroup { output: String, var: String },
    /// A child of a variant group, serving this value. `chosen` is known
    /// only when inputs were given.
    Variant { var: String, value: String, chosen: Option<bool> },
}

/// Where a path inside a template ends up: for `sc where` and prompts.
#[derive(Debug, Serialize)]
pub struct Location {
    /// Template-relative path that was asked about.
    pub path: PathBuf,
    /// Output path, when every component is a plain name (no `{{ }}`).
    pub output: Option<PathBuf>,
    /// Tags of the path and of every ancestor, outermost first.
    pub tags: Vec<Tag>,
    /// The path is control material (the manifest, `.sc/`, a mounted
    /// sub-template, `.git`) and is never emitted.
    pub control: bool,
}

/// Find the template that contains `dir`: the nearest ancestor holding a
/// manifest, else the nearest ancestor holding a `.sc/` directory.
pub fn find_template_root(dir: &Path) -> Option<PathBuf> {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        if d.join(MANIFEST_FILE).is_file() || d.join(CONTROL_DIR).is_dir() {
            return Some(d.to_path_buf());
        }
        cur = d.parent();
    }
    None
}

/// Describe one path inside a loaded template, relative to its root.
pub fn locate(loaded: &Loaded, rel: &Path, inputs: Option<&Inputs>) -> Result<Location, DescribeError> {
    let mut rules = Rules::new(&loaded.manifest, &loaded.local_mount_dirs())?;
    if let Some(inputs) = inputs {
        let renderer = Renderer::new(&loaded.root.join(CONTROL_DIR));
        let ctx = inputs.context();
        for (_, expr, holds) in &mut rules.conditions {
            *holds = Some(condition_holds(&renderer, expr, &ctx)?);
        }
        rules.values = Some(inputs.0.iter().map(|i| (i.name.clone(), crate::scaffolding::inputs::override_text(&i.value))).collect());
    }
    let mut tags = Vec::new();
    let mut control = false;
    let mut so_far = PathBuf::new();
    for c in rel.components() {
        so_far.push(c);
        let is_dir = loaded.root.join(&so_far).is_dir();
        for t in rules.tags(&so_far, is_dir) {
            if t == Tag::Control {
                control = true;
            }
            // A condition on a directory also tags everything under it; say it once.
            if !tags.contains(&t) {
                tags.push(t);
            }
        }
    }
    let output = if control { None } else { rules.output_path(rel) };
    Ok(Location { path: rel.to_path_buf(), output, tags, control })
}

#[derive(Debug, thiserror::Error)]
pub enum DescribeError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("bad glob in manifest: {0}")]
    Glob(#[from] globset::Error),
    #[error(transparent)]
    Condition(#[from] PlanError),
}

/// `inputs`, when given, are used to evaluate `[tree]` conditions.
pub fn describe(loaded: &Loaded, inputs: Option<&Inputs>) -> Result<Description, DescribeError> {
    let combined = loaded.combined_manifest();
    let m = &loaded.manifest;
    let schema = combined
        .vars
        .iter()
        .map(|(name, spec)| InputSchema {
            name: name.clone(),
            kind: spec.kind,
            required: spec.default.is_none(),
            default: spec.default.clone(),
            prompt: spec.prompt.clone(),
            choices: spec.choices.clone(),
            pattern: spec.pattern.clone(),
        })
        .collect();

    let mut tree = Vec::new();
    let mut rules = Rules::new(m, &loaded.local_mount_dirs())?;
    if let Some(inputs) = inputs {
        let renderer = Renderer::new(&loaded.root.join(CONTROL_DIR));
        let ctx = inputs.context();
        for (_, expr, holds) in &mut rules.conditions {
            *holds = Some(condition_holds(&renderer, expr, &ctx)?);
        }
        rules.values = Some(inputs.0.iter().map(|i| (i.name.clone(), crate::scaffolding::inputs::override_text(&i.value))).collect());
    }
    walk(&loaded.root, Path::new(""), 0, &rules, &mut tree)?;

    Ok(Description {
        name: m.template.name.clone(),
        description: m.template.description.clone(),
        source: loaded.root.clone(),
        inputs: schema,
        uses: m.uses.iter().map(|u| u.source.clone()).collect(),
        mounts: m
            .mounts
            .iter()
            .map(|x| MountSchema { path: x.path.clone(), template: x.template.clone(), each: x.each.clone() })
            .collect(),
        hooks: m
            .hooks
            .iter()
            .map(|h| HookSchema {
                phase: h.phase,
                name: h.name.clone().or_else(|| h.script.clone()).unwrap_or_else(|| {
                    h.run.as_deref().unwrap_or("").lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string()
                }),
                when: h.when.clone(),
            })
            .collect(),
        keep: m.files.keep.clone(),
        merge: m.files.merge.iter().map(|(k, v)| (k.clone(), v.spec().strategy)).collect(),
        tree,
    })
}

struct Rules {
    exclude: (GlobSet, Vec<String>),
    raw: (GlobSet, Vec<String>),
    control: Vec<PathBuf>,
    dot_prefix: String,
    rename: indexmap::IndexMap<String, String>,
    /// (output-path globs, expression, evaluated?) from `[tree]`.
    conditions: Vec<(GlobSet, String, Option<bool>)>,
    variant_sep: String,
    /// Input values as text, when inputs were given.
    values: Option<std::collections::BTreeMap<String, String>>,
}

impl Rules {
    fn new(m: &Manifest, mounts: &[PathBuf]) -> Result<Self, globset::Error> {
        let set = |pats: &[String]| -> Result<(GlobSet, Vec<String>), globset::Error> {
            let mut b = GlobSetBuilder::new();
            for p in pats {
                b.add(Glob::new(p)?);
            }
            Ok((b.build()?, pats.to_vec()))
        };
        let mut control = vec![PathBuf::from(MANIFEST_FILE), PathBuf::from(CONTROL_DIR)];
        control.extend_from_slice(mounts);
        let mut conditions = Vec::new();
        for (globs, expr) in m.conditions() {
            conditions.push((set(&globs)?.0, expr, None));
        }
        Ok(Self {
            exclude: set(&m.files.exclude)?,
            raw: set(&m.files.raw)?,
            control,
            dot_prefix: m.files.dot_prefix.clone(),
            rename: m.files.rename.clone(),
            conditions,
            variant_sep: m.files.variant_sep.clone(),
            values: None,
        })
    }

    /// `NAME<sep>VAR` -> (NAME, VAR).
    fn variant_group(&self, name: &str) -> Option<(String, String)> {
        if self.variant_sep.is_empty() {
            return None;
        }
        let (base, var) = name.rsplit_once(&self.variant_sep)?;
        (!base.is_empty() && !var.is_empty() && var.chars().all(|c| c == '_' || c.is_ascii_alphanumeric()))
            .then(|| (base.to_string(), var.to_string()))
    }

    /// The output path of a template path with no template syntax in it:
    /// renames and the dot prefix applied per component.
    fn output_path(&self, rel: &Path) -> Option<PathBuf> {
        let mut out = PathBuf::new();
        let mut so_far = PathBuf::new();
        let mut skip_next = false;
        for c in rel.components() {
            let mut name = c.as_os_str().to_string_lossy().into_owned();
            if name.contains("{{") || name.contains("{%") {
                return None;
            }
            so_far.push(&name);
            if skip_next {
                // The chosen child of a group takes the group's name.
                skip_next = false;
                continue;
            }
            if let Some((base, _)) = self.variant_group(&name) {
                name = base;
                skip_next = true;
            }
            let key = so_far.display().to_string();
            let mapped = match self.rename.get(&key).or_else(|| self.rename.get(&format!("{key}/"))) {
                Some(to) => to.trim_end_matches('/').to_string(),
                None => match name.strip_prefix(&self.dot_prefix) {
                    Some(rest) if !self.dot_prefix.is_empty() && !rest.is_empty() => format!(".{rest}"),
                    _ => name,
                },
            };
            out.push(mapped);
        }
        Some(out)
    }

    fn tags(&self, rel: &Path, is_dir: bool) -> Vec<Tag> {
        let mut tags = Vec::new();
        if self.control.contains(&rel.to_path_buf()) {
            tags.push(Tag::Control);
            return tags;
        }
        if let Some(&i) = self.exclude.0.matches(rel).first() {
            tags.push(Tag::Excluded { rule: self.exclude.1[i].clone() });
            return tags;
        }
        if let (false, Some(&i)) = (is_dir, self.raw.0.matches(rel).first()) {
            tags.push(Tag::Raw { rule: self.raw.1[i].clone() });
        }
        let name_now = rel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if let (true, Some((output, var))) = (is_dir, self.variant_group(&name_now)) {
            tags.push(Tag::VariantGroup { output, var });
            return tags;
        }
        if let Some((_, var)) = rel.parent().and_then(|p| p.file_name()).and_then(|n| self.variant_group(&n.to_string_lossy())) {
            let value = if is_dir { name_now.clone() } else { crate::scaffolding::manifest::variant_stem(&name_now) };
            let chosen = self.values.as_ref().map(|v| v.get(&var) == Some(&value));
            tags.push(Tag::Variant { var, value, chosen });
        }
        if let Some(out) = self.output_path(rel) {
            for (set, expr, holds) in &self.conditions {
                if set.is_match(&out) {
                    tags.push(Tag::When { rule: expr.clone(), holds: *holds });
                }
            }
        }
        let name = rel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let key = rel.display().to_string();
        if let Some(to) = self.rename.get(&key).or_else(|| self.rename.get(&format!("{key}/"))) {
            tags.push(Tag::Renamed { to: to.trim_end_matches('/').to_string() });
        } else if name.contains("{%") {
            tags.push(Tag::Conditional);
        } else if name.contains("{{") {
            tags.push(Tag::Templated);
        } else if let Some(rest) = name.strip_prefix(&self.dot_prefix).filter(|r| !r.is_empty() && !self.dot_prefix.is_empty()) {
            tags.push(Tag::Renamed { to: format!(".{rest}") });
        }
        tags
    }
}

fn walk(root: &Path, rel: &Path, depth: usize, rules: &Rules, out: &mut Vec<Entry>) -> Result<(), DescribeError> {
    let abs = root.join(rel);
    let io = |source| DescribeError::Io { path: abs.clone(), source };
    let mut entries: Vec<_> = std::fs::read_dir(&abs).map_err(io)?.collect::<Result<_, _>>().map_err(io)?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name();
        if name == ".git" {
            continue;
        }
        let rel = rel.join(&name);
        let ft = e.file_type().map_err(io)?;
        let kind = if ft.is_symlink() {
            EntryKind::Symlink
        } else if ft.is_dir() {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        let tags = rules.tags(&rel, kind == EntryKind::Dir);
        let descend = kind == EntryKind::Dir && !tags.iter().any(|t| matches!(t, Tag::Control | Tag::Excluded { .. }));
        out.push(Entry { path: rel.clone(), depth, kind, tags });
        if descend {
            walk(root, &rel, depth + 1, rules, out)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scaffolding::compose;
    use std::fs;

    #[tokio::test]
    async fn describes_schema_and_tree() {
        let dir = tempfile::tempdir().unwrap();
        for (rel, text) in [
            ("sc.toml", "[vars.name]\nprompt = \"Name\"\n[vars.ci]\ntype = \"bool\"\ndefault = true\n[files]\ndot_prefix = \"_\"\nexclude = [\"*.orig\"]\nraw = [\"assets/**\"]\n[files.rename]\n\"github/\" = \".github/\"\n[[mounts]]\npath = \"svc\"\ntemplate = \"parts/svc\"\n"),
            ("github/workflows/ci.yml", ""),
            ("README.md", ""),
            ("_gitignore", ""),
            ("{% if ci %}ci.yml{% endif %}", ""),
            ("src/{{ name }}.rs", ""),
            ("assets/logo.png", ""),
            ("old.orig", ""),
            ("parts/svc/sc.toml", ""),
            (".sc/tests/x/inputs.toml", "name = \"a\"\n"),
        ] {
            let p = dir.path().join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }
        let loaded = compose::load(&dir.path().display().to_string(), None, &Default::default()).await.unwrap();
        let d = describe(&loaded, None).unwrap();

        let names: Vec<_> = d.inputs.iter().map(|i| (i.name.as_str(), i.required)).collect();
        assert_eq!(names, [("name", true), ("ci", false)]);
        assert_eq!(d.mounts[0].template, "parts/svc");

        let tag = |p: &str| d.tree.iter().find(|e| e.path == Path::new(p)).map(|e| e.tags.clone()).unwrap_or_else(|| panic!("{p}"));
        assert_eq!(tag(".sc"), [Tag::Control]);
        assert_eq!(tag("sc.toml"), [Tag::Control]);
        assert_eq!(tag("parts/svc"), [Tag::Control]);
        assert_eq!(tag("old.orig"), [Tag::Excluded { rule: "*.orig".into() }]);
        assert_eq!(tag("assets/logo.png"), [Tag::Raw { rule: "assets/**".into() }]);
        assert_eq!(tag("_gitignore"), [Tag::Renamed { to: ".gitignore".into() }]);
        assert_eq!(tag("github"), [Tag::Renamed { to: ".github".into() }]);
        assert_eq!(tag("{% if ci %}ci.yml{% endif %}"), [Tag::Conditional]);
        assert_eq!(tag("src/{{ name }}.rs"), [Tag::Templated]);
        assert_eq!(tag("README.md"), []);
        assert!(!d.tree.iter().any(|e| e.path.starts_with(".sc/tests")), "control dirs are not descended");

        // Conditions: unevaluated without inputs, evaluated with them.
        fs::write(dir.path().join("sc.toml"), "[vars.ci]\ntype = \"bool\"\ndefault = true\n[files]\ndot_prefix = \"_\"\n[tree]\n\".github/\" = { when = \"ci\" }\n").unwrap();
        fs::create_dir_all(dir.path().join("_github/workflows")).unwrap();
        fs::write(dir.path().join("_github/workflows/ci.yml"), "").unwrap();
        let loaded = compose::load(&dir.path().display().to_string(), None, &Default::default()).await.unwrap();
        let d = describe(&loaded, None).unwrap();
        let e = d.tree.iter().find(|e| e.path == Path::new("_github/workflows/ci.yml")).unwrap();
        assert!(e.tags.contains(&Tag::When { rule: "ci".into(), holds: None }));
        let inputs = crate::scaffolding::inputs::resolve(&loaded.combined_manifest(), &[("ci".into(), "false".into())]).unwrap();
        let d = describe(&loaded, Some(&inputs)).unwrap();
        let e = d.tree.iter().find(|e| e.path == Path::new("_github")).unwrap();
        assert!(e.tags.contains(&Tag::When { rule: "ci".into(), holds: Some(false) }));

        // Variant groups.
        fs::create_dir_all(dir.path().join("README.md@ci")).unwrap();
        fs::write(dir.path().join("README.md@ci/true.md"), "").unwrap();
        fs::write(dir.path().join("README.md@ci/false.md"), "").unwrap();
        let loaded = compose::load(&dir.path().display().to_string(), None, &Default::default()).await.unwrap();
        let inputs = crate::scaffolding::inputs::resolve(&loaded.combined_manifest(), &[("ci".into(), "false".into())]).unwrap();
        let d = describe(&loaded, Some(&inputs)).unwrap();
        let tag = |p: &str| d.tree.iter().find(|e| e.path == Path::new(p)).map(|e| e.tags.clone()).unwrap_or_else(|| panic!("{p}"));
        assert_eq!(tag("README.md@ci"), [Tag::VariantGroup { output: "README.md".into(), var: "ci".into() }]);
        assert!(tag("README.md@ci/false.md").contains(&Tag::Variant { var: "ci".into(), value: "false".into(), chosen: Some(true) }));
        assert!(tag("README.md@ci/true.md").contains(&Tag::Variant { var: "ci".into(), value: "true".into(), chosen: Some(false) }));
    }
}
