//! `sc tree`: describe a template without rendering it. The input schema
//! comes from the manifest (every layer's, for composed templates); the tree
//! is the template directory as it is on disk, annotated with how each entry
//! will be treated.

use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Serialize;

use crate::compose::Loaded;
use crate::manifest::{CONTROL_DIR, MANIFEST_FILE, Manifest, MergeStrategy, VarKind};

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
    pub phase: crate::manifest::Phase,
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
    /// Emitted under a different name because of `dot_prefix`.
    Renamed { to: String },
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
}

pub fn describe(loaded: &Loaded) -> Result<Description, DescribeError> {
    let combined = loaded.combined_manifest();
    let m = &loaded.manifest;
    let inputs = combined
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
    let rules = Rules::new(m, &loaded.local_mount_dirs())?;
    walk(&loaded.root, Path::new(""), 0, &rules, &mut tree)?;

    Ok(Description {
        name: m.template.name.clone(),
        description: m.template.description.clone(),
        source: loaded.root.clone(),
        inputs,
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
        Ok(Self { exclude: set(&m.files.exclude)?, raw: set(&m.files.raw)?, control, dot_prefix: m.files.dot_prefix.clone() })
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
        let name = rel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if name.contains("{%") {
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
    use crate::compose;
    use std::fs;

    #[test]
    fn describes_schema_and_tree() {
        let dir = tempfile::tempdir().unwrap();
        for (rel, text) in [
            ("sc.toml", "[vars.name]\nprompt = \"Name\"\n[vars.ci]\ntype = \"bool\"\ndefault = true\n[files]\nexclude = [\"*.orig\"]\nraw = [\"assets/**\"]\n[[mounts]]\npath = \"svc\"\ntemplate = \"parts/svc\"\n"),
            ("README.md", ""),
            ("dot_gitignore", ""),
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
        let loaded = compose::load(&dir.path().display().to_string(), None, &Default::default()).unwrap();
        let d = describe(&loaded).unwrap();

        let names: Vec<_> = d.inputs.iter().map(|i| (i.name.as_str(), i.required)).collect();
        assert_eq!(names, [("name", true), ("ci", false)]);
        assert_eq!(d.mounts[0].template, "parts/svc");

        let tag = |p: &str| d.tree.iter().find(|e| e.path == Path::new(p)).map(|e| e.tags.clone()).unwrap_or_else(|| panic!("{p}"));
        assert_eq!(tag(".sc"), [Tag::Control]);
        assert_eq!(tag("sc.toml"), [Tag::Control]);
        assert_eq!(tag("parts/svc"), [Tag::Control]);
        assert_eq!(tag("old.orig"), [Tag::Excluded { rule: "*.orig".into() }]);
        assert_eq!(tag("assets/logo.png"), [Tag::Raw { rule: "assets/**".into() }]);
        assert_eq!(tag("dot_gitignore"), [Tag::Renamed { to: ".gitignore".into() }]);
        assert_eq!(tag("{% if ci %}ci.yml{% endif %}"), [Tag::Conditional]);
        assert_eq!(tag("src/{{ name }}.rs"), [Tag::Templated]);
        assert_eq!(tag("README.md"), []);
        assert!(!d.tree.iter().any(|e| e.path.starts_with(".sc/tests")), "control dirs are not descended");
    }
}
