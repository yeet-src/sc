use std::path::{Path, PathBuf};

use colored::Colorize;

use super::{Result, Vars};
use crate::scaffolding::describe::{self, Tag};
use crate::scaffolding::{compose, inputs};

/// Say where a directory inside a template ends up in the output. Prints
/// nothing and exits 0 outside a template, so it is safe in a prompt:
///
///   PROMPT='%~ $(sc where --short) %# '        zsh
///   PS1='\w $(sc where --short) \$ '           bash
#[derive(clap::Args)]
pub struct Args {
    /// Path to describe; the current directory by default.
    path: Option<PathBuf>,
    /// Inputs to evaluate conditions and variant choices with.
    #[command(flatten)]
    vars: Vars,
    /// One compact segment for a shell prompt.
    #[arg(long)]
    short: bool,
    /// Like --short, colored, with escapes wrapped so this shell's prompt
    /// keeps its cursor math right. Implies --short.
    #[arg(long, value_enum, value_name = "SHELL")]
    prompt: Option<PromptShell>,
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum PromptShell {
    Zsh,
    Bash,
}

/// Wrap every ANSI escape in the shell's zero-width markers.
fn wrap_escapes(s: &str, shell: PromptShell) -> String {
    let (open, close) = match shell {
        PromptShell::Zsh => ("%{", "%}"),
        PromptShell::Bash => ("\\[", "\\]"),
    };
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('\x1b') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let end = tail.find('m').map(|m| m + 1).unwrap_or(tail.len());
        out.push_str(open);
        out.push_str(&tail[..end]);
        out.push_str(close);
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// Print the one-line form, styled for a prompt when asked.
fn emit(line: String, prompt: Option<PromptShell>) {
    match prompt {
        Some(sh) => println!("{}", wrap_escapes(&line, sh)),
        None => println!("{line}"),
    }
}

pub async fn run(a: Args) -> Result {
    if a.prompt.is_some() {
        colored::control::set_override(true);
    }
    let short = a.short || a.prompt.is_some();
    let path = match &a.path {
        Some(p) => std::fs::canonicalize(p)?,
        None => std::env::current_dir()?,
    };
    let Some(root) = describe::find_template_root(&path) else {
        return Ok(()); // not in a template: say nothing
    };
    let rel = path.strip_prefix(&root).unwrap_or(Path::new("")).to_path_buf();
    let loaded = compose::load(&root.display().to_string(), None, &Default::default()).await?;
    let inputs = if a.vars.vars.is_empty() {
        None
    } else {
        Some(inputs::resolve_lenient(&loaded.combined_manifest(), &a.vars.vars)?)
    };
    // Inside the variant store, say which variant this is.
    if let Some((key, value)) = describe::in_variant_store(&rel) {
        let value = if root.join(&rel).is_dir() { value } else { crate::scaffolding::manifest::variant_stem(&value) };
        let current = describe::checked_out(&root, &key).as_deref() == Some(value.as_str());
        let note = if value.is_empty() {
            format!("{} {}", "variants of".cyan(), key.display().to_string().blue().bold())
        } else {
            format!("{} variant {}{}", key.display().to_string().blue().bold(), value.green().bold(), if current { " (checked out)".magenta().to_string() } else { String::new() })
        };
        if short {
            let name = loaded.manifest.template.name.clone().unwrap_or_default();
            emit(format!("{} {} {note}", name.cyan().bold(), "›".cyan()), a.prompt);
        } else {
            println!("{} {}", "Template".cyan().bold(), loaded.manifest.template.name.clone().unwrap_or_default().green().bold());
            println!("{} {}", "Here    ".cyan().bold(), rel.display().to_string().blue());
            println!("{} {note}", "Is      ".cyan().bold());
        }
        return Ok(());
    }
    let loc = describe::locate(&loaded, &rel, inputs.as_ref())?;
    let name = loaded.manifest.template.name.clone().unwrap_or_else(|| root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());

    let mut notes: Vec<String> = Vec::new();
    for t in &loc.tags {
        match t {
            Tag::Control => notes.push("control".magenta().to_string()),
            Tag::Excluded { rule } => notes.push(format!("excluded by {rule}").red().to_string()),
            Tag::Raw { rule } => notes.push(format!("raw ({rule})").yellow().to_string()),
            Tag::Conditional => notes.push("conditional".yellow().to_string()),
            Tag::Templated => notes.push("rendered name".yellow().to_string()),
            Tag::Renamed { .. } => {}
            Tag::When { rule, holds: None } => notes.push(format!("when {rule}").yellow().to_string()),
            Tag::When { rule, holds: Some(true) } => notes.push(format!("when {rule}: on").green().to_string()),
            Tag::When { rule, holds: Some(false) } => notes.push(format!("when {rule}: off").red().to_string()),
            Tag::Variants { var, choices, checked_out, chosen } => {
                notes.push(super::tree::variants_note(var, choices, checked_out.as_deref(), chosen.as_deref()))
            }
        }
    }
    let output = match (&loc.output, loc.control) {
        (_, true) => "never emitted".magenta().to_string(),
        (Some(o), _) if o.as_os_str().is_empty() => "/".blue().bold().to_string(),
        (Some(o), _) => format!("{}/", o.display()).blue().bold().to_string(),
        (None, _) => "(name is rendered)".yellow().to_string(),
    };

    if short {
        let notes = if notes.is_empty() { String::new() } else { format!("  {}", notes.join(", ")) };
        let output = if output.is_empty() { String::new() } else { format!(" {output}") };
        emit(format!("{} {}{output}{notes}", name.cyan().bold(), "›".cyan()), a.prompt);
    } else {
        println!("{} {}", "Template".cyan().bold(), name.green().bold());
        println!("{} {}", "Here    ".cyan().bold(), if rel.as_os_str().is_empty() { "/".to_string() } else { rel.display().to_string() }.blue());
        if !output.is_empty() {
            println!("{} {}", "Becomes ".cyan().bold(), output);
        }
        for n in notes {
            println!("         {n}");
        }
    }
    Ok(())
}
