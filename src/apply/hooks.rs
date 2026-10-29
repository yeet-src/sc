//! Running `after` hooks once the destination is committed.

use std::path::Path;

use super::{ApplyError, Hooks, Outcome};
use crate::manifest::Phase;
use crate::plan::{Plan, PlannedHook, program};
use crate::shell;

/// `on_hook` is called just before each hook runs, so a caller can announce
/// it ahead of the hook's own output. A failing hook stops the sequence;
/// files are already written by then, and the error says so.
pub fn run_after(
    plan: &Plan,
    dest: &Path,
    mode: Hooks,
    on_hook: &mut dyn FnMut(&PlannedHook),
    outcome: &mut Outcome,
) -> Result<(), ApplyError> {
    if mode == Hooks::Skip {
        return Ok(());
    }
    let ctx = shell::Context::new(dest, &plan.inputs, &plan.template.source, Some(dest));
    for hook in plan.hooks.iter().filter(|h| h.phase == Phase::After) {
        on_hook(hook);
        let result = match mode {
            Hooks::Run => shell::interactive(program(hook), &hook.name, &ctx),
            _ => shell::capture(program(hook), &hook.name, &ctx).map(drop),
        };
        result.map_err(|source| ApplyError::Hook { name: hook.name.clone(), source })?;
        outcome.hooks.push(hook.name.clone());
    }
    Ok(())
}
