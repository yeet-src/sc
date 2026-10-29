use std::path::{Path, PathBuf};

use colored::Colorize;

use super::ui::{Show, print_plan};
use super::{Result, Template, Vars};
use crate::scaffolding::plan::{ANSWERS_FILE, PlanOptions};
use crate::scaffolding::{describe, inputs, source, update};

/// What would happen. Three shapes, told apart by what you give it:
///
///   sc plan <tpl> [dir] -v k=v          render a template; compare against dir if given
///   sc plan                             inside a project: what rederive would do, and what you edited
///   sc plan <tpl> -v a=1 -- -v a=2      two input sets: what differs between them
#[derive(clap::Args)]
pub struct Args {
    /// Template (a directory, `gh:owner/repo`, a git URL, `#ref` allowed),
    /// or a project directory. Defaults to the current directory.
    template: Option<String>,
    /// Destination directory. When given, each operation is compared
    /// against what is already there.
    dest: Option<PathBuf>,
    #[command(flatten)]
    vars: Vars,
    /// A second input set, after `--`: compare the two renders.
    #[arg(last = true, value_name = "-v KEY=VALUE...")]
    second: Vec<String>,
    /// Compare two golden cases by name instead of input sets; give two.
    #[arg(long = "case", value_name = "NAME")]
    cases: Vec<String>,
    /// Also print the rendered contents of each text file.
    #[arg(long, short = 'c')]
    contents: bool,
    /// Show a unified diff for files that would be modified.
    #[arg(long, short = 'd')]
    diff: bool,
    /// Also list dropped template entries and full hook scripts.
    #[arg(long)]
    verbose: bool,
    /// Emit the plan as JSON instead of text.
    #[arg(long, short = 'j', conflicts_with_all = ["contents", "diff", "verbose"])]
    json: bool,
    /// Permit the template to run commands while planning (before hooks,
    /// generate rules, shell()). They are listed in the output.
    #[arg(long)]
    allow_shell: bool,
    /// Directory inside the repository that holds the template.
    #[arg(long, short = 's', value_name = "DIR")]
    subdir: Option<PathBuf>,
    /// Use the cached copy of a git template without contacting the remote.
    #[arg(long, short = 'o')]
    offline: bool,
    /// Layer another template under this one. Repeatable.
    #[arg(long = "with", short = 'w', value_name = "TEMPLATE")]
    with: Vec<String>,
}

pub async fn run(a: Args) -> Result {
    let cwd = std::env::current_dir()?;
    let given = a.template.clone();
    let spec = given.clone().unwrap_or_else(|| ".".into());
    let as_path = Path::new(&spec);

    // A project: has an answers file and is not itself a template.
    let is_project = as_path.join(ANSWERS_FILE).is_file() && !as_path.join("sc.toml").is_file();
    if is_project {
        return project(as_path, &a).await;
    }

    // A template, named or found from the current directory.
    let spec = match (&given, describe::find_template_root(&cwd)) {
        (Some(s), _) => s.clone(),
        (None, Some(root)) => root.display().to_string(),
        (None, None) => return Err("not inside a template or a project; give a template or run from a project directory".into()),
    };
    let template = Template { template: spec, subdir: a.subdir.clone(), offline: a.offline, with: a.with.clone() };
    let loaded = template.load().await?;

    if !a.second.is_empty() || !a.cases.is_empty() {
        return super::diff::compare(&loaded, &a.vars.vars, &a.second, &a.cases, a.allow_shell, a.json).await;
    }

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

/// Inside a project: where it stands, and what `rederive` would do now.
async fn project(dest: &Path, a: &Args) -> Result {
    let opts = update::Options {
        reference: None,
        vars: a.vars.vars.clone(),
        allow_shell: a.allow_shell,
        fetch: source::Options { offline: a.offline, cache_dir: None },
        pin: !a.vars.vars.is_empty(),
        replace_inputs: false,
    };
    let p = update::prepare(dest, &opts).await?;
    if a.json {
        serde_json::to_writer_pretty(std::io::stdout().lock(), &p.plan)?;
        println!();
        return Ok(());
    }
    super::status::print(&p);
    println!("\n{}", "What rederive would do now".cyan().bold());
    print_plan(&p.plan, &Show { contents: a.contents, diff: a.diff, verbose: a.verbose });
    Ok(())
}
