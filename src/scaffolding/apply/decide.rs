//! The policy table: what each op's status means under each `--existing`
//! policy, decided for the whole plan before anything is touched.

use super::{ApplyError, Existing, Options};
use crate::scaffolding::plan::{Op, OpKind, Origin, Plan, Status};

/// Decide what to do with each op before touching anything.
pub enum Decision<'a> {
    Write(&'a Op),
    Unchanged(&'a Op),
    Skip(&'a Op, String),
    Keep(&'a Op),
}

pub fn decide<'a>(plan: &'a Plan, opts: &Options) -> Result<Vec<Decision<'a>>, ApplyError> {
    let mut out = Vec::with_capacity(plan.ops.len());
    let mut blocked = 0;
    for op in &plan.ops {
        out.push(match (&op.status, opts.existing) {
            (Status::Create, _) => Decision::Write(op),
            (Status::Unchanged, _) => Decision::Unchanged(op),
            // A merge is the author's declared intent for an existing file,
            // so it needs no --existing policy.
            (Status::Merge { .. }, _) => Decision::Write(op),
            (Status::Kept { .. }, _) => Decision::Keep(op),
            (Status::Update { conflicts: false, .. }, _) => Decision::Write(op),
            (Status::Update { conflicts: true, .. }, Existing::Overwrite) => Decision::Write(op),
            (Status::Update { conflicts: true, .. }, Existing::Skip) => Decision::Skip(op, "merge conflicts".into()),
            (Status::Update { conflicts: true, .. }, Existing::Fail) => {
                blocked += 1;
                Decision::Unchanged(op)
            }
            // The engine's own files (the answers file) are always ours to rewrite.
            (Status::Modify { .. }, _) if matches!(op.kind, OpKind::WriteFile { origin: Origin::Generated, .. }) => {
                Decision::Write(op)
            }
            (Status::Modify { .. }, Existing::Overwrite) => Decision::Write(op),
            (Status::Modify { .. }, Existing::Skip) => Decision::Skip(op, "exists with different content".into()),
            (Status::Conflict { reason }, Existing::Skip) => Decision::Skip(op, reason.clone()),
            (Status::Conflict { .. }, Existing::Overwrite) => return Err(ApplyError::Conflict(op.path.clone())),
            (Status::Modify { .. } | Status::Conflict { .. }, Existing::Fail) => {
                blocked += 1;
                Decision::Unchanged(op)
            }
        });
    }
    if blocked > 0 {
        return Err(ApplyError::Blocked(blocked));
    }
    Ok(out)
}
