//! The template engine wrapper. One place decides the engine, its strictness,
//! the filters template authors can rely on, and where includes resolve.

use std::path::Path;
use std::sync::{Arc, Mutex};

use heck::{ToKebabCase, ToLowerCamelCase, ToPascalCase, ToShoutySnakeCase, ToSnakeCase, ToTitleCase};
use minijinja::syntax::SyntaxConfig;
use minijinja::{Environment, UndefinedBehavior, Value};

use crate::manifest::Syntax;
use crate::shell;

/// Shared by every renderer of one plan: whether `shell()` may run, where,
/// and a log of what it ran so the plan can show it.
pub struct ShellState {
    pub allowed: bool,
    pub ctx: shell::Context,
    pub log: Mutex<Vec<String>>,
}

#[derive(Debug, thiserror::Error)]
#[error("{file}{line}: {detail}")]
pub struct RenderError {
    pub file: String,
    /// Pre-formatted as `:N` so the Display impl stays simple.
    line: String,
    pub detail: String,
}

pub struct Renderer {
    env: Environment<'static>,
}

impl Renderer {
    /// `control_root` is the directory `{% include %}` and `{% import %}`
    /// names resolve against: the template's `.sc/` directory.
    pub fn new(control_root: &Path) -> Self {
        Self::build(control_root, None, None).expect("default syntax is valid")
    }

    pub fn configure(
        control_root: &Path,
        syntax: Option<&Syntax>,
        shell: Option<Arc<ShellState>>,
    ) -> Result<Self, RenderError> {
        Self::build(control_root, syntax, shell)
    }

    fn build(control_root: &Path, syntax: Option<&Syntax>, shell: Option<Arc<ShellState>>) -> Result<Self, RenderError> {
        let mut env = Environment::new();
        // An undefined variable is a template bug, never silent empty output.
        env.set_undefined_behavior(UndefinedBehavior::Strict);
        // A file's final newline is content; do not strip it.
        env.set_keep_trailing_newline(true);
        env.set_loader(minijinja::path_loader(control_root));
        env.add_filter("snake_case", |s: &str| s.to_snake_case());
        env.add_filter("kebab_case", |s: &str| s.to_kebab_case());
        env.add_filter("pascal_case", |s: &str| s.to_pascal_case());
        env.add_filter("camel_case", |s: &str| s.to_lower_camel_case());
        env.add_filter("shouty_snake_case", |s: &str| s.to_shouty_snake_case());
        env.add_filter("title_case", |s: &str| s.to_title_case());
        if let Some(st) = shell {
            env.add_function("shell", move |cmd: String| -> Result<String, minijinja::Error> {
                use minijinja::{Error, ErrorKind};
                if !st.allowed {
                    return Err(Error::new(ErrorKind::InvalidOperation, "shell() requires --allow-shell"));
                }
                let out = shell::capture(shell::Program::Inline { text: &cmd, shell: "sh" }, &cmd, &st.ctx)
                    .map_err(|e| Error::new(ErrorKind::InvalidOperation, e.to_string()))?;
                st.log.lock().unwrap().push(cmd);
                Ok(out.strip_suffix('\n').unwrap_or(&out).to_string())
            });
        }
        if let Some(s) = syntax {
            let cfg = SyntaxConfig::builder()
                .block_delimiters(s.block.0.clone(), s.block.1.clone())
                .variable_delimiters(s.variable.0.clone(), s.variable.1.clone())
                .comment_delimiters(s.comment.0.clone(), s.comment.1.clone())
                .build()
                .map_err(|e| RenderError { file: "sc.toml".into(), line: String::new(), detail: e.to_string() })?;
            env.set_syntax(cfg);
        }
        Ok(Self { env })
    }

    /// Render `source`, attributing errors to `name` (a template-relative path).
    pub fn render(&self, name: &str, source: &str, ctx: &Value) -> Result<String, RenderError> {
        let wrap = |e: minijinja::Error| RenderError {
            file: e.name().filter(|n| *n != name).map(|n| format!("{name} -> {n}")).unwrap_or_else(|| name.to_string()),
            line: e.line().map(|l| format!(":{l}")).unwrap_or_default(),
            detail: match e.detail() {
                Some(d) => format!("{}: {d}", e.kind()),
                None => e.kind().to_string(),
            },
        };
        let tmpl = self.env.template_from_named_str(name, source).map_err(wrap)?;
        tmpl.render(ctx).map_err(wrap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minijinja::context;

    #[test]
    fn filters_and_trailing_newline() {
        let r = Renderer::new(Path::new("."));
        let ctx = context! { name => "My App" };
        let out = r.render("t", "{{ name | snake_case }}/{{ name | pascal_case }}\n", &ctx).unwrap();
        assert_eq!(out, "my_app/MyApp\n");
    }

    #[test]
    fn undefined_is_an_error_with_location() {
        let r = Renderer::new(Path::new("."));
        let err = r.render("src/x.rs", "ok\n{{ nope }}", &context! {}).unwrap_err();
        assert_eq!(err.file, "src/x.rs");
        assert!(err.to_string().starts_with("src/x.rs:2:"), "{err}");
    }

    #[test]
    fn includes_resolve_against_control_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("partials")).unwrap();
        std::fs::write(dir.path().join("partials/hdr.txt"), "== {{ name }} ==\n").unwrap();
        let r = Renderer::new(dir.path());
        let out = r.render("f", "{% include 'partials/hdr.txt' %}body\n", &context! { name => "x" }).unwrap();
        assert_eq!(out, "== x ==\nbody\n");
        let err = r.render("f", "{% include 'partials/missing' %}", &context! {}).unwrap_err();
        assert!(err.to_string().contains("missing"), "{err}");
    }

    #[test]
    fn shell_function_is_gated() {
        let state = |allowed| {
            Arc::new(ShellState {
                allowed,
                ctx: shell::Context { cwd: std::env::temp_dir(), env: vec![] },
                log: Mutex::new(vec![]),
            })
        };
        let r = Renderer::configure(Path::new("."), None, Some(state(false))).unwrap();
        let err = r.render("f", "{{ shell('echo x') }}", &context! {}).unwrap_err();
        assert!(err.to_string().contains("--allow-shell"), "{err}");

        let st = state(true);
        let r = Renderer::configure(Path::new("."), None, Some(st.clone())).unwrap();
        assert_eq!(r.render("f", "v={{ shell('echo x') }}!", &context! {}).unwrap(), "v=x!");
        assert_eq!(*st.log.lock().unwrap(), ["echo x"]);
    }

    #[test]
    fn custom_syntax() {
        let syntax = Syntax {
            path: "x".into(),
            block: ("[%".into(), "%]".into()),
            variable: ("[[".into(), "]]".into()),
            comment: ("[#".into(), "#]".into()),
        };
        let r = Renderer::configure(Path::new("."), Some(&syntax), None).unwrap();
        let out = r.render("t", "{{ .Values.x }} [[ n ]] [% if n %]y[% endif %][# c #]\n", &context! { n => 1 }).unwrap();
        assert_eq!(out, "{{ .Values.x }} 1 y\n");
    }
}
