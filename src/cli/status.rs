use std::path::PathBuf;

use colored::Colorize;

use super::Result;
use crate::scaffolding::update;
use crate::scaffolding::{plan::Status, source};

/// Where a generated project stands: which template and version it came
/// from, its inputs, and which files were edited since the last render.
#[derive(clap::Args)]
pub struct Args {
    /// Project directory containing a .sc.toml answers file.
    #[arg(default_value = ".")]
    dest: PathBuf,
    /// Use the cached copy of a git template without contacting the remote.
    #[arg(long, short = 'o')]
    offline: bool,
}

pub async fn run(a: Args) -> Result {
    let opts = update::Options {
        fetch: source::Options { offline: a.offline, cache_dir: None },
        pin: true,
        ..Default::default()
    };
    let p = update::prepare(&a.dest, &opts).await?;
    let t = &p.plan.template;
    let label = |s: &str| format!("{:<9}", s).cyan().bold();
    match (&t.name, &t.git) {
        (name, Some(g)) => println!(
            "{} {}  {} {}",
            label("Template"),
            name.clone().unwrap_or_default().green().bold(),
            g.url.blue(),
            format!("@ {}", &g.commit[..g.commit.len().min(12)]).magenta()
        ),
        (name, None) => println!("{} {}  {}", label("Template"), name.clone().unwrap_or_default().green().bold(), t.source.display().to_string().blue()),
    }
    if !t.with.is_empty() {
        println!("{} {}", label("With"), t.with.join(", ").green());
    }
    println!("\n{}", "Inputs".cyan().bold());
    let width = p.plan.inputs.0.iter().map(|i| i.name.len()).max().unwrap_or(0);
    for i in &p.plan.inputs.0 {
        let value = serde_json::to_string(&i.value).unwrap_or_default();
        println!("  {} {} {}", format!("{:<width$}", i.name).yellow(), "=".magenta(), value.green());
    }

    let edited = &p.plan.local_edits;
    let stale: Vec<_> = p.plan.ops.iter().filter(|o| matches!(o.status, Status::Modify { .. } | Status::Update { .. } | Status::Merge { .. })).map(|o| o.path.clone()).collect();
    println!("\n{}", "Files".cyan().bold());
    if edited.is_empty() {
        println!("  {}", "no local edits since the last render".green());
    } else {
        for e in edited {
            println!("  {} {}", "edited".yellow().bold(), e.display().to_string().blue());
        }
    }
    if !stale.is_empty() {
        println!(
            "  {}",
            format!("{} file(s) would change if re-rendered now; run `sc set` or `sc update --plan-only` to see", stale.len()).magenta()
        );
    }
    Ok(())
}
