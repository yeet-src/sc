# sc tutorial

Thirty minutes, two halves. First you use a template; then you make one.
Every command here runs against the repository's `examples/` directory, so
you can follow along verbatim.

The one rule to keep in mind: **a project is `render(template, inputs)`
plus your edits.** The inputs are written to `.sc.toml` in the project.
Files you never touched follow the template; files you edited are yours.

## Part 1: using a template

### Look before you render

```
sc where examples/python-package
```

```
Inputs
  name        string required  Package name; matches [a-z][a-z0-9_-]*
  description string = A small package  One-line description
  python      choice = 3.12  Minimum Python; one of 3.10, 3.11, 3.12
  license     choice = MIT  one of MIT, Apache-2.0, none
  ci          bool   = true  Add a GitHub Actions workflow?

Tree
python-package
├── .github/  when ci
│   └── workflows/
│       └── ci.yml
├── .gitignore
├── LICENSE  one of Apache-2.0, MIT by license; checked out MIT
├── README.md
├── pyproject.toml
├── src/
│   └── {{ name | snake_case }}/  rendered name
│       └── __init__.py
└── tests/
    └── test_version.py
```

The tree is the output tree. Three annotations tell you the rest: `.github/`
exists only `when ci`, `LICENSE` is one of two variants chosen by the
`license` input, and one directory name is rendered from `name`.

### Plan

```
sc plan examples/python-package ./tools -v name=yeet-tools -v license=Apache-2.0
```

Nothing is written. You get the inputs with where each came from, one line
per file with a status marker, the hooks that would run, and a summary.
Add `--contents` to read every rendered file, or `--diff` when the
destination already has files.

### Apply

```
sc fork examples/python-package ./tools -v name=yeet-tools -v license=Apache-2.0
```

Same plan, then a `[y/N]`, then the writes and the `git init` hook. In CI
pass `-y`. Open `tools/.sc.toml`: it records the template, its commit when
it came from git, and every input.

### Change your mind

Work in the project for a while. Edit `README.md`, add a dependency to
`pyproject.toml`. Then:

```
cd tools
sc plan
```

```
Files
  edited README.md
  edited pyproject.toml
```

Now turn CI off and switch the license:

```
sc rederive ci=false license=MIT
```

```
Edits     2 files modified locally since the last render  (kept; merged where the template also changed)
- README.md  (62 B, rendered)  kept by `README.md`
> pyproject.toml  (246 B, rendered, merge: toml)
x .github/workflows/ci.yml  no longer produced; unchanged locally, will be removed
```

Read that plan: your README is kept because the template says so, your
`pyproject.toml` edit is merged with the template's, and the workflow file
is removed because you never touched it. `sc rederive ci=true` brings it back.

### Follow the template

When the template's author publishes a new version:

```
sc rederive --diff --plan-only
sc rederive
```

For a git template, `rederive` renders the version you came from as a common
ancestor and three-way merges: untouched files take the new version, files
the template didn't change keep your edits, files both of you changed are
merged line by line, with conflict markers only where hunks overlap. Pin a
tag (`gh:org/tpl#v2`) and `rederive` moves only when you bump it with
`--ref`.

### Combine templates

```
sc fork ./project ./app --with gh:acme/ci-template#v1 -v name=app
```

Inputs are the union of both templates. Where both produce a path and
either has a merge rule for it, the files merge; otherwise the main
template wins. The combination is recorded, so `rederive` applies it again.

## Part 2: making a template

### Start

```
sc new my-template
cd my-template
```

`sc new` writes a manifest with every section commented, two files, and a
golden test that already passes. Inside the template, `sc where` shows its
inputs and output tree, and `sc where .github` says what that directory
becomes and when.

### Declare inputs

Edit `sc.toml`:

```toml
[template]
name = "my-template"
description = "What it makes"

[vars.name]
prompt = "Project name"
pattern = "[a-z][a-z0-9_-]*"

[vars.license]
type = "choice"
choices = ["MIT", "Apache-2.0", "none"]
default = "MIT"

[vars.ci]
type = "bool"
default = true
```

Types are `string`, `bool`, `int`, `choice`, and `list`. Prefer `choice`
over free strings whenever a value selects structure: a typo then fails at
plan time instead of silently producing a different tree.

### Write files

Files are files. `{{ name }}` inside them is rendered, and so are names:

```
src/{{ name | snake_case }}/__init__.py
```

Filters: `snake_case`, `kebab_case`, `pascal_case`, `camel_case`,
`shouty_snake_case`, `title_case`, plus everything Jinja has. An undefined
variable is an error, never blank output. Shared snippets go under
`.sc/partials/` and are pulled in with `{% include "partials/x" %}`.

Preview constantly:

```
sc plan . -v name=demo --contents
```

### Say what depends on what

Per-path rules live in one table keyed by output path:

```toml
[tree]
".github/"     = { when = "ci" }
"Dockerfile"   = { when = "deploy == 'container'" }
"README.md"    = { keep = true }
"Cargo.toml"   = { merge = "toml" }
".gitignore"   = { merge = "append-lines" }
"assets/**"    = { raw = true }
```

`when` is a Jinja expression over the inputs: comparisons, `and`, `or`,
`not`, `in`, filters, tests. `keep` means create once, never overwrite.
`merge` combines with an existing file (`toml`, `json`, `yaml`,
`append-lines`, or `insert` with a `marker`). `raw` copies bytes
unrendered, for files that contain `{{` of their own.

Check it without rendering:

```
sc where . -v ci=false
```

### Variants

When a path has several versions, put them in a directory named after the
output and the input that picks:

```
LICENSE@license/
  MIT
  Apache-2.0
```

The child whose name equals the input's value is emitted as `LICENSE`; no
match emits nothing (so `license = "none"` works with no file). Directories
work the same way: `config@env/dev/` and `config@env/prod/`. That's the
whole mechanism: no declaration, no state, just files you can see.

`sc where . -v license=MIT` marks the chosen one.

### Lock the output

A golden case is an `inputs.toml` and the tree it must produce:

```
mkdir -p .sc/tests/minimal
printf 'name = "tiny"\nci = false\nlicense = "none"\n' > .sc/tests/minimal/inputs.toml
sc test . --update          # writes .sc/tests/minimal/expected/
sc test .                   # compares, from now on
```

Commit the expected trees. A template change that alters output fails
`sc test` with the exact hunk, and `sc test --update` after an intended
change makes the diff reviewable in git. Compare cases directly:

```
sc plan . --case minimal --case default
```

### Hooks

```toml
[[hooks]]
name = "git init"
phase = "after"
when = "{{ git }}"
run = "[ -d .git ] || git init -q"
```

After hooks run in the destination once files are written, with inputs as
`SC_VAR_NAME`. They're listed in every plan before the user confirms.
Anything that would run *before* confirmation, such as `before` hooks or
the `shell()` template function, is refused without `--allow-shell`.

### Watch it live

```
sc fork . ../scratch -v name=demo --watch
```

`../scratch` re-renders whenever you save a template file. Its `.sc.toml`
is the switchboard: set `ci = false` there and `.github/` disappears from
the scratch tree. Edits inside the scratch tree are overwritten; it's a
view.

### Ship it

Commit the template with its `.sc/tests/`. If it lives in a repository of
its own, put it under `template/` so the template's `.github/` isn't the
repository's CI; `sc` finds `template/sc.toml` by itself. Tag releases, and
tell users to pin them: `gh:you/my-template#v1`.

## Cheat sheet

| I want to | Command |
| --- | --- |
| see what a template asks and makes | `sc where <tpl>` or, inside it, `sc where` |
| see what would happen | `sc plan <tpl> <dir> -v k=v` |
| make a project | `sc fork <tpl> <dir> -v k=v` |
| see what I've edited, and what rederive would do | `sc plan` (inside the project) |
| change inputs | `sc rederive k=v` |
| take a newer template version | `sc rederive` |
| combine two templates | `sc fork <tpl> <dir> --with <tpl2>` |
| start a template | `sc new <dir>` |
| compare two input sets | `sc plan <tpl> -v a=1 -- -v a=2` |
| check golden tests | `sc test <tpl>` |
| keep a rendered tree in sync while editing | `sc fork <tpl> <scratch> --watch` |
