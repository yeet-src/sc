//! Walking the template tree: one planner per layer, producing ops.

use std::path::{Component, Path, PathBuf};

use minijinja::Value;

use super::rules::{Renderers, Rules};
use super::{Content, DropReason, Dropped, Op, OpKind, Origin, PlanError, Status, Warning};
use crate::scaffolding::manifest::{CONTROL_DIR, MANIFEST_FILE, variant_stem};

pub struct Planner<'a> {
    pub root: &'a Path,
    pub extra_control: &'a [PathBuf],
    pub renderers: Renderers,
    pub dot_prefix: &'a str,
    /// Template-relative path (no trailing slash) -> output name.
    pub rename: &'a indexmap::IndexMap<String, String>,
    /// Separator in `NAME<sep>VAR` variant group directories; "" disables.
    pub variant_sep: &'a str,
    pub ctx: Value,
    pub exclude: Rules,
    pub raw: Rules,
    /// `raw` rules matched against output paths.
    pub raw_out: Rules,
    pub ops: Vec<Op>,
    pub dropped: Vec<Dropped>,
    pub warnings: Vec<Warning>,
}

impl Planner<'_> {
    /// `src_rel` is the template-relative directory being walked; `out_rel`
    /// is where its rendered contents land, relative to the destination.
    pub fn walk(&mut self, src_rel: &Path, out_rel: &Path) -> Result<(), PlanError> {
        let abs = self.root.join(src_rel);
        let io = |source| PlanError::Io { path: abs.display().to_string(), source };
        let mut entries: Vec<_> =
            std::fs::read_dir(&abs).map_err(io)?.collect::<Result<_, _>>().map_err(io)?;
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            let name = entry.file_name();
            let src_rel = src_rel.join(&name);
            if src_rel == Path::new(MANIFEST_FILE)
                || src_rel == Path::new(CONTROL_DIR)
                || src_rel == Path::new(".git")
                || self.extra_control.contains(&src_rel)
            {
                self.drop(src_rel, DropReason::Control);
                continue;
            }
            if let Some(rule) = self.exclude.matched(&src_rel) {
                let rule = rule.to_string();
                self.drop(src_rel, DropReason::Excluded { rule });
                continue;
            }
            let ft = entry.file_type().map_err(io)?;
            if let (true, Some((base, var))) = (ft.is_dir(), self.variant_group(&name.to_string_lossy())) {
                self.variant(&src_rel, out_rel, &base, &var)?;
                continue;
            }

            let Some(out_name) = self.render_component(&src_rel, &name.to_string_lossy())? else {
                self.drop(src_rel, DropReason::EmptyPath);
                continue;
            };
            let out_rel = out_rel.join(out_name);

            let kind = if ft.is_symlink() {
                let target = std::fs::read_link(entry.path()).map_err(io)?;
                self.check_symlink(&out_rel, &target);
                OpKind::Symlink { target }
            } else if ft.is_dir() {
                let before = self.ops.len();
                self.walk(&src_rel, &out_rel)?;
                // A directory that only held control material (a mounted
                // sub-template, say) is control material itself.
                if self.ops.len() == before && self.extra_control.iter().any(|c| c.starts_with(&src_rel)) {
                    self.drop(src_rel, DropReason::Control);
                    continue;
                }
                OpKind::CreateDir
            } else {
                self.file(&entry.path(), &src_rel, &out_rel)?
            };
            self.ops.push(Op { template_path: src_rel, path: out_rel, mount: None, kind, status: Status::Create });
        }
        Ok(())
    }

    fn drop(&mut self, template_path: PathBuf, reason: DropReason) {
        self.dropped.push(Dropped { template_path, reason });
    }

    fn warn(&mut self, path: &Path, message: impl Into<String>) {
        self.warnings.push(Warning { path: path.to_path_buf(), message: message.into() });
    }

    /// `NAME<sep>VAR` -> (NAME, VAR), for a directory name.
    fn variant_group(&self, name: &str) -> Option<(String, String)> {
        if self.variant_sep.is_empty() {
            return None;
        }
        let (base, var) = name.rsplit_once(self.variant_sep)?;
        (!base.is_empty() && !var.is_empty() && var.chars().all(|c| c == '_' || c.is_ascii_alphanumeric()))
            .then(|| (base.to_string(), var.to_string()))
    }

    /// Emit the child of a variant group whose stem equals the value of
    /// `var`, as `base` in `out_dir`; nothing when no child matches.
    fn variant(&mut self, group_rel: &Path, out_dir: &Path, base: &str, var: &str) -> Result<(), PlanError> {
        let value = self.ctx.get_attr(var).unwrap_or_default();
        if value.is_undefined() {
            return Err(PlanError::UnknownVariantInput { group: group_rel.display().to_string(), var: var.to_string() });
        }
        let value = crate::scaffolding::inputs::override_text(&value);

        let abs = self.root.join(group_rel);
        let io = |source| PlanError::Io { path: abs.display().to_string(), source };
        let mut children: Vec<_> = std::fs::read_dir(&abs).map_err(io)?.collect::<Result<_, _>>().map_err(io)?;
        children.sort_by_key(|e| e.file_name());
        let stem = |e: &std::fs::DirEntry| {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) { name } else { variant_stem(&name) }
        };
        let Some(chosen) = children.iter().find(|e| stem(e) == value) else {
            let available = children.iter().map(stem).collect();
            self.drop(group_rel.to_path_buf(), DropReason::NoVariant { var: var.to_string(), value, available });
            return Ok(());
        };

        let Some(out_name) = self.render_component(group_rel, base)? else {
            self.drop(group_rel.to_path_buf(), DropReason::EmptyPath);
            return Ok(());
        };
        let out_rel = out_dir.join(out_name);
        let child_rel = group_rel.join(chosen.file_name());
        let ft = chosen.file_type().map_err(io)?;
        let kind = if ft.is_symlink() {
            let target = std::fs::read_link(chosen.path()).map_err(io)?;
            self.check_symlink(&out_rel, &target);
            OpKind::Symlink { target }
        } else if ft.is_dir() {
            self.walk(&child_rel, &out_rel)?;
            OpKind::CreateDir
        } else {
            self.file(&chosen.path(), &child_rel, &out_rel)?
        };
        self.ops.push(Op { template_path: child_rel, path: out_rel, mount: None, kind, status: Status::Create });
        Ok(())
    }

    /// Render one path component. `None` means "drop this entry".
    fn render_component(&self, src_rel: &Path, component: &str) -> Result<Option<PathBuf>, PlanError> {
        let rendered = if component.contains("{{") || component.contains("{%") {
            let name = format!("{} (path)", src_rel.display());
            self.renderers.default.render(&name, component, &self.ctx)?
        } else {
            component.to_string()
        };
        if rendered.is_empty() {
            return Ok(None);
        }
        // An explicit rename wins over the dot prefix convention.
        let key = src_rel.display().to_string();
        let rendered = match self.rename.get(&key).or_else(|| self.rename.get(&format!("{key}/"))) {
            Some(to) => to.trim_end_matches('/').to_string(),
            None => match rendered.strip_prefix(self.dot_prefix) {
                Some(rest) if !self.dot_prefix.is_empty() && !rest.is_empty() => format!(".{rest}"),
                _ => rendered,
            },
        };
        let bad = |reason| PlanError::BadPath {
            template_path: src_rel.display().to_string(),
            rendered: rendered.clone(),
            reason,
        };
        let out = PathBuf::from(&rendered);
        for c in out.components() {
            match c {
                Component::Normal(_) | Component::CurDir => {}
                Component::ParentDir => return Err(bad("escapes the destination")),
                Component::RootDir | Component::Prefix(_) => return Err(bad("is absolute")),
            }
        }
        Ok(Some(out))
    }

    fn file(&mut self, abs: &Path, src_rel: &Path, out_rel: &Path) -> Result<OpKind, PlanError> {
        let io = |source| PlanError::Io { path: abs.display().to_string(), source };
        let bytes = std::fs::read(abs).map_err(io)?;
        let mode = file_mode(abs).map_err(io)?;

        let (content, origin) = if let Some(rule) = self.raw.matched(src_rel).or_else(|| self.raw_out.matched(out_rel)) {
            (Content::Bytes(bytes), Origin::Raw { rule: rule.to_string() })
        } else if looks_binary(&bytes) {
            (Content::Bytes(bytes), Origin::Binary)
        } else {
            let text = String::from_utf8(bytes).expect("looks_binary rejects invalid UTF-8");
            let renderer = self.renderers.for_file(src_rel);
            let rendered = renderer.render(&src_rel.display().to_string(), &text, &self.ctx)?;
            if rendered.is_empty() && !text.is_empty() {
                self.warn(out_rel, "renders to an empty file");
            }
            if rendered.contains("{{") || rendered.contains("{%") {
                self.warn(out_rel, "rendered output still contains template syntax");
            }
            (Content::Text(rendered), Origin::Rendered)
        };
        Ok(OpKind::WriteFile { content, mode, origin })
    }

    /// Warn when a symlink's target would resolve outside the destination.
    fn check_symlink(&mut self, out_rel: &Path, target: &Path) {
        if target.is_absolute() {
            self.warn(out_rel, format!("symlink target `{}` is absolute", target.display()));
            return;
        }
        let mut depth = out_rel.components().count() as i64 - 1;
        for c in target.components() {
            match c {
                Component::ParentDir => depth -= 1,
                Component::Normal(_) => depth += 1,
                _ => {}
            }
            if depth < 0 {
                self.warn(
                    out_rel,
                    format!("symlink target `{}` points outside the destination", target.display()),
                );
                return;
            }
        }
    }
}

/// A file is binary if its first 8 KiB contains a NUL byte or is not valid
/// UTF-8 at a non-boundary position.
pub fn looks_binary(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(8192)];
    if head.contains(&0) {
        return true;
    }
    match std::str::from_utf8(head) {
        Ok(_) => false,
        // Invalid only because we cut a multi-byte char at the 8 KiB edge.
        Err(e) => e.error_len().is_some() || std::str::from_utf8(bytes).is_err(),
    }
}

#[cfg(unix)]
pub fn file_mode(path: &Path) -> std::io::Result<u32> {
    use std::os::unix::fs::PermissionsExt;
    Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
pub fn file_mode(_path: &Path) -> std::io::Result<u32> {
    Ok(0o644)
}
