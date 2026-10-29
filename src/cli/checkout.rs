use std::path::{Path, PathBuf};

use colored::Colorize;

use super::{Result, Vars};
use crate::scaffolding::describe::{self, checked_out};
use crate::scaffolding::manifest::{CONTROL_DIR, variant_stem};
use crate::scaffolding::{compose, inputs};

/// Check out variants in a template's working tree: `sc checkout license=MIT`
/// points `LICENSE` at `.sc/variants/LICENSE/MIT`. With no arguments, show
/// what is checked out.
#[derive(clap::Args)]
pub struct Args {
    /// Inputs that select variants, as KEY=VALUE. Repeatable.
    #[arg(value_name = "KEY=VALUE", value_parser = inputs::parse_override)]
    changes: Vec<(String, String)>,
    #[command(flatten)]
    vars: Vars,
    /// Take the inputs from a golden case of the template.
    #[arg(long = "case", value_name = "NAME")]
    case: Option<String>,
    /// Template directory; found from the current directory by default.
    #[arg(long, short = 't')]
    template: Option<PathBuf>,
    /// Replace a real file or directory at a group's path with the link.
    #[arg(long, short = 'f')]
    force: bool,
}

pub async fn run(a: Args) -> Result {
    let start = match &a.template {
        Some(t) => std::fs::canonicalize(t)?,
        None => std::env::current_dir()?,
    };
    let Some(root) = describe::find_template_root(&start) else {
        return Err("not inside a template (no sc.toml or .sc/ found)".into());
    };
    let loaded = compose::load(&root.display().to_string(), None, &Default::default()).await?;
    let groups = loaded.manifest.variant_groups();
    if groups.is_empty() {
        println!("{}", "This template declares no variant groups ([tree] ... = { variants = \"...\" }).".yellow());
        return Ok(());
    }

    let mut overrides = a.changes.clone();
    overrides.extend(a.vars.vars.iter().cloned());
    if let Some(case) = &a.case {
        let path = root.join(CONTROL_DIR).join("tests").join(case).join("inputs.toml");
        let text = std::fs::read_to_string(&path).map_err(|e| format!("case `{case}`: {}: {e}", path.display()))?;
        let table: toml::Table = toml::from_str(&text)?;
        overrides = table.into_iter().map(|(k, v)| (k, inputs::override_text(&v))).collect();
    }

    if overrides.is_empty() {
        for (key, var) in &groups {
            let now = checked_out(&root, Path::new(key)).unwrap_or_else(|| "nothing".into());
            println!("{} {} {}  {}", key.blue().bold(), "=".magenta(), now.green().bold(), format!("by {var}").cyan());
        }
        return Ok(());
    }

    // Resolve only what was given plus defaults; groups whose input is not
    // determined are left as they are.
    let resolved = inputs::resolve_lenient(&loaded.combined_manifest(), &overrides)?;
    let value_of = |var: &str| resolved.0.iter().find(|i| i.name == var).map(|i| inputs::override_text(&i.value));
    let explicit: Vec<&str> = overrides.iter().map(|(k, _)| k.as_str()).collect();

    for (key, var) in &groups {
        if !explicit.contains(&var.as_str()) && a.case.is_none() {
            continue;
        }
        let Some(value) = value_of(var) else { continue };
        let store = root.join(CONTROL_DIR).join("variants").join(key);
        let candidates: Vec<PathBuf> = std::fs::read_dir(&store)
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        let found = candidates.iter().find(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if p.is_dir() { name == value } else { variant_stem(&name) == value }
        });
        let link = root.join(key);
        let Some(target) = found else {
            let have: Vec<String> = candidates.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
            println!("{} {}  {}", "x".red().bold(), key.blue(), format!("no variant for {var} = {value}; have {}", have.join(", ")).red());
            continue;
        };
        if link.exists() || link.symlink_metadata().is_ok() {
            let is_link = link.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false);
            if !is_link && !a.force {
                return Err(format!("{}: a real file is in the way; move it into {} or pass --force", key, store.display()).into());
            }
            if link.is_dir() && !is_link {
                std::fs::remove_dir_all(&link)?;
            } else {
                std::fs::remove_file(&link)?;
            }
        }
        let rel_target = Path::new(CONTROL_DIR).join("variants").join(key).join(target.file_name().unwrap());
        let rel_target = relative_from(&rel_target, Path::new(key));
        std::os::unix::fs::symlink(&rel_target, &link)?;
        println!("{} {} {} {}", "checked out".green().bold(), key.blue().bold(), "->".cyan(), rel_target.display().to_string().magenta());
    }
    Ok(())
}

/// `target` and `link`, both template-relative, as a link target relative
/// to the link's directory.
fn relative_from(target: &Path, link: &Path) -> PathBuf {
    let depth = link.components().count().saturating_sub(1);
    let mut p = PathBuf::new();
    for _ in 0..depth {
        p.push("..");
    }
    p.join(target)
}
