//! Everything that prints to a terminal. Nothing below this module knows
//! what a terminal is.
//!
//! Palette: section titles cyan, paths blue, values green, metadata magenta,
//! template-side information cyan, attention yellow, problems red.

use std::path::Path;

use colored::{ColoredString, Colorize};

use crate::apply;
use crate::inputs;
use crate::manifest::Phase;
use crate::plan::{Content, DropReason, Exec, OpKind, Origin, Plan, Status};

/// Ask on the terminal; without one, require `--yes` so CI never hangs.
pub fn confirm() -> Result<bool, Box<dyn std::error::Error>> {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        return Err("stdin is not a terminal; pass --yes to apply without confirmation".into());
    }
    print!("\n{} {} ", "Apply?".cyan().bold(), "[y/N]".magenta());
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}

pub fn print_outcome(o: &apply::Outcome) {
    println!();
    for p in &o.created {
        println!("{} {}", "created ".green().bold(), p.display().to_string().blue());
    }
    for p in &o.modified {
        println!("{} {}", "modified".yellow().bold(), p.display().to_string().blue());
    }
    for (p, why) in &o.skipped {
        println!("{} {}  {}", "skipped ".red().bold(), p.display().to_string().blue(), why.yellow());
    }
    for p in &o.kept {
        println!("{} {}", "kept    ".magenta().bold(), p.display().to_string().blue());
    }
    for h in &o.hooks {
        println!("{} {}", "hook    ".cyan().bold(), h.green());
    }
    let parts: Vec<String> = [
        (o.created.len(), "created", "green"),
        (o.modified.len(), "modified", "yellow"),
        (o.unchanged.len(), "unchanged", "blue"),
        (o.skipped.len(), "skipped", "red"),
        (o.kept.len(), "kept", "magenta"),
        (o.hooks.len(), "hook(s) run", "cyan"),
    ]
    .into_iter()
    .filter(|(n, _, _)| *n > 0)
    .map(|(n, what, color)| format!("{n} {what}").color(color).bold().to_string())
    .collect();
    println!("\n{} {}{}", "Applied:".cyan().bold(), parts.join(&", ".magenta().to_string()), ".".cyan());
}

pub struct Show {
    pub contents: bool,
    pub diff: bool,
    pub verbose: bool,
}

pub fn print_plan(plan: &Plan, show: &Show) {
    print_header(plan);

    for op in &plan.ops {
        let path_str = op.path.display().to_string();
        let (marker, path) = match (&op.status, &op.kind) {
            (Status::Conflict { .. }, _) => ("!".red().bold(), path_str.red().bold()),
            (Status::Kept { .. }, _) => ("-".magenta().bold(), path_str.magenta()),
            (Status::Update { conflicts: true, .. }, _) => ("*".red().bold(), path_str.red().bold()),
            (Status::Update { .. }, _) => ("*".cyan().bold(), path_str.blue()),
            (_, OpKind::CreateDir) => (status_marker(&op.status), format!("{path_str}/").blue().bold()),
            (_, OpKind::Symlink { .. }) => (status_marker(&op.status), path_str.cyan().bold()),
            (_, OpKind::WriteFile { .. }) => (status_marker(&op.status), path_str.blue()),
        };
        let detail = match &op.kind {
            OpKind::CreateDir => String::new(),
            OpKind::WriteFile { content, mode, origin } => {
                let mut parts = vec![format!("{} B", content.as_bytes().len()).magenta()];
                parts.push(match origin {
                    Origin::Rendered => "rendered".green(),
                    Origin::Raw { rule } => format!("raw ({rule})").yellow(),
                    Origin::Binary => "binary".yellow(),
                    Origin::Generated => "generated".cyan(),
                    Origin::Shell { .. } => "shell".cyan().bold(),
                });
                if mode & 0o111 != 0 {
                    parts.push("exec".magenta().bold());
                }
                if let Status::Modify { mode_only: true, .. } = op.status {
                    parts.push("mode only".yellow().bold());
                }
                if let Status::Merge { strategy, .. } = &op.status {
                    parts.push(format!("merge: {}", strategy.name()).cyan().bold());
                }
                match &op.status {
                    Status::Update { conflicts: true, .. } => parts.push("3-way merge: CONFLICTS".red().bold()),
                    Status::Update { .. } => parts.push("3-way merge".cyan().bold()),
                    _ => {}
                }
                format!("  {}{}{}", "(".magenta(), join(&parts), ")".magenta())
            }
            OpKind::Symlink { target } => format!(" {} {}", "->".cyan(), target.display().to_string().cyan()),
        };
        // Only annotate where this component itself was renamed; children of a
        // renamed directory would otherwise repeat the same information.
        let renamed = if op.template_path.file_name() != op.path.file_name() {
            format!("  {} {}", "from".cyan(), op.template_path.display().to_string().cyan())
        } else {
            String::new()
        };
        let conflict = match &op.status {
            Status::Conflict { reason } => format!("  {}", reason.red()),
            Status::Kept { rule } => format!("  {}", format!("kept by `{rule}`").magenta()),
            _ => String::new(),
        };
        println!("{marker} {path}{detail}{renamed}{conflict}");

        if let (
            true,
            Status::Modify { existing, .. } | Status::Merge { existing, .. } | Status::Update { existing, .. },
            OpKind::WriteFile { content: Content::Text(new), .. },
        ) = (show.diff, &op.status, &op.kind)
        {
            print_diff(&op.path, existing, new);
        }
    }

    if show.verbose && !plan.dropped.is_empty() {
        println!("\n{}", "Dropped".cyan().bold());
        for d in &plan.dropped {
            let why = match &d.reason {
                DropReason::Control => "control file".to_string(),
                DropReason::Excluded { rule } => format!("excluded by `{rule}`"),
                DropReason::EmptyPath => "path rendered empty".to_string(),
            };
            println!("  {} {}  {}", "-".red(), d.template_path.display().to_string().blue(), why.yellow());
        }
    }

    if !plan.executed.is_empty() {
        println!("\n{}", "Ran while planning".cyan().bold());
        for e in &plan.executed {
            println!("  {} {}  {}", "$".magenta().bold(), e.purpose.yellow(), e.command.green());
        }
    }

    if !plan.hooks.is_empty() {
        println!("\n{}", "Hooks".cyan().bold());
        for h in &plan.hooks {
            let phase = match h.phase {
                Phase::Before => "before".yellow(),
                Phase::After => "after ".yellow(),
            };
            let (how, what) = match &h.exec {
                Exec::Shell { shell } => (shell.magenta(), h.name.green()),
                Exec::Script { path } => ("script".magenta(), path.blue()),
            };
            let lines = h.text.lines().count();
            let extra = if lines > 1 { format!("  ({lines} lines)").magenta().to_string() } else { String::new() };
            println!("  {phase} {how} {what}{extra}");
            if show.verbose {
                for l in h.text.lines() {
                    println!("      {}", l.green());
                }
            }
        }
    }

    if !plan.warnings.is_empty() {
        println!("\n{}", "Warnings".yellow().bold());
        for w in &plan.warnings {
            println!("  {} {}{} {}", "!".yellow().bold(), w.path.display().to_string().blue(), ":".yellow(), w.message.yellow());
        }
    }

    print_summary(plan);

    if show.contents {
        for op in &plan.ops {
            if let OpKind::WriteFile { content: Content::Text(t), .. } = &op.kind {
                println!("\n{}", format!("--- {} ---", op.path.display()).cyan().bold());
                print!("{}", t.green());
            }
        }
    }
}

fn status_marker(status: &Status) -> ColoredString {
    match status {
        Status::Create => "+".green().bold(),
        Status::Modify { .. } => "~".yellow().bold(),
        Status::Merge { .. } => ">".cyan().bold(),
        Status::Unchanged => "=".blue().bold(),
        Status::Conflict { .. } => "!".red().bold(),
        Status::Kept { .. } => "-".magenta().bold(),
        Status::Update { conflicts: true, .. } => "*".red().bold(),
        Status::Update { .. } => "*".cyan().bold(),
    }
}

fn print_header(plan: &Plan) {
    let t = &plan.template;
    let label = |s: &str| format!("{:<9}", s).cyan().bold();
    match (&t.name, &t.description) {
        (Some(n), Some(d)) => println!("{} {}  {}", label("Template"), n.green().bold(), d.green()),
        (Some(n), None) => println!("{} {}", label("Template"), n.green().bold()),
        _ => {}
    }
    match &t.git {
        Some(g) => {
            let at = match &g.reference {
                Some(r) => format!("{r} @ {}", &g.commit[..g.commit.len().min(12)]),
                None => g.commit[..g.commit.len().min(12)].to_string(),
            };
            let sub = g.subdir.as_ref().map(|s| format!("  ({})", s.display())).unwrap_or_default();
            println!("{} {}  {}{}", label("Source"), g.url.blue(), at.magenta(), sub.blue());
        }
        None => println!("{} {}", label("Source"), t.source.display().to_string().blue()),
    }
    match &t.dest {
        Some(d) => println!("{} {}", label("Dest"), d.display().to_string().blue()),
        None => println!("{} {}", label("Dest"), "(none; all operations shown as creates)".yellow()),
    }

    if !plan.inputs.0.is_empty() {
        println!("\n{}", "Inputs".cyan().bold());
        let width = plan.inputs.0.iter().map(|i| i.name.len()).max().unwrap_or(0);
        for i in &plan.inputs.0 {
            let source = match i.source {
                inputs::Source::Flag => "flag",
                inputs::Source::Default => "default",
                inputs::Source::Hook => "hook",
                inputs::Source::Answers => "answers",
            };
            let value = serde_json::to_string(&i.value).unwrap_or_else(|_| i.value.to_string());
            println!(
                "  {} {} {}  {}",
                format!("{:<width$}", i.name).yellow(),
                "=".magenta(),
                value.green(),
                format!("({source})").magenta()
            );
        }
    }
    println!();
}

fn print_summary(plan: &Plan) {
    let (mut create, mut modify, mut merge, mut unchanged, mut conflict, mut kept, mut update, mut clash, mut bytes) =
        (0, 0, 0, 0, 0, 0, 0, 0, 0usize);
    for op in &plan.ops {
        let size = match &op.kind {
            OpKind::WriteFile { content, .. } => content.as_bytes().len(),
            _ => 0,
        };
        match op.status {
            Status::Create => {
                create += 1;
                bytes += size;
            }
            Status::Modify { .. } => {
                modify += 1;
                bytes += size;
            }
            Status::Merge { .. } => {
                merge += 1;
                bytes += size;
            }
            Status::Unchanged => unchanged += 1,
            Status::Conflict { .. } => conflict += 1,
            Status::Kept { .. } => kept += 1,
            Status::Update { conflicts, .. } => {
                update += 1;
                bytes += size;
                if conflicts {
                    clash += 1;
                }
            }
        }
    }
    let mut parts = vec![format!("{create} to create").green().bold()];
    if modify > 0 {
        parts.push(format!("{modify} to modify").yellow().bold());
    }
    if merge > 0 {
        parts.push(format!("{merge} to merge").cyan().bold());
    }
    if update > 0 {
        parts.push(format!("{update} to 3-way merge").cyan().bold());
    }
    if clash > 0 {
        parts.push(format!("{clash} with conflicts").red().bold());
    }
    if kept > 0 {
        parts.push(format!("{kept} kept").magenta().bold());
    }
    if unchanged > 0 {
        parts.push(format!("{unchanged} unchanged").blue().bold());
    }
    if conflict > 0 {
        parts.push(format!("{conflict} conflict{}", if conflict == 1 { "" } else { "s" }).red().bold());
    }
    let extras = [(plan.dropped.len(), "dropped"), (plan.warnings.len(), "warning(s)")]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, what)| format!("{n} {what}"))
        .collect::<Vec<_>>();
    let extras = if extras.is_empty() {
        String::new()
    } else {
        format!("  [{}]", extras.join(", ")).yellow().to_string()
    };
    println!(
        "\n{} {}{} {} {}{}",
        "Plan:".cyan().bold(),
        join(&parts),
        ".".cyan(),
        human_bytes(bytes).magenta().bold(),
        "to write.".magenta(),
        extras
    );
}

pub fn print_diff(path: &Path, existing: &[u8], new: &str) {
    let old = String::from_utf8_lossy(existing);
    let diff = similar::TextDiff::from_lines(old.as_ref(), new);
    let name = path.display().to_string();
    for hunk in diff.unified_diff().context_radius(3).header(&name, &name).iter_hunks() {
        for line in hunk.to_string().lines() {
            let colored = match line.chars().next() {
                Some('+') => line.green(),
                Some('-') => line.red(),
                Some('@') => line.cyan().bold(),
                _ => line.white(),
            };
            println!("    {colored}");
        }
    }
}

fn join(parts: &[ColoredString]) -> String {
    let sep = ", ".magenta().to_string();
    parts.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(&sep)
}

fn human_bytes(n: usize) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{v:.1} {}", UNITS[i]) }
}

