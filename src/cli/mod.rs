//! Command-line interface: argument definitions, dispatch and error display.
//! Each subcommand lives in its own file; `ui` owns all formatting.

mod apply;
mod dev;
mod diff;
mod enter;
mod new;
mod plan;
mod rederive;
mod status;
mod test;
mod tree;
mod ui;
mod r#where;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::builder::styling::{AnsiColor, Effects, Styles};
use clap::{Parser, Subcommand};
use colored::Colorize;

/// Help and error styling, matching the plan output's palette.
const STYLES: Styles = Styles::styled()
    .header(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
    .usage(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
    .literal(AnsiColor::Green.on_default())
    .placeholder(AnsiColor::Magenta.on_default())
    .valid(AnsiColor::Green.on_default())
    .invalid(AnsiColor::Red.on_default())
    .error(AnsiColor::Red.on_default().effects(Effects::BOLD));

use crate::scaffolding::{compose, inputs, source};

pub type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Parser)]
#[command(name = "sc", about = "Scaffolding engine", version, styles = STYLES, help_template = HELP)]
pub struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show what a template would make, without writing anything.
    Plan(plan::Args),
    /// Make a project from a template. The project remembers where it came from.
    Fork(apply::Args),
    /// Recompute a project from its template, newer version or new inputs, keeping your edits.
    Rederive(rederive::Args),
    /// Show a project's template, inputs, and local edits.
    Status(status::Args),

    /// Create a starter template.
    New(new::Args),
    /// Show a template's inputs and file tree without rendering it.
    Tree(tree::Args),
    /// Run a template's golden tests under .sc/tests/.
    Test(test::Args),
    /// Compare two renders of a template under different inputs or cases.
    Diff(diff::Args),
    /// Keep a scratch directory rendered from a template while you edit it.
    Dev(dev::Args),
    /// Open a shell inside a template, with a prompt that says where you are.
    Enter(enter::Args),
    /// Say where the current directory inside a template ends up in the output.
    Where(r#where::Args),

    /// Print a shell completion script.
    Completions(enter::CompletionsArgs),
    /// How to leave an sc shell.
    #[command(hide = true)]
    Exit(enter::ExitArgs),
    /// Alias of `fork`.
    #[command(hide = true)]
    Apply(apply::Args),
    /// Alias of `rederive` that always takes the newest template version.
    #[command(hide = true)]
    Pull(rederive::Args),
    /// Alias of `rederive` that always stays on the recorded version.
    #[command(hide = true)]
    Set(rederive::Args),
    /// Alias of `pull`.
    #[command(hide = true)]
    Update(rederive::Args),
}

const HELP: &str = "\
{before-help}{about}

{usage-heading} {usage}

A project is render(template, inputs) plus your edits. Inputs live in the
project's .sc.toml; files you never touched follow the template, files you
edited are yours.

Use a template:
  plan         show what a template would make, without writing anything
  fork         make a project from a template; it remembers where it came from
  rederive     recompute a project from its template, newer version or new inputs, keeping your edits
  status       show a project's template, inputs, and local edits

Make a template:
  new          create a starter template
  tree         show a template's inputs and file tree without rendering it
  test         run a template's golden tests
  diff         compare two renders under different inputs or cases
  dev          keep a scratch directory rendered while you edit
  enter        open a shell inside a template with a prompt that says where you are
  where        say where a directory inside a template ends up in the output

Other:
  completions  print a shell completion script
  help         print this message or the help of a subcommand

{options}{after-help}";
/// Arguments shared by every command that takes a template spec.
#[derive(clap::Args)]
pub struct Template {
    /// Template: a directory, `gh:owner/repo`, `gl:owner/repo`, or any git
    /// URL, optionally with `#ref`.
    pub template: String,
    /// Directory inside the repository that holds the template.
    #[arg(long, short = 's', value_name = "DIR")]
    pub subdir: Option<PathBuf>,
    /// Use the cached copy of a git template without contacting the remote.
    #[arg(long, short = 'o')]
    pub offline: bool,
    /// Layer another template under this one; its inputs join this one's
    /// and its files are overridden or merged by this one's. Repeatable.
    #[arg(long = "with", short = 'w', value_name = "TEMPLATE")]
    pub with: Vec<String>,
}

impl Template {
    async fn load(&self) -> std::result::Result<compose::Loaded, compose::ComposeError> {
        let fetch = source::Options { offline: self.offline, cache_dir: None };
        compose::load_with(&self.template, self.subdir.clone(), &self.with, &fetch).await
    }
}

/// `--var KEY=VALUE`, repeatable.
#[derive(clap::Args)]
pub struct Vars {
    /// Set a template variable. Repeatable.
    #[arg(long = "var", short = 'v', value_name = "KEY=VALUE", value_parser = inputs::parse_override)]
    vars: Vec<(String, String)>,
}

#[tokio::main]
pub async fn main() -> ExitCode {
    let result = match Cli::parse().cmd {
        Cmd::Plan(a) => plan::run(a).await,
        Cmd::Fork(a) | Cmd::Apply(a) => apply::run(a).await,
        Cmd::Rederive(a) => rederive::run(a, rederive::Mode::Auto).await,
        Cmd::Pull(a) | Cmd::Update(a) => rederive::run(a, rederive::Mode::Pull).await,
        Cmd::Set(a) => rederive::run(a, rederive::Mode::Set).await,
        Cmd::Status(a) => status::run(a).await,
        Cmd::New(a) => new::run(a).await,
        Cmd::Tree(a) => tree::run(a).await,
        Cmd::Test(a) => test::run(a).await,
        Cmd::Diff(a) => diff::run(a).await,
        Cmd::Dev(a) => dev::run(a).await,
        Cmd::Enter(a) => enter::run(a).await,
        Cmd::Where(a) => r#where::run(a).await,
        Cmd::Completions(a) => enter::completions(a),
        Cmd::Exit(a) => enter::exit(a).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{} {e}", "error:".red().bold());
            let mut src = std::error::Error::source(&*e);
            while let Some(s) = src {
                eprintln!("  {} {s}", "caused by:".dimmed());
                src = s.source();
            }
            ExitCode::FAILURE
        }
    }
}

/// `set`, `status` and `update` act on a generated project. Run inside a
/// template instead, say so and point at what works there.
fn require_project(dest: &std::path::Path) -> Result {
    use crate::scaffolding::plan::ANSWERS_FILE;
    if dest.join(ANSWERS_FILE).is_file() {
        return Ok(());
    }
    let abs = std::fs::canonicalize(dest).unwrap_or_else(|_| dest.to_path_buf());
    if crate::scaffolding::describe::find_template_root(&abs).is_some() {
        return Err(format!(
            "{} is a template, not a project forked from one.\n  preview inputs here:  sc tree . -v ci=false   or   sc plan . -v name=x --contents\n  fork it somewhere:    sc fork . <dir> -v name=x   and run this command there",
            dest.display()
        )
        .into());
    }
    Err(format!("{} has no {ANSWERS_FILE}; was it created by sc fork?", dest.display()).into())
}

fn announce_hook(h: &crate::scaffolding::plan::PlannedHook) {
    println!("\n{} {}", "running after hook:".cyan().bold(), h.name.green());
}

fn aborted() {
    println!("{}", "Aborted; nothing written.".yellow().bold());
}
