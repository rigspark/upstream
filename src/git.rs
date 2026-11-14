//! Thin wrappers over the `git` command for bare repos in the data folder.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub type Result<T> = std::result::Result<T, String>;

/// Runs git in `dir` with prompts off. `extra_env` carries per-call secrets (never argv).
pub fn git_env(dir: &Path, args: &[&str], extra_env: &[(String, String)]) -> Result<Vec<u8>> {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err
            .trim()
            .lines()
            .last()
            .unwrap_or("git failed")
            .to_string())
    }
}

pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    git_env(dir, args, &[]).map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// Repo names: letters, digits, `.`, `-`, `_`; no leading dot.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// Paths inside a repo: relative, no `..`, no backslashes.
pub fn valid_path(path: &str) -> bool {
    !path.starts_with('/') && !path.contains('\\') && path.split('/').all(|s| s != ".." && s != ".")
}

/// Branch names safe to pass to git as one argument.
pub fn valid_branch(b: &str) -> bool {
    !b.is_empty() && !b.starts_with('-') && git_ref_ok(b)
}

fn git_ref_ok(b: &str) -> bool {
    !b.contains("..")
        && !b.contains(' ')
        && !b.contains(':')
        && !b.contains('~')
        && !b.contains('^')
        && !b.ends_with('/')
}

pub fn repos_dir(home: &Path) -> PathBuf {
    home.join("repos")
}

pub fn repo_path(home: &Path, name: &str) -> PathBuf {
    repos_dir(home).join(format!("{name}.git"))
}

pub fn create_bare(home: &Path, name: &str) -> Result<()> {
    if !valid_name(name) {
        return Err("Use letters, digits, dots, dashes or underscores.".into());
    }
    let path = repo_path(home, name);
    if path.exists() {
        return Err("A repo with that name already exists.".into());
    }
    std::fs::create_dir_all(repos_dir(home)).map_err(|e| e.to_string())?;
    git(
        &repos_dir(home),
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            path.to_str().unwrap_or_default(),
        ],
    )?;
    enable_push(&path)
}

pub fn enable_push(path: &Path) -> Result<()> {
    git(path, &["config", "http.receivepack", "true"]).map(|_| ())
}

pub fn list_repos(home: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(repos_dir(home))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_suffix(".git")
                .map(str::to_string)
        })
        .collect();
    names.sort();
    names
}

#[derive(Debug, Clone)]
pub struct Branch {
    pub name: String,
    pub sha: String,
    pub time: u64,
    pub subject: String,
}

pub fn branches(repo: &Path) -> Vec<Branch> {
    let out = git(
        repo,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname:short)%09%(objectname)%09%(committerdate:unix)%09%(subject)",
            "refs/heads/",
        ],
    )
    .unwrap_or_default();
    out.lines()
        .filter_map(|l| {
            let mut p = l.splitn(4, '\t');
            Some(Branch {
                name: p.next()?.into(),
                sha: p.next()?.into(),
                time: p.next()?.parse().ok()?,
                subject: p.next().unwrap_or("").into(),
            })
        })
        .collect()
}

pub fn tips(repo: &Path) -> BTreeMap<String, String> {
    branches(repo)
        .into_iter()
        .map(|b| (b.name, b.sha))
        .collect()
}

pub fn default_branch(repo: &Path) -> String {
    let head = git(repo, &["symbolic-ref", "--short", "HEAD"]).unwrap_or_default();
    let head = head.trim();
    let all = branches(repo);
    if all.iter().any(|b| b.name == head) {
        head.to_string()
    } else {
        all.first()
            .map(|b| b.name.clone())
            .unwrap_or_else(|| "main".into())
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    pub time: u64,
}

pub fn tree(repo: &Path, rev: &str, path: &str) -> Result<Vec<Entry>> {
    let spec = if path.is_empty() {
        rev.to_string()
    } else {
        format!("{rev}:{path}")
    };
    let out = git(repo, &["ls-tree", &spec])?;
    let mut entries: Vec<Entry> = out
        .lines()
        .filter_map(|l| {
            let (meta, name) = l.split_once('\t')?;
            let dir = meta.split(' ').nth(1)? == "tree";
            let full = if path.is_empty() {
                name.to_string()
            } else {
                format!("{path}/{name}")
            };
            let time = git(repo, &["log", "-1", "--format=%ct", rev, "--", &full])
                .ok()
                .and_then(|t| t.trim().parse().ok())
                .unwrap_or(0);
            Some(Entry {
                name: name.into(),
                dir,
                time,
            })
        })
        .collect();
    entries.sort_by(|a, b| b.dir.cmp(&a.dir).then(a.name.cmp(&b.name)));
    Ok(entries)
}

pub fn is_dir(repo: &Path, rev: &str, path: &str) -> bool {
    path.is_empty()
        || git(repo, &["cat-file", "-t", &format!("{rev}:{path}")])
            .map(|t| t.trim() == "tree")
            .unwrap_or(false)
}

pub fn read_file(repo: &Path, rev: &str, path: &str) -> Result<Vec<u8>> {
    git_env(repo, &["show", &format!("{rev}:{path}")], &[])
}

pub fn count(repo: &Path, range: &str) -> usize {
    git(repo, &["rev-list", "--count", range])
        .ok()
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(0)
}

pub fn is_ancestor(repo: &Path, a: &str, b: &str) -> bool {
    git(repo, &["merge-base", "--is-ancestor", a, b]).is_ok()
}

pub fn subject(repo: &Path, rev: &str) -> String {
    git(repo, &["log", "-1", "--format=%s", rev])
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

pub fn diff(repo: &Path, into: &str, from: &str) -> Result<String> {
    git(repo, &["diff", "--no-color", &format!("{into}...{from}")])
}

/// Squash-merges `from` into `into` and pushes the result back. Returns the new commit.
pub fn merge_squash(repo: &Path, into: &str, from: &str, message: &str) -> Result<String> {
    let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let work = tmp.path().join("w");
    let src = repo.to_str().ok_or("bad path")?;
    git(tmp.path(), &["clone", "-q", "--branch", into, src, "w"])?;
    git(
        &work,
        &["fetch", "-q", "origin", &format!("refs/heads/{from}")],
    )?;
    git(&work, &["merge", "--squash", "FETCH_HEAD"])
        .map_err(|_| "These branches conflict. Bring main into the change first.".to_string())?;
    if git(&work, &["diff", "--cached", "--quiet"]).is_ok() {
        return Err("Nothing to merge: the change is already in.".into());
    }
    let (name, email) = identity();
    git(
        &work,
        &[
            "-c",
            &format!("user.name={name}"),
            "-c",
            &format!("user.email={email}"),
            "commit",
            "-q",
            "-m",
            message,
        ],
    )?;
    git(
        &work,
        &["push", "-q", "origin", &format!("HEAD:refs/heads/{into}")],
    )?;
    git(&work, &["rev-parse", "HEAD"]).map(|s| s.trim().to_string())
}

/// The user's own git identity, so merges are theirs.
pub fn identity() -> (String, String) {
    let get = |k: &str| {
        Command::new("git")
            .args(["config", "--global", k])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    (
        get("user.name").unwrap_or_else(|| "Upstream".into()),
        get("user.email").unwrap_or_else(|| "upstream@localhost".into()),
    )
}

/// A throwaway checkout of one commit, for the runner.
pub fn checkout(repo: &Path, sha: &str, into: &Path) -> Result<()> {
    let src = repo.to_str().ok_or("bad path")?;
    git(
        Path::new("/"),
        &[
            "clone",
            "-q",
            "--no-checkout",
            src,
            into.to_str().ok_or("bad path")?,
        ],
    )?;
    git(into, &["checkout", "-q", "--detach", sha]).map(|_| ())
}

/// Emails used in a repo's history that would become public.
pub fn author_emails(repo: &Path, rev: &str) -> Vec<String> {
    let out = git(repo, &["log", "--format=%ae", rev]).unwrap_or_default();
    let mut v: Vec<String> = out.lines().map(str::to_string).collect();
    v.sort();
    v.dedup();
    v
}
