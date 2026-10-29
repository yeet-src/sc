//! Glob rule sets from the manifest, and the per-syntax renderers.

use std::path::Path;
use std::sync::Arc;

use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::manifest::{CONTROL_DIR, Manifest, MergeRule, MergeSpec};
use crate::render::{Renderer, ShellState};

use super::PlanError;

pub struct Rules {
    pub set: GlobSet,
    pub patterns: Vec<String>,
}

impl Rules {
    pub fn new(patterns: &[String]) -> Result<Self, globset::Error> {
        let mut b = GlobSetBuilder::new();
        for p in patterns {
            b.add(Glob::new(p)?);
        }
        Ok(Self { set: b.build()?, patterns: patterns.to_vec() })
    }

    /// The first pattern matching `path`, if any.
    pub fn matched(&self, path: &Path) -> Option<&str> {
        self.set.matches(path).first().map(|&i| self.patterns[i].as_str())
    }
}

pub struct MergeRules {
    rules: Rules,
    specs: Vec<MergeSpec>,
}

impl MergeRules {
    pub fn new(merge: &indexmap::IndexMap<String, MergeRule>) -> Result<Self, globset::Error> {
        let patterns: Vec<String> = merge.keys().cloned().collect();
        Ok(Self { rules: Rules::new(&patterns)?, specs: merge.values().map(|r| r.spec()).collect() })
    }

    pub fn matched(&self, path: &Path) -> Option<(&str, &MergeSpec)> {
        let i = *self.rules.set.matches(path).first()?;
        Some((self.rules.patterns[i].as_str(), &self.specs[i]))
    }
}

/// Per-glob renderers for files that need different delimiters, plus the
/// default. Paths are always rendered with the default syntax.
pub struct Renderers {
    pub default: Renderer,
    custom: Vec<(Rules, Renderer)>,
}

impl Renderers {
    pub fn new(root: &Path, manifest: &Manifest, shell: &Arc<ShellState>) -> Result<Self, PlanError> {
        let control = root.join(CONTROL_DIR);
        let mut custom = Vec::new();
        for s in &manifest.files.syntax {
            let r = Renderer::configure(&control, Some(s), Some(shell.clone()))?;
            custom.push((Rules::new(std::slice::from_ref(&s.path))?, r));
        }
        Ok(Self { default: Renderer::configure(&control, None, Some(shell.clone()))?, custom })
    }

    pub fn for_file(&self, src_rel: &Path) -> &Renderer {
        self.custom.iter().find(|(r, _)| r.matched(src_rel).is_some()).map(|(_, r)| r).unwrap_or(&self.default)
    }
}
