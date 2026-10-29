use std::path::{Path, PathBuf};

use clap::CommandFactory;
use colored::Colorize;

use super::Result;
use crate::scaffolding::describe;

/// Open a shell inside a template. The prompt shows where you are in the
/// output, `sc` completions are loaded, `SC_TEMPLATE` names the root, and
/// `sc` resolves to this binary. Leave it with `exit`.
#[derive(clap::Args)]
pub struct Args {
    /// Template directory; found from the current directory by default.
    template: Option<PathBuf>,
    /// Run one command in the environment instead of an interactive shell.
    #[arg(long, short = 'c', value_name = "COMMAND")]
    command: Option<String>,
}

pub async fn run(a: Args) -> Result {
    let start = match &a.template {
        Some(t) => std::fs::canonicalize(t)?,
        None => std::env::current_dir()?,
    };
    let Some(root) = describe::find_template_root(&start) else {
        return Err("not inside a template (no sc.toml or .sc/ found); give a template directory".into());
    };
    if std::env::var_os("SC_SHELL").is_some() && a.command.is_none() {
        return Err("already in an sc shell; type `exit` to leave it first".into());
    }

    let exe = std::env::current_exe()?;
    let exe_dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    let path = format!("{}:{}", exe_dir.display(), std::env::var("PATH").unwrap_or_default());
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let flavor = Path::new(&shell).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let rc_dir = tempfile::Builder::new().prefix("sc-shell-").tempdir()?;

    // Completions go to a file: sourcing a process substitution is fragile
    // in a non-interactive `-c` run.
    let completions = |shell: clap_complete::Shell| -> std::io::Result<PathBuf> {
        let mut buf = Vec::new();
        clap_complete::generate(shell, &mut super::Cli::command(), "sc", &mut buf);
        let p = rc_dir.path().join(format!("completions.{shell}"));
        std::fs::write(&p, buf)?;
        Ok(p)
    };
    let q = shell_quote;
    // The context line sits above the prompt; SC_WHERE carries its own
    // newline so an empty result collapses the prompt back to one line.
    let prompt_fn = |shell: &str| {
        format!(
            "_sc_prompt() {{ SC_WHERE=\"$({} where --prompt {shell} 2>/dev/null)\"; [ -n \"$SC_WHERE\" ] && SC_WHERE=\"$SC_WHERE\"$'\\n'; }}",
            q(&exe.display().to_string())
        )
    };

    let mut cmd = tokio::process::Command::new(&shell);
    cmd.current_dir(&root).env("SC_TEMPLATE", &root).env("SC_SHELL", "1").env("PATH", &path);
    match flavor.as_str() {
        "zsh" => {
            let comp = completions(clap_complete::Shell::Zsh)?;
            let rc = [
                "[ -f \"$HOME/.zshrc\" ] && source \"$HOME/.zshrc\"".to_string(),
                format!("export PATH={}", q(&path)),
                "autoload -Uz compinit && compinit -i 2>/dev/null".to_string(),
                format!("source {} 2>/dev/null", q(&comp.display().to_string())),
                "setopt PROMPT_SUBST".to_string(),
                prompt_fn("zsh"),
                "precmd_functions+=(_sc_prompt)".to_string(),
                "PROMPT='${SC_WHERE}'\"$PROMPT\"".to_string(),
            ];
            std::fs::write(rc_dir.path().join(".zshrc"), rc.join("\n") + "\n")?;
            cmd.env("ZDOTDIR", rc_dir.path());
            if let Some(c) = &a.command {
                cmd.args(["-i", "-c", c]);
            }
        }
        "bash" => {
            let comp = completions(clap_complete::Shell::Bash)?;
            let rc_path = rc_dir.path().join("rc");
            let rc = [
                "[ -f \"$HOME/.bashrc\" ] && source \"$HOME/.bashrc\"".to_string(),
                format!("export PATH={}", q(&path)),
                format!("source {} 2>/dev/null", q(&comp.display().to_string())),
                prompt_fn("bash"),
                "PROMPT_COMMAND=\"_sc_prompt${PROMPT_COMMAND:+;$PROMPT_COMMAND}\"".to_string(),
                "PS1='${SC_WHERE}'\"$PS1\"".to_string(),
            ];
            std::fs::write(&rc_path, rc.join("\n") + "\n")?;
            cmd.args(["--rcfile", &rc_path.display().to_string()]);
            if let Some(c) = &a.command {
                cmd.args(["-i", "-c", c]);
            }
        }
        _ => {
            if let Some(c) = &a.command {
                cmd.args(["-c", c]);
            }
        }
    }

    let name = std::fs::read_to_string(root.join("sc.toml"))
        .ok()
        .and_then(|t| toml::from_str::<toml::Table>(&t).ok())
        .and_then(|t| t.get("template")?.get("name")?.as_str().map(str::to_string))
        .unwrap_or_else(|| root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    if a.command.is_none() {
        println!(
            "{} {}  {}\n{}",
            "Entering".cyan().bold(),
            name.green().bold(),
            root.display().to_string().blue(),
            "The prompt shows what the current directory becomes. Try `sc tree`, `sc checkout`, `sc where`. Type `exit` to leave.".yellow()
        );
    }
    let status = cmd.status().await?;
    if a.command.is_none() {
        println!("{} {}", "Left".cyan().bold(), name.green());
    }
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// `sc exit`: a child process cannot close its parent shell, so this just
/// says how to leave.
#[derive(clap::Args)]
pub struct ExitArgs {}

pub async fn exit(_a: ExitArgs) -> Result {
    if std::env::var_os("SC_SHELL").is_some() {
        println!("{}", "Type `exit` to leave the sc shell.".yellow());
    } else {
        println!("{}", "Not in an sc shell.".yellow());
    }
    Ok(())
}

/// `sc completions <shell>`: print a completion script.
#[derive(clap::Args)]
pub struct CompletionsArgs {
    #[arg(value_enum)]
    shell: clap_complete::Shell,
}

pub fn completions(a: CompletionsArgs) -> Result {
    use std::io::Write;
    let mut buf = Vec::new();
    clap_complete::generate(a.shell, &mut super::Cli::command(), "sc", &mut buf);
    let _ = std::io::stdout().write_all(&buf); // a closed pipe is not an error here
    Ok(())
}
