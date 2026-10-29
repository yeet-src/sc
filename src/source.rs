//! Where templates come from: a local directory, or a git repository fetched
//! into a cache and checked out at a ref.
//!
//! Git access goes through the `git` binary. A bare mirror of each
//! repository lives in the cache; each run extracts the requested commit
//! into a temporary directory with `git archive`, so the cache is never
//! mutated by a template and any ref, including a bare commit, works.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Local(PathBuf),
    Git { url: String, reference: Option<String>, subdir: Option<PathBuf> },
}

/// Provenance recorded in the answers file for a git template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitInfo {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    pub commit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subdir: Option<PathBuf>,
}

#[derive(Debug)]
pub struct Resolved {
    /// The template directory to plan from.
    pub root: PathBuf,
    pub git: Option<GitInfo>,
    /// Keeps an extracted git checkout alive for as long as it is used.
    _checkout: Option<tempfile::TempDir>,
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Use the cache as-is; never contact the remote.
    pub offline: bool,
    pub cache_dir: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("template directory {0} does not exist")]
    Missing(PathBuf),
    #[error("subdirectory {subdir} not found in {url} at {commit}")]
    NoSubdir { url: String, commit: String, subdir: PathBuf },
    #[error("{url} is not in the cache and --offline was given")]
    Offline { url: String },
    #[error("git {args}: {detail}")]
    Git { args: String, detail: String },
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

impl Source {
    /// `gh:owner/repo`, `gl:owner/repo`, any URL with a scheme, an scp-style
    /// `git@host:path`, or a path ending in `.git` is git; anything else is a
    /// local path. A trailing `#ref` selects a branch, tag or commit.
    pub fn parse(spec: &str, subdir: Option<PathBuf>) -> Self {
        let (body, reference) = match spec.rsplit_once('#') {
            Some((b, r)) if !r.is_empty() && !b.is_empty() => (b, Some(r.to_string())),
            _ => (spec, None),
        };
        let url = if let Some(rest) = body.strip_prefix("gh:") {
            format!("https://github.com/{rest}.git")
        } else if let Some(rest) = body.strip_prefix("gl:") {
            format!("https://gitlab.com/{rest}.git")
        } else if body.contains("://") || body.starts_with("git@") || body.ends_with(".git") {
            body.to_string()
        } else {
            return Source::Local(PathBuf::from(spec));
        };
        Source::Git { url, reference, subdir }
    }

    pub fn resolve(&self, opts: &Options) -> Result<Resolved, SourceError> {
        match self {
            Source::Local(path) => {
                if !path.is_dir() {
                    return Err(SourceError::Missing(path.clone()));
                }
                Ok(Resolved { root: path.clone(), git: None, _checkout: None })
            }
            Source::Git { url, reference, subdir } => {
                let mirror = cache_dir(opts)?.join(cache_name(url));
                sync_mirror(url, &mirror, opts.offline)?;
                let commit = rev_parse(&mirror, reference.as_deref().unwrap_or("HEAD"))?;
                let checkout = tempfile::Builder::new().prefix("sc-template-").tempdir()?;
                archive(&mirror, &commit, checkout.path())?;
                let root = match subdir {
                    Some(s) => checkout.path().join(s),
                    None => checkout.path().to_path_buf(),
                };
                if !root.is_dir() {
                    return Err(SourceError::NoSubdir {
                        url: url.clone(),
                        commit,
                        subdir: subdir.clone().unwrap_or_default(),
                    });
                }
                let git = GitInfo { url: url.clone(), reference: reference.clone(), commit, subdir: subdir.clone() };
                Ok(Resolved { root, git: Some(git), _checkout: Some(checkout) })
            }
        }
    }
}

fn cache_dir(opts: &Options) -> Result<PathBuf, SourceError> {
    let dir = match &opts.cache_dir {
        Some(d) => d.clone(),
        None => match std::env::var_os("SC_CACHE_DIR") {
            Some(d) => PathBuf::from(d),
            None => {
                let base = std::env::var_os("XDG_CACHE_HOME")
                    .map(PathBuf::from)
                    .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
                    .unwrap_or_else(std::env::temp_dir);
                base.join("sc")
            }
        },
    };
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Readable plus unique: the repo name and a hash of the full URL.
fn cache_name(url: &str) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    let stem = url
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .rsplit(['/', ':'])
        .next()
        .unwrap_or("repo")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect::<String>();
    format!("{stem}-{:016x}", h.finish())
}

fn sync_mirror(url: &str, mirror: &Path, offline: bool) -> Result<(), SourceError> {
    if mirror.join("HEAD").exists() {
        if !offline {
            git(None, &["-C", &mirror.display().to_string(), "remote", "update", "--prune"])?;
        }
        return Ok(());
    }
    if offline {
        return Err(SourceError::Offline { url: url.to_string() });
    }
    git(None, &["clone", "--quiet", "--mirror", url, &mirror.display().to_string()])?;
    Ok(())
}

fn rev_parse(mirror: &Path, reference: &str) -> Result<String, SourceError> {
    let out = git(Some(mirror), &["rev-parse", "--verify", "--quiet", &format!("{reference}^{{commit}}")])
        .map_err(|e| match e {
            SourceError::Git { args, detail } if detail.is_empty() => SourceError::Git {
                args,
                detail: format!("ref `{reference}` not found"),
            },
            e => e,
        })?;
    Ok(out.trim().to_string())
}

fn archive(mirror: &Path, commit: &str, into: &Path) -> Result<(), SourceError> {
    let args = format!("archive {commit} | tar -x");
    let mut archive = Command::new("git")
        .args(["-C", &mirror.display().to_string(), "archive", "--format=tar", commit])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let tar = Command::new("tar")
        .args(["-x", "-C", &into.display().to_string()])
        .stdin(archive.stdout.take().expect("piped"))
        .output()?;
    let archive = archive.wait_with_output()?;
    if !archive.status.success() || !tar.status.success() {
        let detail = String::from_utf8_lossy(&archive.stderr).trim().to_string()
            + String::from_utf8_lossy(&tar.stderr).trim();
        return Err(SourceError::Git { args, detail });
    }
    Ok(())
}

fn git(cwd: Option<&Path>, args: &[&str]) -> Result<String, SourceError> {
    let mut cmd = Command::new("git");
    if let Some(c) = cwd {
        cmd.current_dir(c);
    }
    let out = cmd.args(args).output()?;
    if !out.status.success() {
        return Err(SourceError::Git {
            args: args.join(" "),
            detail: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_specs() {
        assert_eq!(Source::parse("./tpl", None), Source::Local("./tpl".into()));
        assert_eq!(Source::parse("tpl#notref", None), Source::Local("tpl#notref".into()));
        assert_eq!(
            Source::parse("gh:org/repo#v1", Some("sub".into())),
            Source::Git { url: "https://github.com/org/repo.git".into(), reference: Some("v1".into()), subdir: Some("sub".into()) }
        );
        assert_eq!(
            Source::parse("git@github.com:org/repo.git", None),
            Source::Git { url: "git@github.com:org/repo.git".into(), reference: None, subdir: None }
        );
        assert_eq!(
            Source::parse("https://example.com/r.git#abc123", None),
            Source::Git { url: "https://example.com/r.git".into(), reference: Some("abc123".into()), subdir: None }
        );
    }

    /// Builds a real repo with two commits and a tag, then resolves it by
    /// default branch, tag, commit and subdir through the cache.
    #[test]
    fn resolves_git_through_cache() {
        let repo = tempfile::tempdir().unwrap();
        let r = repo.path().display().to_string();
        let sh = |args: &[&str]| {
            let out = Command::new("git").args(["-C", &r]).args(args).env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t").output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        sh(&["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(repo.path().join("sub")).unwrap();
        std::fs::write(repo.path().join("sub/f.txt"), "one").unwrap();
        sh(&["add", "."]);
        sh(&["commit", "-qm", "one"]);
        sh(&["tag", "v1"]);
        let first = sh(&["rev-parse", "HEAD"]);
        std::fs::write(repo.path().join("sub/f.txt"), "two").unwrap();
        sh(&["commit", "-qam", "two"]);
        let second = sh(&["rev-parse", "HEAD"]);

        let cache = tempfile::tempdir().unwrap();
        let opts = Options { offline: false, cache_dir: Some(cache.path().to_path_buf()) };
        let url = format!("file://{r}");

        let head = Source::parse(&url, Some("sub".into())).resolve(&opts).unwrap();
        assert_eq!(std::fs::read_to_string(head.root.join("f.txt")).unwrap(), "two");
        assert_eq!(head.git.as_ref().unwrap().commit, second);

        let tagged = Source::parse(&format!("{url}#v1"), Some("sub".into())).resolve(&opts).unwrap();
        assert_eq!(std::fs::read_to_string(tagged.root.join("f.txt")).unwrap(), "one");
        assert_eq!(tagged.git.as_ref().unwrap().commit, first);

        let offline = Options { offline: true, ..opts.clone() };
        let by_commit = Source::parse(&format!("{url}#{first}"), None).resolve(&offline).unwrap();
        assert_eq!(std::fs::read_to_string(by_commit.root.join("sub/f.txt")).unwrap(), "one");

        let err = Source::parse(&format!("{url}#nope"), None).resolve(&opts).unwrap_err();
        assert!(err.to_string().contains("ref `nope` not found"), "{err}");
        let err = Source::parse(&url, Some("missing".into())).resolve(&opts).unwrap_err();
        assert!(matches!(err, SourceError::NoSubdir { .. }));
        let err = Source::parse("file:///nowhere/x.git", None).resolve(&offline).unwrap_err();
        assert!(matches!(err, SourceError::Offline { .. }));
    }
}
