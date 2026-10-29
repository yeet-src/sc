use colored::Colorize;

use super::{Result, Template};
use crate::describe::{self, EntryKind, Tag};
use crate::manifest::{Phase, VarKind};

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    template: Template,
    /// Emit the description as JSON instead of text.
    #[arg(long)]
    json: bool,
}

pub fn run(a: Args) -> Result {
    let loaded = a.template.load()?;
    let d = describe::describe(&loaded)?;
    if a.json {
        serde_json::to_writer_pretty(std::io::stdout().lock(), &d)?;
        println!();
        return Ok(());
    }

    let label = |s: &str| format!("{:<9}", s).cyan().bold();
    match (&d.name, &d.description) {
        (Some(n), Some(desc)) => println!("{} {}  {}", label("Template"), n.green().bold(), desc.green()),
        (Some(n), None) => println!("{} {}", label("Template"), n.green().bold()),
        _ => {}
    }
    println!("{} {}", label("Source"), d.source.display().to_string().blue());

    println!("\n{}", "Inputs".cyan().bold());
    if d.inputs.is_empty() {
        println!("  {}", "none declared; any -v KEY=VALUE is accepted as a string".yellow());
    }
    let width = d.inputs.iter().map(|i| i.name.len()).max().unwrap_or(0);
    for i in &d.inputs {
        let kind = match i.kind {
            VarKind::String => "string",
            VarKind::Bool => "bool",
            VarKind::Int => "int",
            VarKind::Choice => "choice",
            VarKind::List => "list",
        };
        let value = match &i.default {
            Some(v) => format!("= {}", crate::inputs::override_text(v)).green(),
            None => "required".red().bold(),
        };
        let mut notes = Vec::new();
        if let Some(p) = &i.prompt {
            notes.push(p.clone().normal());
        }
        if !i.choices.is_empty() {
            notes.push(format!("one of {}", i.choices.join(", ")).yellow());
        }
        if let Some(p) = &i.pattern {
            notes.push(format!("matches {p}").yellow());
        }
        let notes = if notes.is_empty() {
            String::new()
        } else {
            format!("  {}", notes.iter().map(|n| n.to_string()).collect::<Vec<_>>().join("; ".magenta().to_string().as_str()))
        };
        println!("  {} {} {}{}", format!("{:<width$}", i.name).yellow().bold(), format!("{kind:<6}").magenta(), value, notes);
    }

    if !d.uses.is_empty() {
        println!("\n{}", "Uses".cyan().bold());
        for u in &d.uses {
            println!("  {}", u.green());
        }
    }
    if !d.mounts.is_empty() {
        println!("\n{}", "Mounts".cyan().bold());
        for m in &d.mounts {
            let each = m.each.as_ref().map(|v| format!("  for each of {v}").yellow().to_string()).unwrap_or_default();
            println!("  {}  {} {}{}", m.path.blue().bold(), "<-".cyan(), m.template.green(), each);
        }
    }
    if !d.hooks.is_empty() {
        println!("\n{}", "Hooks".cyan().bold());
        for h in &d.hooks {
            let phase = match h.phase {
                Phase::Before => "before".yellow(),
                Phase::After => "after ".yellow(),
            };
            let when = h.when.as_ref().map(|w| format!("  when {w}").magenta().to_string()).unwrap_or_default();
            println!("  {phase} {}{when}", h.name.green());
        }
    }
    if !d.keep.is_empty() || !d.merge.is_empty() {
        println!("\n{}", "Existing files".cyan().bold());
        for k in &d.keep {
            println!("  {} {}", "keep ".magenta(), k.blue());
        }
        for (p, s) in &d.merge {
            println!("  {} {}  {}", "merge".magenta(), p.blue(), s.name().green());
        }
    }

    println!("\n{}", "Tree".cyan().bold());
    let root = d.source.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| ".".into());
    println!("{}", root.blue().bold());
    // Which depths still have siblings below, for the vertical guides.
    let mut open = [false; 64];
    for (i, e) in d.tree.iter().enumerate() {
        let last = !d.tree[i + 1..].iter().take_while(|x| x.depth >= e.depth).any(|x| x.depth == e.depth);
        let prefix: String = open[..e.depth].iter().map(|o| if *o { "│   " } else { "    " }).collect();
        open[e.depth] = !last;
        let branch = if last { "└── " } else { "├── " };
        let name = e.path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let name = match e.kind {
            EntryKind::Dir => format!("{name}/").blue().bold(),
            EntryKind::Symlink => name.cyan(),
            EntryKind::File => name.normal(),
        };
        let tags: Vec<String> = e
            .tags
            .iter()
            .map(|t| match t {
                Tag::Control => "control".magenta().to_string(),
                Tag::Excluded { rule } => format!("excluded by {rule}").red().to_string(),
                Tag::Raw { rule } => format!("raw ({rule})").yellow().to_string(),
                Tag::Conditional => "conditional".yellow().to_string(),
                Tag::Templated => "rendered name".yellow().to_string(),
                Tag::Renamed { to } => format!("-> {to}").cyan().to_string(),
            })
            .collect();
        let tags = if tags.is_empty() { String::new() } else { format!("  {}", tags.join(", ")) };
        println!("{}{branch}{name}{tags}", prefix.magenta());
    }
    Ok(())
}
