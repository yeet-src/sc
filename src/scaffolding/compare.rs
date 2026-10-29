//! Comparing two plans of the same template under different inputs: what
//! appears, what disappears, what changes. The engine behind `sc diff`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::scaffolding::plan::{Content, OpKind, Plan};

#[derive(Debug, Serialize)]
pub struct Comparison {
    pub changes: Vec<Change>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Change {
    /// Only in the second plan.
    Added { path: PathBuf, dir: bool },
    /// Only in the first plan.
    Removed { path: PathBuf, dir: bool },
    /// In both with different content; text on both sides when available.
    Changed {
        path: PathBuf,
        #[serde(skip)]
        before: Option<String>,
        #[serde(skip)]
        after: Option<String>,
    },
    ModeChanged { path: PathBuf, exec: bool },
}

impl Change {
    pub fn path(&self) -> &Path {
        match self {
            Change::Added { path, .. } | Change::Removed { path, .. } | Change::Changed { path, .. } | Change::ModeChanged { path, .. } => path,
        }
    }
}

fn index(p: &Plan) -> BTreeMap<PathBuf, &OpKind> {
    p.ops.iter().map(|o| (o.path.clone(), &o.kind)).collect()
}

pub fn compare(a: &Plan, b: &Plan) -> Comparison {
    let (ia, ib) = (index(a), index(b));
    let mut changes = Vec::new();
    for (path, ka) in &ia {
        match ib.get(path) {
            None => changes.push(Change::Removed { path: path.clone(), dir: matches!(ka, OpKind::CreateDir) }),
            Some(kb) => match (ka, kb) {
                (OpKind::WriteFile { content: ca, mode: ma, .. }, OpKind::WriteFile { content: cb, mode: mb, .. }) => {
                    if ca.as_bytes() != cb.as_bytes() {
                        let text = |c: &Content| match c {
                            Content::Text(t) => Some(t.clone()),
                            Content::Bytes(_) => None,
                        };
                        changes.push(Change::Changed { path: path.clone(), before: text(ca), after: text(cb) });
                    } else if (ma & 0o111 != 0) != (mb & 0o111 != 0) {
                        changes.push(Change::ModeChanged { path: path.clone(), exec: mb & 0o111 != 0 });
                    }
                }
                (OpKind::Symlink { target: ta }, OpKind::Symlink { target: tb }) if ta != tb => {
                    changes.push(Change::Changed { path: path.clone(), before: None, after: None });
                }
                _ => {}
            },
        }
    }
    for (path, kb) in &ib {
        if !ia.contains_key(path) {
            changes.push(Change::Added { path: path.clone(), dir: matches!(kb, OpKind::CreateDir) });
        }
    }
    changes.sort_by(|x, y| x.path().cmp(y.path()));
    Comparison { changes }
}
