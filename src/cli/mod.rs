//! Command-line interface: argument definitions, dispatch and error display.
//! Each subcommand lives in its own file; `ui` owns all formatting.

mod apply;
mod new;
mod plan;
mod test;
mod tree;
mod ui;
mod update;

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
#[command(name = "sc", about = "Scaffolding engine", version, styles = STYLES)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Compute and print the operations a template would perform, without
    /// writing anything.
    Plan(plan::Args),
    /// Render a template into a destination directory.
    Apply(apply::Args),
    /// Re-apply the template a project was generated from, at its latest
    /// version, merging template changes with local edits.
    Update(update::Args),
    /// Run a template's golden tests under .sc/tests/.
    Test(test::Args),
    /// Create a starter template with a commented manifest and a golden test.
    New(new::Args),
    /// Show a template's input schema and file tree without rendering it.
    Tree(tree::Args),
}

/// Arguments shared by every command that takes a template spec.
#[derive(clap::Args)]
pub struct Template {
    /// Template: a directory, `gh:owner/repo`, `gl:owner/repo`, or any git
    /// URL, optionally with `#ref`.
    template: String,
    /// Directory inside the repository that holds the template.
    #[arg(long, value_name = "DIR")]
    subdir: Option<PathBuf>,
    /// Use the cached copy of a git template without contacting the remote.
    #[arg(long)]
    offline: bool,
}

impl Template {
    async fn load(&self) -> std::result::Result<compose::Loaded, compose::ComposeError> {
        compose::load(&self.template, self.subdir.clone(), &source::Options { offline: self.offline, cache_dir: None }).await
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
        Cmd::Apply(a) => apply::run(a).await,
        Cmd::Update(a) => update::run(a).await,
        Cmd::Test(a) => test::run(a).await,
        Cmd::New(a) => new::run(a).await,
        Cmd::Tree(a) => tree::run(a).await,
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

fn announce_hook(h: &crate::scaffolding::plan::PlannedHook) {
    println!("\n{} {}", "running after hook:".cyan().bold(), h.name.green());
}

fn aborted() {
    println!("{}", "Aborted; nothing written.".yellow().bold());
}
