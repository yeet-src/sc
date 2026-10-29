use std::path::PathBuf;

use super::ui::{Show, print_plan};
use super::{Result, Template, Vars};
use crate::scaffolding::inputs;
use crate::scaffolding::plan::PlanOptions;

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    template: Template,
    /// Destination directory. When given, each operation is compared
    /// against what is already there.
    dest: Option<PathBuf>,
    #[command(flatten)]
    vars: Vars,
    /// Also print the rendered contents of each text file.
    #[arg(long)]
    contents: bool,
    /// Show a unified diff for files that would be modified.
    #[arg(long)]
    diff: bool,
    /// Also list dropped template entries and full hook scripts.
    #[arg(long)]
    verbose: bool,
    /// Emit the plan as JSON instead of text.
    #[arg(long, conflicts_with_all = ["contents", "diff", "verbose"])]
    json: bool,
    /// Permit the template to run commands while planning (before hooks,
    /// generate rules, shell()). They are listed in the output.
    #[arg(long)]
    allow_shell: bool,
}

pub async fn run(a: Args) -> Result {
    let loaded = a.template.load().await?;
    let inputs = inputs::resolve(&loaded.combined_manifest(), &a.vars.vars)?;
    let plan = loaded.plan(a.dest.as_deref(), inputs, &PlanOptions { allow_shell: a.allow_shell, ..Default::default() })?;
    if a.json {
        serde_json::to_writer_pretty(std::io::stdout().lock(), &plan)?;
        println!();
    } else {
        print_plan(&plan, &Show { contents: a.contents, diff: a.diff, verbose: a.verbose });
    }
    Ok(())
}
