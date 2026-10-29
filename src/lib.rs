//! `sc`: scaffolding engine.
//!
//! The pipeline is `manifest` -> `inputs` -> `plan` -> `apply`. A [`plan::Plan`]
//! is plain data: everything that inspects, tests or displays a run works on
//! that value, and `apply` is the only consumer with side effects.
//!
//! - [`source`] fetches a template (local path or git) and [`compose`] loads
//!   it with the templates it builds on.
//! - [`render`] wraps the template engine; [`merge`] combines rendered files
//!   with existing ones; [`shell`] runs hooks and generators.
//! - [`update`] re-applies a newer template version with a three-way merge,
//!   using the [`answers`] file written by every apply.
//! - [`golden`] runs a template's own expected-output tests; [`skeleton`]
//!   writes a starter template; [`describe`] explains one without rendering.
//! - [`cli`] is the command-line front end.

pub mod answers;
pub mod apply;
pub mod cli;
pub mod compose;
pub mod describe;
pub mod golden;
pub mod inputs;
pub mod manifest;
pub mod merge;
pub mod plan;
pub mod render;
pub mod shell;
pub mod skeleton;
pub mod source;
pub mod update;
