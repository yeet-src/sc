use std::path::PathBuf;

use colored::Colorize;

use super::Result;
use crate::{compose, golden, skeleton};

#[derive(clap::Args)]
pub struct Args {
    /// Directory to create the template in.
    dir: PathBuf,
    /// Template name for the manifest; defaults to the directory name.
    #[arg(long)]
    name: Option<String>,
}

pub fn run(a: Args) -> Result {
    let name = a
        .name
        .clone()
        .or_else(|| a.dir.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "template".into());
    let written = skeleton::create(&a.dir, &name)?;
    for rel in &written {
        println!("{} {}", "created".green().bold(), a.dir.join(rel).display().to_string().blue());
    }

    // Generate the golden expectation so `sc test` passes from the start.
    let loaded = compose::load(&a.dir.display().to_string(), None, &Default::default())?;
    golden::run(&loaded, None, true)?;
    println!("{} {}", "created".green().bold(), a.dir.join(".sc/tests/default/expected/").display().to_string().blue());

    let dir = a.dir.display();
    println!(
        "\n{}\n  {} {}\n  {} {}\n  {} {}",
        "Next:".cyan().bold(),
        "edit".magenta(),
        format!("{dir}/sc.toml").blue(),
        "preview".magenta(),
        format!("sc plan {dir} -v name=\"My Project\" --contents").green(),
        "test".magenta(),
        format!("sc test {dir}   (--update after intended changes)").green()
    );
    Ok(())
}
