//! Running `after` hooks once the destination is committed.

use std::path::Path;

use super::{ApplyError, Hooks, Outcome};
use crate::scaffolding::manifest::Phase;
use crate::scaffolding::plan::{Plan, PlannedHook, program};
use crate::scaffolding::shell;

/// `on_hook` is called just before each hook runs, so a caller can announce
/// it ahead of the hook's own output. A failing hook stops the sequence;
/// files are already written by then, and the error says so.
pub async fn run_after(
    plan: &Plan,
    dest: &Path,
    mode: Hooks,
    on_hook: &mut dyn FnMut(&PlannedHook),
    outcome: &mut Outcome,
) -> Result<(), ApplyError> {
    if mode == Hooks::Skip {
        return Ok(());
    }
    for hook in plan.hooks.iter().filter(|h| h.phase == Phase::After) {
        let cwd = dest.join(&hook.cwd);
        let ctx = shell::Context::new(&cwd, &hook.inputs, &plan.template.source, Some(dest));
        on_hook(hook);
        let result = match mode {
            Hooks::Run => shell::interactive(program(hook), &hook.name, &ctx).await,
            _ => shell::capture(program(hook), &hook.name, &ctx).await.map(drop),
        };
        result.map_err(|source| ApplyError::Hook { name: hook.name.clone(), source })?;
        outcome.hooks.push(hook.name.clone());
    }
    Ok(())
}
