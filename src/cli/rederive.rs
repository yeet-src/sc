use std::path::PathBuf;

use colored::Colorize;

use super::ui::{Show, confirm, print_outcome, print_plan};
use super::{Result, Vars, aborted, announce_hook};
use crate::scaffolding::apply::{self, Existing, Hooks};
use crate::scaffolding::manifest::CONTROL_DIR;
use crate::scaffolding::{inputs, source, update};

/// Recompute a project from its template and replay your edits on top.
///
/// With no inputs given, moves to the template's newest version (or
/// `--ref`). With inputs given and no `--ref`, stays on the recorded
/// version and changes only the inputs. Files you never touched follow the
/// template, including going away; files you edited are kept, and merged
/// three ways where the template changed them too.
#[derive(clap::Args)]
pub struct Args {
    /// Inputs to change, as KEY=VALUE. Repeatable.
    #[arg(value_name = "KEY=VALUE", value_parser = inputs::parse_override)]
    changes: Vec<(String, String)>,
    #[command(flatten)]
    vars: Vars,
    /// Project directory containing a .sc.toml answers file.
    #[arg(long, default_value = ".")]
    dest: PathBuf,
    /// Move to this branch, tag or commit of the template.
    #[arg(long = "ref", short = 'r', value_name = "REF")]
    reference: Option<String>,
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
    /// Use the cached copy of a git template without contacting the remote.
    #[arg(long, short = 'o')]
    offline: bool,
}

/// How the aliases bias the version choice.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `rederive`: newest version unless inputs were given without --ref.
    Auto,
    /// `pull`: always the newest version (or --ref).
    Pull,
    /// `set`: always the recorded version.
    Set,
}

pub async fn run(a: Args, mode: Mode) -> Result {
    super::require_project(&a.dest)?;
    let mut vars = a.changes.clone();
    vars.extend(a.vars.vars.iter().cloned());
    let inputs_given = !vars.is_empty() || a.case.is_some();
    let pin = match mode {
        Mode::Pull => false,
        Mode::Set => true,
        Mode::Auto => inputs_given && a.reference.is_none(),
    };
    let mut opts = update::Options {
        reference: a.reference.clone(),
        vars,
        allow_shell: a.allow_shell,
        fetch: source::Options { offline: a.offline, cache_dir: None },
        pin,
        replace_inputs: false,
    };
    if let Some(case) = &a.case {
        let probe = update::prepare(&a.dest, &update::Options { vars: vec![], ..opts.clone() }).await?;
        let path = probe.loaded.root.join(CONTROL_DIR).join("tests").join(case).join("inputs.toml");
        let text = std::fs::read_to_string(&path).map_err(|e| format!("case `{case}`: {}: {e}", path.display()))?;
        let table: toml::Table = toml::from_str(&text)?;
        opts.vars = table.into_iter().map(|(k, v)| (k, inputs::override_text(&v))).collect();
        opts.replace_inputs = true;
    }
    if mode == Mode::Set && opts.vars.is_empty() {
        return Err("nothing to set; give KEY=VALUE pairs or --case NAME".into());
    }

    let prepared = update::prepare(&a.dest, &opts).await?;
    let short = |c: &str| c[..c.len().min(12)].to_string();
    let what = match (&prepared.previous, &prepared.plan.template.git) {
        (Some(old), Some(new)) if *old != new.commit => format!("{} {} {}", short(old).magenta(), "->".cyan(), short(&new.commit).magenta()),
        (Some(_), Some(_)) => "same template version".yellow().to_string(),
        _ => "local template; no recorded version, so only files you edited are protected".yellow().to_string(),
    };
    let changes = if opts.vars.is_empty() {
        String::new()
    } else {
        format!("  {}", opts.vars.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ").green())
    };
    println!("{} {what}{changes}\n", "Rederive".cyan().bold());
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
