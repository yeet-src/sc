use std::path::PathBuf;

use colored::Colorize;

use super::ui::{Show, confirm, print_outcome, print_plan};
use super::{Result, Vars, aborted, announce_hook};
use crate::scaffolding::apply::{self, Existing, Hooks};
use crate::scaffolding::manifest::CONTROL_DIR;
use crate::scaffolding::{inputs, source, update};

/// Change inputs and re-derive the tree, staying on the template version
/// the project was generated from. Files the new inputs stop producing are
/// removed if you never edited them; your edits are kept everywhere.
#[derive(clap::Args)]
pub struct Args {
    /// Project directory containing a .sc.toml answers file.
    #[arg(long, default_value = ".")]
    dest: PathBuf,
    /// Inputs to change, as KEY=VALUE. Repeatable.
    #[arg(value_name = "KEY=VALUE", value_parser = inputs::parse_override)]
    changes: Vec<(String, String)>,
    #[command(flatten)]
    vars: Vars,
    /// Take every input from a golden case of the template instead.
    #[arg(long = "case", value_name = "NAME", conflicts_with = "changes")]
    case: Option<String>,
    /// What to do with conflicting merges.
    #[arg(long, short = 'e', value_enum, default_value_t)]
    existing: Existing,
    /// Show a unified diff for files that would change.
    #[arg(long, short = 'd')]
    diff: bool,
    /// Apply without asking for confirmation.
    #[arg(long, short = 'y')]
    yes: bool,
    /// Only show the plan; write nothing.
    #[arg(long, short = 'p')]
    plan_only: bool,
    #[arg(long)]
    allow_shell: bool,
    #[arg(long)]
    no_hooks: bool,
    #[arg(long, short = 'o')]
    offline: bool,
}

pub async fn run(a: Args) -> Result {
    let mut vars = a.changes.clone();
    vars.extend(a.vars.vars.iter().cloned());
    let mut opts = update::Options {
        reference: None,
        vars,
        allow_shell: a.allow_shell,
        fetch: source::Options { offline: a.offline, cache_dir: None },
        pin: true,
        replace_inputs: false,
    };
    if let Some(case) = &a.case {
        // Resolve the template once to find its golden cases.
        let probe = update::prepare(&a.dest, &update::Options { vars: vec![], ..opts.clone() }).await?;
        let path = probe.loaded.root.join(CONTROL_DIR).join("tests").join(case).join("inputs.toml");
        let text = std::fs::read_to_string(&path).map_err(|e| format!("case `{case}`: {}: {e}", path.display()))?;
        let table: toml::Table = toml::from_str(&text)?;
        opts.vars = table.into_iter().map(|(k, v)| (k, inputs::override_text(&v))).collect();
        opts.replace_inputs = true;
    }
    if opts.vars.is_empty() {
        return Err("nothing to set; give KEY=VALUE pairs or --case NAME".into());
    }
    let prepared = update::prepare(&a.dest, &opts).await?;
    println!(
        "{} {}\n",
        "Setting".cyan().bold(),
        opts.vars.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ").green()
    );
    print_plan(&prepared.plan, &Show { contents: false, diff: a.diff, verbose: false });
    if a.plan_only {
        return Ok(());
    }
    if !a.yes && !confirm()? {
        aborted();
        return Ok(());
    }
    let hooks = if a.no_hooks { Hooks::Skip } else { Hooks::Run };
    let outcome = apply::apply(&prepared.plan, &apply::Options { existing: a.existing, hooks }, &mut announce_hook).await?;
    print_outcome(&outcome);
    Ok(())
}
