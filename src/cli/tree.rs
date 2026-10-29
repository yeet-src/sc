use colored::Colorize;

use super::{Result, Template, Vars};
use crate::scaffolding::describe::{self, EntryKind, Tag};
use crate::scaffolding::inputs;
use crate::scaffolding::manifest::{Phase, VarKind};

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    template: Template,
    /// Inputs to evaluate `[tree]` conditions with; missing required inputs
    /// are allowed here and left undefined.
    #[command(flatten)]
    vars: Vars,
    /// Emit the description as JSON instead of text.
    #[arg(long, short = 'j')]
    json: bool,
    /// Also show control entries: the manifest, .sc/, mounted sub-templates.
    #[arg(long, short = 'a')]
    all: bool,
}

pub async fn run(a: Args) -> Result {
    let loaded = a.template.load().await?;
    let inputs = if a.vars.vars.is_empty() {
        None
    } else {
        Some(inputs::resolve_lenient(&loaded.combined_manifest(), &a.vars.vars)?)
    };
    let d = describe::describe(&loaded, inputs.as_ref())?;
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
            Some(v) => format!("= {}", crate::scaffolding::inputs::override_text(v)).green(),
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
    // Hide control entries, and directories that held nothing else, as the
    // planner does. Computed back to front so a parent sees its children.
    let mut hidden = vec![false; d.tree.len()];
    if !a.all {
        for i in (0..d.tree.len()).rev() {
            let e = &d.tree[i];
            if e.tags.contains(&Tag::Control) {
                hidden[i] = true;
                continue;
            }
            if e.kind == EntryKind::Dir {
                let children: Vec<usize> = (i + 1..d.tree.len())
                    .take_while(|&j| d.tree[j].depth > e.depth)
                    .filter(|&j| d.tree[j].depth == e.depth + 1)
                    .collect();
                hidden[i] = !children.is_empty() && children.iter().all(|&j| hidden[j]);
            }
        }
    }
    let tree: Vec<_> = d.tree.iter().enumerate().filter(|(i, _)| !hidden[*i]).map(|(_, e)| e).collect();
    // Which depths still have siblings below, for the vertical guides.
    let mut open = [false; 64];
    for (i, e) in tree.iter().enumerate() {
        let last = !tree[i + 1..].iter().take_while(|x| x.depth >= e.depth).any(|x| x.depth == e.depth);
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
                Tag::When { rule, holds: None } => format!("when {rule}").yellow().to_string(),
                Tag::When { rule, holds: Some(true) } => format!("when {rule}: on").green().to_string(),
                Tag::When { rule, holds: Some(false) } => format!("when {rule}: off").red().to_string(),
                Tag::VariantGroup { output, var } => format!("-> {output}, one of these by {var}").cyan().to_string(),
                Tag::Variant { var, value, chosen: None } => format!("{var} = {value}").yellow().to_string(),
                Tag::Variant { var, value, chosen: Some(true) } => format!("{var} = {value}: chosen").green().bold().to_string(),
                Tag::Variant { var, value, chosen: Some(false) } => format!("{var} = {value}").red().to_string(),
            })
            .collect();
        let tags = if tags.is_empty() { String::new() } else { format!("  {}", tags.join(", ")) };
        println!("{prefix}{branch}{name}{tags}");
    }
    Ok(())
}
