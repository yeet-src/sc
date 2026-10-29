use colored::Colorize;

use super::ui::print_diff;
use super::{Result, Template, Vars};
use crate::scaffolding::compare::{self, Change};
use crate::scaffolding::manifest::CONTROL_DIR;
use crate::scaffolding::plan::{Plan, PlanOptions};
use crate::scaffolding::{compose, inputs};

/// Compare two renders of one template: `sc diff tpl -v ci=false -- -v ci=true`,
/// or by golden case: `sc diff tpl --case minimal --case with-ci`.
#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    template: Template,
    /// Inputs for the first render.
    #[command(flatten)]
    vars: Vars,
    /// Inputs for the second render, after `--`.
    #[arg(last = true, value_name = "-v KEY=VALUE...")]
    second: Vec<String>,
    /// Golden case names to compare instead of inputs; give two.
    #[arg(long = "case", value_name = "NAME")]
    cases: Vec<String>,
    /// Permit the template to run commands while planning.
    #[arg(long)]
    allow_shell: bool,
    /// Emit the comparison as JSON instead of text.
    #[arg(long, short = 'j')]
    json: bool,
}

pub async fn run(a: Args) -> Result {
    let loaded = a.template.load().await?;
    let manifest = loaded.combined_manifest();
    let (first, second, labels) = match a.cases.as_slice() {
        [] => {
            let mut second = Vec::new();
            let mut it = a.second.iter();
            while let Some(arg) = it.next() {
                let kv = match arg.strip_prefix("-v").or_else(|| arg.strip_prefix("--var")) {
                    Some("") => it.next().cloned().unwrap_or_default(),
                    Some(rest) => rest.trim_start_matches('=').to_string(),
                    None => arg.clone(),
                };
                second.push(inputs::parse_override(&kv)?);
            }
            (a.vars.vars.clone(), second, ("first".to_string(), "second".to_string()))
        }
        [x, y] => (case_inputs(&loaded, x)?, case_inputs(&loaded, y)?, (x.clone(), y.clone())),
        _ => return Err("give exactly two --case names".into()),
    };
    let opts = PlanOptions { allow_shell: a.allow_shell, ..Default::default() };
    let render = |overrides: &[(String, String)]| -> Result<Plan> {
        let inputs = inputs::resolve(&manifest, overrides)?;
        Ok(loaded.plan(None, inputs, &opts)?)
    };
    let (pa, pb) = (render(&first)?, render(&second)?);
    let cmp = compare::compare(&pa, &pb);

    if a.json {
        serde_json::to_writer_pretty(std::io::stdout().lock(), &cmp)?;
        println!();
        return Ok(());
    }
    println!("{} {} {} {}\n", "Comparing".cyan().bold(), labels.0.magenta(), "->".cyan(), labels.1.magenta());
    if cmp.changes.is_empty() {
        println!("{}", "No differences.".green().bold());
        return Ok(());
    }
    let (mut added, mut removed, mut changed) = (0, 0, 0);
    for c in &cmp.changes {
        let p = c.path().display().to_string();
        match c {
            Change::Added { dir, .. } => {
                added += 1;
                println!("{} {}", "+".green().bold(), if *dir { format!("{p}/").blue().bold() } else { p.blue() });
            }
            Change::Removed { dir, .. } => {
                removed += 1;
                println!("{} {}", "-".red().bold(), if *dir { format!("{p}/").blue().bold() } else { p.blue() });
            }
            Change::Changed { path, before, after } => {
                changed += 1;
                println!("{} {}", "~".yellow().bold(), p.blue());
                if let (Some(b), Some(a)) = (before, after) {
                    print_diff(path, b.as_bytes(), a);
                }
            }
            Change::ModeChanged { exec, .. } => {
                changed += 1;
                println!("{} {}  {}", "~".yellow().bold(), p.blue(), if *exec { "now executable" } else { "no longer executable" }.magenta());
            }
        }
    }
    println!(
        "\n{} {}, {}, {}.",
        "Diff:".cyan().bold(),
        format!("{added} added").green().bold(),
        format!("{removed} removed").red().bold(),
        format!("{changed} changed").yellow().bold()
    );
    Ok(())
}

fn case_inputs(loaded: &compose::Loaded, case: &str) -> Result<Vec<(String, String)>> {
    let path = loaded.root.join(CONTROL_DIR).join("tests").join(case).join("inputs.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("case `{case}`: {}: {e}", path.display()))?;
    let table: toml::Table = toml::from_str(&text)?;
    Ok(table.into_iter().map(|(k, v)| (k, inputs::override_text(&v))).collect())
}
