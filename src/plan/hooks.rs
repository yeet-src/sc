//! Resolving manifest hooks into fully rendered, runnable programs.

use std::path::Path;

use minijinja::Value;

use super::{Exec, PlanError, PlannedHook};
use crate::manifest::{CONTROL_DIR, Hook};
use crate::render::Renderer;
use crate::shell::Program;

pub fn hook_name(h: &Hook) -> String {
    if let Some(n) = &h.name {
        return n.clone();
    }
    match (&h.run, &h.script) {
        (_, Some(s)) => s.clone(),
        (Some(r), None) => r.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#')).unwrap_or("").to_string(),
        (None, None) => String::new(),
    }
}

pub fn hook_enabled(h: &Hook, renderer: &Renderer, ctx: &Value) -> Result<bool, PlanError> {
    let Some(when) = &h.when else { return Ok(true) };
    let v = renderer.render(&format!("hook `{}` when", hook_name(h)), when, ctx)?;
    Ok(!matches!(v.trim().to_ascii_lowercase().as_str(), "" | "false" | "0" | "no" | "none"))
}

pub fn resolve_hook(h: &Hook, root: &Path, renderer: &Renderer, ctx: &Value) -> Result<PlannedHook, PlanError> {
    let name = hook_name(h);
    let (raw, exec) = match (&h.run, &h.script) {
        (_, Some(script)) => {
            let path = root.join(CONTROL_DIR).join(script);
            let text = std::fs::read_to_string(&path).map_err(|e| PlanError::HookScript(script.clone(), e))?;
            (text, Exec::Script { path: script.clone() })
        }
        (Some(run), None) => (run.clone(), Exec::Shell { shell: h.shell.clone() }),
        (None, None) => unreachable!("validated by manifest"),
    };
    let text = if h.render { renderer.render(&format!("hook `{name}`"), &raw, ctx)? } else { raw };
    Ok(PlannedHook { phase: h.phase, name, text, exec })
}

pub fn program(hook: &PlannedHook) -> Program<'_> {
    match &hook.exec {
        Exec::Shell { shell } => Program::Inline { text: &hook.text, shell },
        Exec::Script { .. } => Program::Script { text: &hook.text },
    }
}
