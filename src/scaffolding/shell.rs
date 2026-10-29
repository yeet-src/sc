//! Running template-provided commands: hooks, `generate` rules and the
//! `shell()` template function. One runner so cwd, environment and error
//! reporting are the same everywhere.
//!
//! The async functions are the normal entry points. The blocking variant
//! exists for the `shell()` template function, which runs inside the
//! synchronous renderer.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::scaffolding::inputs::Inputs;

#[derive(Debug, thiserror::Error)]
pub enum ShellError {
    #[error("could not start `{command}`: {source}")]
    Spawn {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("`{command}` exited with {status}{stderr}")]
    Failed { command: String, status: String, stderr: String },
    #[error("writing temporary script: {0}")]
    Temp(std::io::Error),
}

/// Everything a command needs besides its text.
#[derive(Debug, Clone)]
pub struct Context {
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
}

impl Context {
    pub fn new(cwd: &Path, inputs: &Inputs, template: &Path, dest: Option<&Path>) -> Self {
        let mut env = vec![("SC_TEMPLATE".to_string(), template.display().to_string())];
        if let Some(d) = dest {
            env.push(("SC_DEST".to_string(), d.display().to_string()));
        }
        for i in &inputs.0 {
            let value = match i.value.as_str() {
                Some(s) => s.to_string(),
                None => i.value.to_string(),
            };
            env.push((format!("SC_VAR_{}", i.name.to_uppercase()), value));
        }
        Self { cwd: cwd.to_path_buf(), env }
    }
}

#[derive(Debug, Clone)]
pub enum Program<'a> {
    /// Script text fed to `shell` via a temporary file.
    Inline { text: &'a str, shell: &'a str },
    /// Script text written to an executable temporary file and run via its
    /// shebang, so any interpreter works.
    Script { text: &'a str },
}

/// Run and capture stdout. Non-zero exit is an error carrying stderr.
pub async fn capture(program: Program<'_>, label: &str, ctx: &Context) -> Result<String, ShellError> {
    let (mut cmd, _tmp) = command(program, ctx)?;
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let out = tokio::process::Command::from(cmd)
        .output()
        .await
        .map_err(|source| ShellError::Spawn { command: label.to_string(), source })?;
    check(label, &out)
}

/// Blocking form of [`capture`], for use from synchronous code such as the
/// template renderer. Never call it on an async runtime thread.
pub fn capture_blocking(program: Program<'_>, label: &str, ctx: &Context) -> Result<String, ShellError> {
    let (mut cmd, _tmp) = command(program, ctx)?;
    let out = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|source| ShellError::Spawn { command: label.to_string(), source })?;
    check(label, &out)
}

fn check(label: &str, out: &std::process::Output) -> Result<String, ShellError> {
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stderr = if stderr.trim().is_empty() { String::new() } else { format!(":\n{}", stderr.trim_end()) };
        return Err(ShellError::Failed { command: label.to_string(), status: out.status.to_string(), stderr });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Run with inherited stdio so the user sees the output live.
pub async fn interactive(program: Program<'_>, label: &str, ctx: &Context) -> Result<(), ShellError> {
    let (cmd, _tmp) = command(program, ctx)?;
    let status = tokio::process::Command::from(cmd)
        .status()
        .await
        .map_err(|source| ShellError::Spawn { command: label.to_string(), source })?;
    if !status.success() {
        return Err(ShellError::Failed { command: label.to_string(), status: status.to_string(), stderr: String::new() });
    }
    Ok(())
}

/// The temp file must outlive the child, so it is returned alongside.
fn command(program: Program, ctx: &Context) -> Result<(Command, Option<tempfile::NamedTempFile>), ShellError> {
    let write = |text: &str| -> Result<tempfile::NamedTempFile, ShellError> {
        let mut tmp = tempfile::Builder::new().prefix("sc-hook-").tempfile().map_err(ShellError::Temp)?;
        std::io::Write::write_all(&mut tmp, text.as_bytes()).map_err(ShellError::Temp)?;
        Ok(tmp)
    };
    let (mut cmd, tmp) = match program {
        Program::Inline { text, shell } => {
            let tmp = write(text)?;
            let mut cmd = Command::new(shell);
            cmd.arg(tmp.path());
            (cmd, Some(tmp))
        }
        Program::Script { text } => {
            let tmp = write(text)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700)).map_err(ShellError::Temp)?;
            }
            (Command::new(tmp.path()), Some(tmp))
        }
    };
    cmd.current_dir(&ctx.cwd).envs(ctx.env.iter().map(|(k, v)| (k, v)));
    Ok((cmd, tmp))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        Context { cwd: std::env::temp_dir(), env: vec![("SC_VAR_X".into(), "hi".into())] }
    }

    #[tokio::test]
    async fn captures_stdout_and_env() {
        let out = capture(Program::Inline { text: "echo \"$SC_VAR_X\"\npwd", shell: "sh" }, "t", &ctx()).await.unwrap();
        assert!(out.starts_with("hi\n"), "{out}");
        let out = capture_blocking(Program::Inline { text: "echo \"$SC_VAR_X\"", shell: "sh" }, "t", &ctx()).unwrap();
        assert_eq!(out, "hi\n");
    }

    #[tokio::test]
    async fn script_runs_via_shebang() {
        let out = capture(Program::Script { text: "#!/bin/sh\necho from-script\n" }, "t", &ctx()).await.unwrap();
        assert_eq!(out, "from-script\n");
    }

    #[tokio::test]
    async fn failure_carries_stderr() {
        let err = capture(Program::Inline { text: "echo boom >&2; exit 3", shell: "sh" }, "t", &ctx()).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("exit status: 3") && msg.contains("boom"), "{msg}");
    }
}
