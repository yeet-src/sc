use std::path::{Path, PathBuf};

use colored::Colorize;

use super::Result;
use crate::scaffolding::describe::{self, checked_out};
use crate::scaffolding::manifest::CONTROL_DIR;
use crate::scaffolding::compose;

/// Manage a template's variant store.
#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Subcommand)]
enum Cmd {
    /// Add a variant to a group, copied from the one checked out (or empty),
    /// and check it out.
    Add {
        /// Group path, as in the manifest: `LICENSE`, `config`.
        group: String,
        /// Value of the selecting input this variant serves: `GPL-3.0`.
        value: String,
        /// Start empty instead of copying the checked-out variant.
        #[arg(long)]
        empty: bool,
        /// Template directory; found from the current directory by default.
        #[arg(long, short = 't')]
        template: Option<PathBuf>,
    },
    /// List groups and their variants.
    List {
        #[arg(long, short = 't')]
        template: Option<PathBuf>,
    },
}

pub async fn run(a: Args) -> Result {
    let (template, cmd) = match a.cmd {
        Cmd::Add { ref template, .. } | Cmd::List { ref template } => (template.clone(), a.cmd),
    };
    let start = match template {
        Some(t) => std::fs::canonicalize(t)?,
        None => std::env::current_dir()?,
    };
    let Some(root) = describe::find_template_root(&start) else {
        return Err("not inside a template (no sc.toml or .sc/ found)".into());
    };
    let loaded = compose::load(&root.display().to_string(), None, &Default::default()).await?;
    let groups = loaded.manifest.variant_groups();

    match cmd {
        Cmd::List { .. } => {
            for (key, var) in &groups {
                let store = root.join(CONTROL_DIR).join("variants").join(key);
                let mut names: Vec<String> = std::fs::read_dir(&store)
                    .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
                    .unwrap_or_default();
                names.sort();
                let now = checked_out(&root, Path::new(key));
                println!("{}  {}", key.blue().bold(), format!("by {var}").cyan());
                for n in names {
                    let mark = if now.as_deref() == Some(crate::scaffolding::manifest::variant_stem(&n).as_str()) || now.as_deref() == Some(n.as_str()) { "*".green().bold().to_string() } else { " ".into() };
                    println!("  {mark} {}", n.yellow());
                }
            }
            Ok(())
        }
        Cmd::Add { group, value, empty, .. } => {
            let Some((key, var)) = groups.iter().find(|(k, _)| *k == group) else {
                return Err(format!("`{group}` is not a variant group; declare it in [tree] with variants = \"<input>\"").into());
            };
            let store = root.join(CONTROL_DIR).join("variants").join(key);
            std::fs::create_dir_all(&store)?;
            let current = checked_out(&root, Path::new(key)).map(|c| store.join(c));
            // Keep the extension of the checked-out file, if it has one.
            let ext = current.as_ref().and_then(|c| c.extension()).map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
            let new_path = store.join(format!("{value}{ext}"));
            if new_path.exists() {
                return Err(format!("{} already exists", new_path.display()).into());
            }
            match (&current, empty) {
                (Some(c), false) if c.is_dir() => copy_dir(c, &new_path)?,
                (Some(c), false) if c.exists() => {
                    std::fs::copy(c, &new_path)?;
                }
                _ => {
                    std::fs::write(&new_path, b"")?;
                }
            }
            println!("{} {}", "created".green().bold(), new_path.strip_prefix(&root).unwrap_or(&new_path).display().to_string().blue());
            println!("{}", format!("check it out with: sc checkout {var}={value}").cyan());
            Ok(())
        }
    }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let dest = to.join(e.file_name());
        if e.path().is_dir() {
            copy_dir(&e.path(), &dest)?;
        } else {
            std::fs::copy(e.path(), dest)?;
        }
    }
    Ok(())
}
