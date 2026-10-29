//! Golden tests for templates: `.scaffold/tests/<case>/inputs.toml` holds
//! the inputs, `.scaffold/tests/<case>/expected/` the output tree the plan
//! must produce. `sc test` compares, `sc test --update` rewrites.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::compose::{ComposeError, Loaded};
use crate::inputs::{self, InputError};
use crate::manifest::CONTROL_DIR;
use crate::plan::{ANSWERS_FILE, OpKind, PlanOptions};

#[derive(Debug)]
pub struct CaseResult {
    pub name: String,
    pub findings: Vec<Finding>,
}

#[derive(Debug)]
pub enum Finding {
    Missing(PathBuf),
    Extra(PathBuf),
    /// Text content differs; carries the expected and actual text.
    Differs { path: PathBuf, expected: String, actual: String },
    BinaryDiffers(PathBuf),
    ModeDiffers { path: PathBuf, expected_exec: bool },
}

#[derive(Debug, thiserror::Error)]
pub enum TestError {
    #[error("no tests under {0}/{CONTROL_DIR}/tests")]
    NoTests(PathBuf),
    #[error("case `{0}` not found")]
    NoCase(String),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{case}/inputs.toml: {source}")]
    Inputs {
        case: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("case `{case}`: {source}")]
    Resolve {
        case: String,
        #[source]
        source: InputError,
    },
    #[error("case `{case}`: {source}")]
    Plan {
        case: String,
        #[source]
        source: Box<ComposeError>,
    },
}

/// A rendered output tree: path -> (bytes, executable).
type Tree = BTreeMap<PathBuf, (Vec<u8>, bool)>;

pub fn run(loaded: &Loaded, only: Option<&str>, update: bool) -> Result<Vec<CaseResult>, TestError> {
    let tests_dir = loaded.root.join(CONTROL_DIR).join("tests");
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |source| TestError::Io { path, source }
    };
    let mut cases: Vec<PathBuf> = match std::fs::read_dir(&tests_dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(TestError::NoTests(loaded.root.clone())),
        Err(e) => return Err(io(&tests_dir)(e)),
    };
    cases.sort();
    if let Some(name) = only {
        cases.retain(|c| c.file_name().is_some_and(|f| f == name));
        if cases.is_empty() {
            return Err(TestError::NoCase(name.to_string()));
        }
    }

    let manifest = loaded.combined_manifest();
    let mut results = Vec::new();
    for case in cases {
        let name = case.file_name().unwrap().to_string_lossy().into_owned();
        let inputs_path = case.join("inputs.toml");
        let text = std::fs::read_to_string(&inputs_path).map_err(io(&inputs_path))?;
        let table: toml::Table = toml::from_str(&text).map_err(|source| TestError::Inputs { case: name.clone(), source })?;
        let overrides: Vec<(String, String)> = table
            .into_iter()
            .map(|(k, v)| (k, match v { toml::Value::String(s) => s, other => other.to_string() }))
            .collect();
        let inputs = inputs::resolve(&manifest, &overrides).map_err(|source| TestError::Resolve { case: name.clone(), source })?;
        let plan = loaded
            .plan(None, inputs, &PlanOptions::default())
            .map_err(|source| TestError::Plan { case: name.clone(), source: Box::new(source) })?;

        let mut actual = Tree::new();
        for op in &plan.ops {
            if op.path == Path::new(ANSWERS_FILE) {
                continue; // contains the absolute template path
            }
            match &op.kind {
                OpKind::WriteFile { content, mode, .. } => {
                    actual.insert(op.path.clone(), (content.as_bytes().to_vec(), mode & 0o111 != 0));
                }
                OpKind::Symlink { target } => {
                    actual.insert(op.path.clone(), (format!("-> {}", target.display()).into_bytes(), false));
                }
                OpKind::CreateDir => {}
            }
        }

        let expected_dir = case.join("expected");
        if update {
            write_tree(&expected_dir, &actual)?;
            results.push(CaseResult { name, findings: vec![] });
            continue;
        }
        let expected = read_tree(&expected_dir)?;
        results.push(CaseResult { name, findings: compare(&expected, &actual) });
    }
    Ok(results)
}

fn compare(expected: &Tree, actual: &Tree) -> Vec<Finding> {
    let mut out = Vec::new();
    for (path, (want, want_exec)) in expected {
        match actual.get(path) {
            None => out.push(Finding::Missing(path.clone())),
            Some((got, got_exec)) => {
                if want != got {
                    match (std::str::from_utf8(want), std::str::from_utf8(got)) {
                        (Ok(e), Ok(a)) => out.push(Finding::Differs { path: path.clone(), expected: e.into(), actual: a.into() }),
                        _ => out.push(Finding::BinaryDiffers(path.clone())),
                    }
                } else if want_exec != got_exec {
                    out.push(Finding::ModeDiffers { path: path.clone(), expected_exec: *want_exec });
                }
            }
        }
    }
    for path in actual.keys() {
        if !expected.contains_key(path) {
            out.push(Finding::Extra(path.clone()));
        }
    }
    out
}

fn read_tree(dir: &Path) -> Result<Tree, TestError> {
    let mut tree = Tree::new();
    if !dir.exists() {
        return Ok(tree);
    }
    fn walk(base: &Path, dir: &Path, tree: &mut Tree) -> Result<(), TestError> {
        let io = |source| TestError::Io { path: dir.to_path_buf(), source };
        for entry in std::fs::read_dir(dir).map_err(io)? {
            let entry = entry.map_err(io)?;
            let path = entry.path();
            let ft = entry.file_type().map_err(io)?;
            if ft.is_dir() {
                walk(base, &path, tree)?;
            } else {
                let rel = path.strip_prefix(base).unwrap().to_path_buf();
                let bytes = if ft.is_symlink() {
                    format!("-> {}", std::fs::read_link(&path).map_err(io)?.display()).into_bytes()
                } else {
                    std::fs::read(&path).map_err(io)?
                };
                let exec = !ft.is_symlink() && crate::plan::file_mode(&path).map_err(io)? & 0o111 != 0;
                tree.insert(rel, (bytes, exec));
            }
        }
        Ok(())
    }
    walk(dir, dir, &mut tree)?;
    Ok(tree)
}

fn write_tree(dir: &Path, tree: &Tree) -> Result<(), TestError> {
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |source| TestError::Io { path, source }
    };
    if dir.exists() {
        std::fs::remove_dir_all(dir).map_err(io(dir))?;
    }
    for (rel, (bytes, exec)) in tree {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).map_err(io(&path))?;
        std::fs::write(&path, bytes).map_err(io(&path))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if *exec { 0o755 } else { 0o644 };
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).map_err(io(&path))?;
        }
    }
    Ok(())
}

impl CaseResult {
    pub fn passed(&self) -> bool {
        self.findings.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose;
    use std::fs;

    #[test]
    fn golden_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        for (rel, text) in [
            ("scaffold.toml", "[vars.name]\n[vars.ci]\ntype = \"bool\"\ndefault = false\n"),
            ("README.md", "# {{ name }}\n"),
            ("{% if ci %}ci.yml{% endif %}", "on: push\n"),
            (".scaffold/tests/basic/inputs.toml", "name = \"demo\"\n"),
            (".scaffold/tests/with-ci/inputs.toml", "name = \"demo\"\nci = true\n"),
        ] {
            let p = dir.path().join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }
        let loaded = compose::load(&dir.path().display().to_string(), None, &Default::default()).unwrap();

        // Nothing expected yet: everything is extra.
        let results = run(&loaded, None, false).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| !r.passed()));

        run(&loaded, None, true).unwrap();
        assert!(dir.path().join(".scaffold/tests/with-ci/expected/ci.yml").exists());
        assert!(!dir.path().join(".scaffold/tests/basic/expected/ci.yml").exists());
        let results = run(&loaded, None, false).unwrap();
        assert!(results.iter().all(|r| r.passed()), "{results:?}");

        fs::write(dir.path().join("README.md"), "# {{ name }}!\n").unwrap();
        let results = run(&loaded, Some("basic"), false).unwrap();
        assert_eq!(results.len(), 1);
        assert!(matches!(&results[0].findings[..], [Finding::Differs { path, .. }] if path == Path::new("README.md")));
        assert!(matches!(run(&loaded, Some("nope"), false), Err(TestError::NoCase(_))));
    }
}
