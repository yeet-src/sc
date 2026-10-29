use std::path::PathBuf;

use colored::Colorize;

use super::ui::{Show, confirm, print_outcome, print_plan};
use super::{Result, Vars, aborted, announce_hook};
use crate::apply::{self, Existing, Hooks};
use crate::{source, update};

#[derive(clap::Args)]
pub struct Args {
    /// Project directory containing a .sc.toml answers file.
    #[arg(default_value = ".")]
    dest: PathBuf,
    /// Move to this branch, tag or commit instead of the recorded ref.
    #[arg(long = "ref", value_name = "REF")]
    reference: Option<String>,
    #[command(flatten)]
    vars: Vars,
    /// What to do with files that conflict or were modified outside a merge.
    #[arg(long, value_enum, default_value_t)]
    existing: Existing,
    /// Show a unified diff for files that would change.
    #[arg(long)]
    diff: bool,
    /// Apply without asking for confirmation.
    #[arg(long, short = 'y')]
    yes: bool,
    /// Only show the plan; write nothing.
    #[arg(long)]
    plan_only: bool,
    /// Permit the template to run commands while planning.
    #[arg(long)]
    allow_shell: bool,
    /// Do not run `after` hooks.
    #[arg(long)]
    no_hooks: bool,
    /// Use the cached copy of a git template without contacting the remote.
    #[arg(long)]
    offline: bool,
}

pub fn run(a: Args) -> Result {
    let opts = update::Options {
        reference: a.reference,
        vars: a.vars.vars,
        allow_shell: a.allow_shell,
        fetch: source::Options { offline: a.offline, cache_dir: None },
    };
    let prepared = update::prepare(&a.dest, &opts)?;
    let short = |c: &str| c[..c.len().min(12)].to_string();
    match (&prepared.previous, &prepared.plan.template.git) {
        (Some(old), Some(new)) if *old != new.commit => println!(
            "{} {} {} {}\n",
            "Updating".cyan().bold(),
            short(old).magenta(),
            "->".cyan(),
            short(&new.commit).magenta()
        ),
        (Some(_), Some(_)) => println!("{}\n", "Already at the latest commit; re-checking inputs and files.".yellow()),
        _ => println!("{}\n", "Local template: no recorded ancestor, so local edits are not merged.".yellow()),
    }
    print_plan(&prepared.plan, &Show { contents: false, diff: a.diff, verbose: false });
    if a.plan_only {
        return Ok(());
    }
    if !a.yes && !confirm()? {
        aborted();
        return Ok(());
    }
    let hooks = if a.no_hooks { Hooks::Skip } else { Hooks::Run };
    let outcome = apply::apply(&prepared.plan, &apply::Options { existing: a.existing, hooks }, &mut announce_hook)?;
    print_outcome(&outcome);
    Ok(())
}
