# sc

Scaffolding engine. Point it at a template, give it a few inputs, and it
renders a project. Point it at the same project later and it merges in what
the template changed since, without losing your edits.

```
sc apply gh:yeet-src/templates --subdir rust-cli ./my-app -v name="My App"
```

`sc` never writes before showing you a plan, and the plan tells you exactly
what each file will do: create, modify, merge, keep, or conflict.

**The one rule.** A project is `render(template @ version, inputs)` plus your
edits on top. The inputs live in `.sc.toml`, and nothing else is state.
Every command either looks (`plan`, `tree`, `diff`, `where`, `status`,
`test`), changes the inputs (`set`), or changes the template version
(`update`). Files you never touched follow the template, including going
away when an input stops producing them; files you edited are yours.

New here? Read the [tutorial](TUTORIAL.md).

## Install

Download a static binary for macOS or Linux (x86_64 or arm64) from the
[latest release](https://github.com/yeet-src/sc/releases/latest), or build
from source:

```
cargo install --git https://github.com/yeet-src/sc
```

Git templates need `git` and `tar` on the path.

## Commands

| Command | What it does |
| --- | --- |
| `sc plan <template> [dest]` | Show what would happen. Writes nothing. |
| `sc apply <template> <dest>` | Show the plan, confirm, write. |
| `sc update [dest]` | Re-apply the template a project came from, at its newest version. |
| `sc set KEY=VALUE...` | Change a project's inputs and re-derive its tree, same template version. |
| `sc status [dest]` | Show a project's template, inputs, and files edited since the last render. |
| `sc test <template>` | Run a template's golden tests. |
| `sc tree <template>` | Show a template's inputs and file tree without rendering it. |
| `sc diff <template>` | Compare two renders under different inputs or golden cases. |
| `sc dev <template> <scratch>` | Keep a scratch directory rendered while you edit the template. |
| `sc where [path]` | Say where a directory inside a template ends up in the output. |
| `sc enter [template]` | Open a shell inside a template with a prompt that says where you are. |
| `sc checkout KEY=VALUE...` | Point a template's variant links at the variants these inputs select. |
| `sc variant add GROUP VALUE` | Add a variant to a group's store. |
| `sc new <dir>` | Create a starter template. |

A template is a directory, `gh:owner/repo`, `gl:owner/repo`, or any git URL.
Add `#ref` for a branch, tag or commit, and `--subdir` when the template lives
inside a larger repository. Git templates are cached under `~/.cache/sc`;
`--offline` uses the cache without contacting the remote.

Inputs are given with `-v name=value`, repeatable. Anything the template
declares and you do not pass must have a default, or the command stops and
tells you what is missing.

### Reading a plan

```
Template  rust-cli  A minimal Rust command-line application
Source    examples/rust-cli
Dest      ./my-app

Inputs
  name    = "My App"  (flag)
  edition = "2024"    (default)
  ci      = true      (default)

+ .github/workflows/ci.yml  (144 B, rendered)
> Cargo.toml  (109 B, rendered, merge: toml)
- README.md  (29 B, rendered)  kept by `README.md`
= src/main.rs  (52 B, rendered)

Plan: 1 to create, 1 to merge, 1 kept, 1 unchanged. 253 B to write.
```

| Marker | Meaning |
| --- | --- |
| `+` | Nothing exists there; the file will be created. |
| `~` | A file exists with different content; it will be replaced. |
| `>` | A file exists and a merge rule combines the two. |
| `*` | `update` three-way merged your edits with the template's changes. Red means conflict markers. |
| `=` | Already identical. Nothing happens. |
| `-` | A `keep` rule says leave it alone. |
| `!` | Something incompatible is there, like a directory where a file goes. |

`--diff` shows the hunks for anything marked `~`, `>` or `*`. `--contents`
prints every rendered file. `--verbose` lists what the template dropped and
why. `--json` gives the whole plan as data.

### Existing files

By default `apply` refuses to run if any file would be replaced or conflicts.
`--existing skip` leaves those files alone and writes the rest.
`--existing overwrite` replaces them. Conflicts, where a directory sits where
a file goes, are never resolved automatically.

Writes are staged and then renamed into place, so a failure part-way leaves
the destination as it was. If a file changed between plan and apply, `sc`
notices and stops.

### Changing inputs

```
sc set ci=false                 # the .github/ directory goes away, if you never edited it
sc set --case with-ci           # switch to a golden case's complete input set
sc status                       # template, version, inputs, and local edits
```

`set` stays on the template version the project was generated from and
re-derives the tree from the new inputs. A file the new inputs no longer
produce is removed if it still holds what the previous render wrote, and
kept if you changed it. The plan shows removals as `x` and says how many
files carry local edits.

### Updating

Every apply writes `.sc.toml` into the destination recording the template,
its commit if it came from git, and every input. `sc update` reads it,
fetches the newest version of the template, and plans against the project.

For git templates the previous commit is rendered as a common ancestor, so:

- a file you never touched takes the new version;
- a file the template did not change keeps your edits;
- a file both of you changed is merged, with conflict markers where hunks
  overlap. Conflicted files are refused unless you pass `--existing overwrite`.

`--ref` moves to a different branch or tag. `-v` changes an input; everything
else comes from the answers file. `--plan-only` shows without writing.

Pin templates to a tag or commit (`gh:org/repo#v2`) if you want `update` to
move only when you say so.

## Writing a template

`sc new my-template` writes a starter you can edit, and `sc tree my-template`
shows what any template asks for and how each of its files will be treated.
The pieces:

```
my-template/
  sc.toml                   manifest (optional)
  .sc/                      partials, hook scripts, tests; never emitted
  README.md                 {{ name }} inside files is rendered
  src/{{ name | snake_case }}.rs      and inside paths
  .gitignore                a real dotfile; nothing special
  .github/workflows/ci.yml  present only when [tree] says so, see below
  LICENSE                   a link to the checked-out variant, see Variants
  .sc/variants/LICENSE/     MIT, Apache-2.0
```

Templates use [minijinja](https://github.com/mitsuhiko/minijinja), a Jinja2
implementation. Every input is a variable. Filters `snake_case`, `kebab_case`,
`pascal_case`, `camel_case`, `shouty_snake_case` and `title_case` are added to
the built-in set. An undefined variable is an error, never silent empty text.
`{% include "partials/header.md" %}` resolves against `.sc/`.

A directory with no manifest is a valid template. Every `-v` is then a string.

**Or watch it live.** `sc dev my-template ./scratch -v name=Demo` renders
into `./scratch` and re-renders whenever the template changes. The scratch
tree's `.sc.toml` is the switchboard: set `ci = false` in it and the
`.github/` directory disappears. Edits inside the scratch tree are
overwritten, so treat it as a view.

**Or work inside it.** `sc enter my-template` opens a shell at the
template with `sc` completions loaded, `SC_TEMPLATE` set, and a line above
the prompt saying what the current directory becomes:

```
rust-cli › .github/workflows/  when ci
~/my-template/.github/workflows %
```

`exit` leaves it. For your own prompt, `sc where --prompt zsh` (or `bash`)
prints that line, colored and escaped for the shell, and nothing outside a
template.

```
~/tpl/.sc/variants/config/prod   [rust-cli config variant prod]
~/tpl/.github/workflows     [rust-cli .github/workflows/ when ci]
~/tpl/.sc/partials          [rust-cli never emitted control]
```

### The manifest

```toml
[template]
name = "rust-cli"
description = "A minimal Rust command-line application"

[vars.name]                       # asked for in this order
prompt = "Project name"
pattern = "[A-Za-z][A-Za-z0-9 _-]*"

[vars.edition]
type = "choice"
choices = ["2021", "2024"]
default = "2024"

[vars.ci]
type = "bool"
default = true

[vars.services]                   # lists are JSON on the command line
type = "list"
default = [{ name = "api", port = 8080 }]
```

Variable types: `string` (default), `bool`, `int`, `choice`, `list`.

### The tree section

Per-path rules live in one table keyed by output path. Globs are allowed,
and a trailing slash covers a directory and everything under it.

```toml
[tree]
".github/"     = { when = "ci" }                  # dropped when the condition is false
"Dockerfile"   = { when = "deploy == 'container'" }
"Makefile"     = { when = "not docker" }
"README.md"    = { keep = true }                  # created if absent, never touched again
"Cargo.toml"   = { merge = "toml" }               # deep merge, keeps your comments and layout
"package.json" = { merge = "json" }
"k8s/*.yaml"   = { merge = "yaml" }               # deep merge; comments are not preserved
".gitignore"   = { merge = "append-lines" }       # adds lines the file lacks
"src/lib.rs"   = { merge = "insert", marker = "// sc:modules" }
"assets/**"    = { raw = true }                   # copied byte for byte, paths still rendered
```

`when` is a template expression over the inputs, so anything `{% if %}`
accepts works. `sc tree my-template -v ci=false` shows which entries a set
of inputs turns off, and `sc diff my-template -v ci=false -- -v ci=true`
shows exactly what changes between two sets.

### Variants

A path can have several versions, one per value of an input. Declare the
group in the manifest and keep the versions in the store:

```toml
[tree]
"LICENSE" = { variants = "license" }      # one of .sc/variants/LICENSE/*
"config/" = { variants = "env" }          # directories work the same way
```

```
.sc/variants/LICENSE/MIT
.sc/variants/LICENSE/Apache-2.0
.sc/variants/config/dev/app.yaml
.sc/variants/config/prod/app.yaml
```

The one whose name (minus an alphabetic extension) equals the input's value
is emitted at the group's path. No match emits nothing, and `sc plan
--verbose` says so, so declare the input as a `choice`.

In the template's working tree the group's path is a symlink to the
checked-out variant, so `ls` shows `LICENSE`, your editor opens it, and
saving edits that variant:

```
sc checkout license=Apache-2.0       # LICENSE -> .sc/variants/LICENSE/Apache-2.0
sc checkout --case with-ci           # every group, from a golden case's inputs
sc checkout                          # what is checked out now
sc variant add LICENSE GPL-3.0       # a new variant, copied from the checked-out one
sc variant list
```

The planner always reads the store, never the links, so what is checked
out never changes what a template produces.

### Other file rules

```toml
[files]
exclude = ["*.orig"]              # never emitted; matched against template paths
dot_prefix = "_"                  # off by default: _gitignore -> .gitignore, for
                                  # registries that strip dotfiles on publish

[files.rename]                    # explicit names, instead of or alongside _
"gitignore" = ".gitignore"
"github/" = ".github/"

[[files.syntax]]                  # for files that use {{ }} themselves
path = "charts/**"
variable = ["[[", "]]"]
block = ["[%", "%]"]
comment = ["[#", "#]"]
```

The older forms `[files] keep`, `[files] raw` and `[files.merge]` still
work and mean the same as the `[tree]` fields.

Binary files are detected and copied as-is.

### Hooks

```toml
[[hooks]]
name = "git init"
phase = "after"                   # after: in the destination, once files are written
when = "{{ git }}"                # skipped when this renders false, 0, no or empty
run = """
set -e
git init -q
"""

[[hooks]]
phase = "before"                  # before: in the template, before planning
script = "hooks/check.sh"         # under .sc/, run via its shebang
```

Hooks see the inputs as `SC_VAR_<NAME>` plus `SC_TEMPLATE` and `SC_DEST`.
Their text is rendered as a template first (`render = false` to skip). The
plan lists every hook before you confirm. `--no-hooks` skips them.

A `before` hook that prints a JSON object adds those keys as inputs.

### Generated files and `shell()`

```toml
[[files.generate]]
path = "LICENSE"
run = "curl -s https://choosealicense.com/licenses/{{ license }}.txt"
once = true                       # only when the file does not exist yet
```

Inside any template, `{{ shell("git config user.name") }}` expands to the
command's output.

Anything that runs *before* you confirm, meaning `before` hooks, `generate`
rules and `shell()`, is refused unless you pass `--allow-shell`. The refusal
lists the commands; the plan shows what ran under "Ran while planning".

### Composing templates

Templates stack. A layer underneath contributes its inputs, files and hooks;
the layer on top overrides files path by path, except where a merge rule on
either side says to combine them instead. So a CI template with
`".gitignore" = "append-lines"` adds its lines to the project's `.gitignore`
rather than replacing it.

From the command line, for any combination:

```
sc apply ./project ./my-app --with gh:org/ci-template#v2 -v name="My App"
```

`--with` is repeatable, asks for the union of all inputs, and is recorded in
the answers file so `sc update` applies the same layers again.

In a manifest, for a fixed combination:

```toml
[[uses]]                          # layer this template over another
source = "gh:org/base#v3"         # files here override the base's, path by path

[[mounts]]                        # place a sub-template at a path, once per element
path = "services/{{ item.name }}"
template = "parts/service"        # a directory in this template, or a git spec
for = "services"                  # a list input; item and index are available
inputs = { name = "{{ item.name }}", port = "{{ item.port }}" }
```

A mounted template has its own manifest and rules. Its hooks run in its
mount directory with its own inputs. Where the outer template and a mount
produce the same path, the outer one wins unless a merge rule combines them.

### Golden tests

```
.sc/tests/<case>/inputs.toml      the inputs for this case
.sc/tests/<case>/expected/        the output tree they must produce
```

`sc test my-template --update` writes the expected trees; `sc test
my-template` compares against them and shows a diff for anything that
changed. Commit the expected trees with the template.

## Examples

- [`examples/rust-cli`](examples/rust-cli): a single project with a
  conditional CI directory, merge and keep rules, and a hook.
- [`examples/monorepo`](examples/monorepo): list inputs and mounts, one crate
  per service.

```
sc plan examples/rust-cli -v name="Hello World" --contents
sc apply examples/monorepo ./shop -v name=Shop -v 'services=[{"name":"api","port":8080}]'
```

## Releases and the commit convention

A push to `master` publishes a release with binaries for all four targets
only when it contains a commit whose subject line carries a marker. Pushes
without one publish nothing.

| Marker | Bump |
| --- | --- |
| `[bump:patch]` | X.Y.Z+1 |
| `[bump:minor]` | X.Y+1.0 |
| `[bump:major]` | X+1.0.0 |

The highest level among the commits since the previous release wins, and the
first release counts up from 0.0.0. Releases from a `release/vX.Y`
maintenance branch never become "Latest". A pull-request check rejects
misspelled markers. `Cargo.toml` carries a placeholder version; the computed
one is stamped in at build time.
