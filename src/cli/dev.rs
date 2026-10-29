use std::collections::BTreeSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Duration;

use colored::Colorize;

use super::{Result, Template, Vars};
use crate::scaffolding::answers::Answers;
use crate::scaffolding::apply::{self, Existing, Hooks};
use crate::scaffolding::plan::{ANSWERS_FILE, PlanOptions};
use crate::scaffolding::{compose, inputs};

/// Keep a scratch directory rendered from a template while you edit it.
///
/// The scratch tree is a view: every change to the template re-renders it,
/// and edits made inside it are overwritten. Its `.sc.toml` is the
/// switchboard: change an input there and the tree re-renders with it.
#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    template: Template,
    /// Scratch directory to keep rendered; created if missing.
    scratch: PathBuf,
    /// Initial inputs; later runs read them back from the scratch .sc.toml.
    #[command(flatten)]
    vars: Vars,
    /// Permit the template to run commands while planning.
    #[arg(long)]
    allow_shell: bool,
    /// How often to look for changes, in milliseconds.
    #[arg(long, default_value_t = 500)]
    interval: u64,
}

pub async fn run(a: Args) -> Result {
    let fetch = crate::scaffolding::source::Options { offline: a.template.offline, cache_dir: None };
    let spec = a.template.template.clone();
    let scratch = a.scratch.clone();
    let opts = PlanOptions { allow_shell: a.allow_shell, ..Default::default() };

    println!(
        "{} {} {} {}  {}",
        "Watching".cyan().bold(),
        spec.blue(),
        "->".cyan(),
        scratch.display().to_string().blue(),
        "(edits inside the scratch tree are overwritten; Ctrl-C to stop)".yellow()
    );
    let mut shown = BTreeSet::new();
    let mut last = render(&spec, &a.template, &scratch, &a.vars.vars, &opts, &fetch, &mut shown).await?;

    let mut ticker = tokio::time::interval(Duration::from_millis(a.interval.max(100)));
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\n{}", "Stopped.".cyan().bold());
                return Ok(());
            }
            _ = ticker.tick() => {}
        }
        let now = fingerprint(&spec, &scratch);
        if now == last {
            continue;
        }
        // Inputs come from the scratch answers file from now on, so editing
        // it is how you flip switches; -v values given at start still win.
        let overrides = match Answers::load(&scratch) {
            Ok(ans) => {
                let mut o = ans.overrides();
                o.extend(a.vars.vars.iter().cloned());
                o
            }
            Err(_) => a.vars.vars.clone(),
        };
        match render(&spec, &a.template, &scratch, &overrides, &opts, &fetch, &mut shown).await {
            Ok(fp) => last = fp,
            Err(e) => {
                eprintln!("{} {e}", "error:".red().bold());
                last = now; // do not spin on the same broken state
            }
        }
    }
}

/// Plan and apply once, remove whatever the previous render produced that
/// this one does not, print a one-line summary, and return the fingerprint
/// of the state that produced it. `shown` is the set of paths currently
/// rendered, updated in place.
#[allow(clippy::too_many_arguments)]
async fn render(
    spec: &str,
    t: &Template,
    scratch: &Path,
    overrides: &[(String, String)],
    opts: &PlanOptions,
    fetch: &crate::scaffolding::source::Options,
    shown: &mut BTreeSet<PathBuf>,
) -> Result<u64> {
    let loaded = compose::load_with(spec, t.subdir.clone(), &t.with, fetch).await?;
    let inputs = inputs::resolve(&loaded.combined_manifest(), overrides)?;
    let plan = loaded.plan(Some(scratch), inputs, opts)?;
    let out = apply::apply(&plan, &apply::Options { existing: Existing::Overwrite, hooks: Hooks::Skip }, &mut |_| {}).await?;

    let now: BTreeSet<PathBuf> = plan.ops.iter().map(|o| o.path.clone()).collect();
    let mut removed = 0;
    let stale: Vec<&PathBuf> = shown.difference(&now).collect();
    for stale in stale.into_iter().rev() {
        // Deepest first, so directories empty out before their turn.
        let abs = scratch.join(stale);
        let ok = if abs.is_dir() { std::fs::remove_dir(&abs).is_ok() } else { std::fs::remove_file(&abs).is_ok() };
        if ok {
            removed += 1;
        }
    }
    *shown = now;

    let stamp = chrono_free_time();
    let mut parts = Vec::new();
    for (n, what, color) in [
        (out.created.len(), "created", "green"),
        (out.modified.len(), "modified", "yellow"),
        (removed, "removed", "red"),
        (out.unchanged.len(), "unchanged", "blue"),
    ] {
        if n > 0 {
            parts.push(format!("{n} {what}").color(color).to_string());
        }
    }
    let dropped = plan.dropped.iter().filter(|d| matches!(d.reason, crate::scaffolding::plan::DropReason::When { .. })).count();
    let off = if dropped > 0 { format!("  {}", format!("{dropped} off by condition").magenta()) } else { String::new() };
    println!("{} {}{off}", stamp.magenta(), parts.join(", ".magenta().to_string().as_str()));
    Ok(fingerprint(spec, scratch))
}

/// A hash of everything that should trigger a re-render: the template tree
/// (paths, sizes, mtimes) for a local template, and the scratch answers
/// file's content.
fn fingerprint(spec: &str, scratch: &Path) -> u64 {
    let mut h = DefaultHasher::new();
    let root = Path::new(spec);
    if root.is_dir() {
        walk(root, &mut h);
    }
    if let Ok(text) = std::fs::read(scratch.join(ANSWERS_FILE)) {
        text.hash(&mut h);
    }
    h.finish()
}

fn walk(dir: &Path, h: &mut DefaultHasher) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if e.file_name() == ".git" {
            continue;
        }
        let path = e.path();
        path.hash(h);
        if let Ok(m) = e.metadata() {
            m.len().hash(h);
            if let Ok(t) = m.modified() {
                t.hash(h);
            }
            if m.is_dir() {
                walk(&path, h);
            }
        }
    }
}

/// HH:MM:SS from the system clock, without pulling in a date crate.
fn chrono_free_time() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    format!("{h:02}:{m:02}:{s:02}")
}
