use std::path::PathBuf;

use super::ui::{Show, confirm, print_outcome, print_plan};
use super::{Result, Template, Vars, aborted, announce_hook};
use crate::scaffolding::apply::{self, Existing, Hooks};
use crate::scaffolding::inputs;
use crate::scaffolding::plan::PlanOptions;

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    template: Template,
    /// Destination directory; created if it does not exist.
    dest: PathBuf,
    #[command(flatten)]
    vars: Vars,
    /// What to do with files that already exist with different content.
    #[arg(long, short = 'e', value_enum, default_value_t)]
    existing: Existing,
    /// Show a unified diff for files that would be modified.
    #[arg(long, short = 'd')]
    diff: bool,
    /// Apply without asking for confirmation.
    #[arg(long, short = 'y')]
    yes: bool,
    /// Emit the outcome as JSON instead of text. Implies --yes.
    #[arg(long, short = 'j', conflicts_with = "diff")]
    json: bool,
    /// Permit the template to run commands while planning (before hooks,
    /// generate rules, shell()). They are listed in the output.
    #[arg(long)]
    allow_shell: bool,
    /// Do not run `after` hooks.
    #[arg(long)]
    no_hooks: bool,
    /// Keep the destination rendered as the template changes: re-fork on
    /// every save, treat its .sc.toml as the switchboard, overwrite edits
    /// inside it. For working on a template. Ctrl-C to stop.
    #[arg(long)]
    watch: bool,
    /// With --watch: how often to look for changes, in milliseconds.
    #[arg(long, default_value_t = 500)]
    interval: u64,
}

pub async fn run(a: Args) -> Result {
    if a.watch {
        return super::dev::watch(&a.template, &a.dest, &a.vars.vars, a.allow_shell, a.interval).await;
    }
    let loaded = a.template.load().await?;
    let inputs = inputs::resolve(&loaded.combined_manifest(), &a.vars.vars)?;
    let plan = loaded.plan(Some(&a.dest), inputs, &PlanOptions { allow_shell: a.allow_shell, ..Default::default() })?;
    if !a.json {
        print_plan(&plan, &Show { contents: false, diff: a.diff, verbose: false });
        if !a.yes && !confirm()? {
            aborted();
            return Ok(());
        }
    }
    let hooks = match (a.no_hooks, a.json) {
        (true, _) => Hooks::Skip,
        (false, true) => Hooks::Capture,
        (false, false) => Hooks::Run,
    };
    let mut announce = |h: &crate::scaffolding::plan::PlannedHook| {
        if !a.json {
            announce_hook(h);
        }
    };
    let outcome = apply::apply(&plan, &apply::Options { existing: a.existing, hooks }, &mut announce).await?;
    if a.json {
        serde_json::to_writer_pretty(std::io::stdout().lock(), &outcome)?;
        println!();
    } else {
        print_outcome(&outcome);
    }
    Ok(())
}
