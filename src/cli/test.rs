use colored::Colorize;

use super::ui::print_diff;
use super::{Result, Template};
use crate::scaffolding::golden::{self, Finding};

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    template: Template,
    /// Run only this case.
    case: Option<String>,
    /// Rewrite each case's expected/ tree from the current output.
    #[arg(long, short = 'u')]
    update: bool,
}

pub async fn run(a: Args) -> Result {
    let loaded = a.template.load().await?;
    let results = golden::run(&loaded, a.case.as_deref(), a.update)?;
    let mut failed = 0;
    for r in &results {
        if a.update {
            println!("{} {}", "updated".cyan().bold(), r.name.blue());
            continue;
        }
        if r.passed() {
            println!("{} {}", "pass".green().bold(), r.name.blue());
            continue;
        }
        failed += 1;
        println!("{} {}", "FAIL".red().bold(), r.name.blue());
        for f in &r.findings {
            let path = |p: &std::path::Path| p.display().to_string().blue();
            match f {
                Finding::Missing(p) => println!("  {} {}", "missing".red(), path(p)),
                Finding::Extra(p) => println!("  {} {}", "extra  ".yellow(), path(p)),
                Finding::BinaryDiffers(p) => println!("  {} {}", "differs".red(), path(p)),
                Finding::ModeDiffers { path: p, expected_exec } => println!(
                    "  {} {}  {}",
                    "mode   ".red(),
                    path(p),
                    if *expected_exec { "expected executable" } else { "expected not executable" }.yellow()
                ),
                Finding::Differs { path: p, expected, actual } => {
                    println!("  {} {}", "differs".red(), path(p));
                    print_diff(p, expected.as_bytes(), actual);
                }
            }
        }
    }
    let total = results.len();
    if a.update {
        println!("\n{} {}", "Updated".cyan().bold(), format!("{total} case(s).").green());
    } else if failed == 0 {
        println!("\n{} {}", "Tests:".cyan().bold(), format!("{total} passed.").green().bold());
    } else {
        println!("\n{} {}", "Tests:".cyan().bold(), format!("{failed} of {total} failed.").red().bold());
        return Err("golden tests failed".into());
    }
    Ok(())
}
