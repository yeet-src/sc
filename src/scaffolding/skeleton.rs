//! `sc new`: write a starter template that demonstrates the conventions and
//! comes with a passing golden test.

use std::path::{Path, PathBuf};

use crate::scaffolding::manifest::{CONTROL_DIR, MANIFEST_FILE};

#[derive(Debug, thiserror::Error)]
pub enum NewError {
    #[error("{0} exists and is not empty")]
    NotEmpty(PathBuf),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Files written for a template called `name`. Paths are relative.
pub fn files(name: &str) -> Vec<(PathBuf, String)> {
    let manifest = format!(
        r#"# Template manifest. Every section is optional; delete what you do not need.

[template]
name = "{name}"
description = "Describe what this template makes"

# Inputs, in the order they are asked for. Types: string, bool, int, choice, list.
[vars.name]
prompt = "Project name"
pattern = "[A-Za-z][A-Za-z0-9 _-]*"

[vars.ci]
type = "bool"
prompt = "Add a CI workflow?"
default = true

# Per-path rules, keyed by output path. A trailing slash covers a directory.
[tree]
".github/" = {{ when = "ci" }}       # dropped when the condition is false
# "README.md"  = {{ keep = true }}                  # created once, never overwritten
# "Cargo.toml" = {{ merge = "toml" }}               # combine with an existing file: toml | json | yaml | append-lines
# "src/lib.rs" = {{ merge = "insert", marker = "// sc:modules" }}
# "assets/**"  = {{ raw = true }}                   # copied byte-for-byte, not rendered

# Variants: a directory `LICENSE@license/` holding `MIT` and `Apache-2.0`
# emits the one matching the license input as LICENSE, or nothing.
#
# How template entries are named and treated.
# [files]
# exclude = ["*.orig"]              # never emitted (template-path globs)
# dot_prefix = "_"                  # _gitignore -> .gitignore; off by default
#
# Variants: a directory `LICENSE@license/` holding `MIT` and `Apache-2.0`
# emits the one matching the license input as LICENSE, or nothing.
#
# [files.rename]                    # or name entries explicitly
# "gitignore" = ".gitignore"
# "github/" = ".github/"
#
# [[files.syntax]]                  # other delimiters for files that use {{{{ }}}} themselves
# path = "charts/**"
# variable = ["[[", "]]"]

# Commands to run. `after` hooks run in the destination once files are written.
# [[hooks]]
# name = "git init"
# phase = "after"
# when = "{{{{ git }}}}"
# run = "git init -q"

# Templates this one builds on, and sub-templates mounted at a path.
# [[uses]]
# source = "gh:org/base-template#v1"
#
# [[mounts]]
# path = "services/{{{{ item.name }}}}"
# template = "parts/service"
# for = "services"                  # a list input; one mount per element
# inputs = {{ name = "{{{{ item.name }}}}" }}
"#
    );
    let readme = "# {{ name | title_case }}\n\n{% include \"partials/footer.md\" %}";
    let footer = "Generated from the `{{ name }}` inputs by sc.\n";
    let gitignore = "/target\n";
    let ci = "name: CI\non: [push]\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n";
    let inputs = "name = \"Example Project\"\n";
    vec![
        (PathBuf::from(MANIFEST_FILE), manifest),
        (PathBuf::from("README.md"), readme.into()),
        (PathBuf::from(".gitignore"), gitignore.into()),
        (PathBuf::from(".github/workflows/ci.yml"), ci.into()),
        (PathBuf::from(CONTROL_DIR).join("partials/footer.md"), footer.into()),
        (PathBuf::from(CONTROL_DIR).join("tests/default/inputs.toml"), inputs.into()),
    ]
}

/// Write the skeleton into `dir`, which must not exist or be empty.
pub fn create(dir: &Path, name: &str) -> Result<Vec<PathBuf>, NewError> {
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |source| NewError::Io { path, source }
    };
    if dir.exists() && std::fs::read_dir(dir).map_err(io(dir))?.next().is_some() {
        return Err(NewError::NotEmpty(dir.to_path_buf()));
    }
    let mut written = Vec::new();
    for (rel, text) in files(name) {
        let path = dir.join(&rel);
        std::fs::create_dir_all(path.parent().unwrap()).map_err(io(&path))?;
        std::fs::write(&path, text).map_err(io(&path))?;
        written.push(rel);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scaffolding::{compose, golden};

    #[tokio::test]
    async fn skeleton_plans_and_passes_its_own_golden_test() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("tpl");
        let written = create(&dir, "demo").unwrap();
        assert_eq!(written.len(), 6);
        assert!(matches!(create(&dir, "demo"), Err(NewError::NotEmpty(_))));

        let loaded = compose::load(&dir.display().to_string(), None, &Default::default()).await.unwrap();
        assert_eq!(loaded.manifest.template.name.as_deref(), Some("demo"));
        golden::run(&loaded, None, true).unwrap();
        let results = golden::run(&loaded, None, false).unwrap();
        assert!(results.iter().all(|r| r.passed()), "{results:?}");
        assert!(dir.join(".sc/tests/default/expected/.github/workflows/ci.yml").exists());
        assert!(dir.join(".sc/tests/default/expected/.gitignore").exists());
        let readme = std::fs::read_to_string(dir.join(".sc/tests/default/expected/README.md")).unwrap();
        assert_eq!(readme, "# Example Project\n\nGenerated from the `Example Project` inputs by sc.\n");
    }
}
