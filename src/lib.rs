//! RigSpark Upstream: private, local source control. Your code stays on this Mac until you're
//! ready to show the world.
//!
//! One loopback-only server holds bare git repos (cloned and pushed over HTTP), changes, runs,
//! diagnostics, the move in / send to GitHub flows and an MCP endpoint for Claude and Codex.

pub mod diag;
pub mod git;
pub mod github;
pub mod mcp;
pub mod model;
pub mod money;
pub mod runner;
pub mod scan;
pub mod store;
pub mod web;

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use store::{Change, ChangeState, GithubLink, State as Data};

pub struct App {
    pub home: PathBuf,
    pub port: u16,
    pub state: Mutex<Data>,
    /// Required on every form POST; only pages this server rendered carry it.
    pub form_token: String,
    pub approvals: Mutex<HashMap<u64, tokio::sync::oneshot::Sender<bool>>>,
    /// Short-lived GitHub sessions for one move in or one send. Tokens never reach disk.
    pub sessions: Mutex<HashMap<String, Session>>,
    pub jobs: Mutex<Vec<Job>>,
    pub run_slot: tokio::sync::Semaphore,
}

pub struct Session {
    pub token: String,
    pub login: String,
    pub created: Instant,
    pub repos: Vec<github::RemoteRepo>,
}

#[derive(Debug, Clone)]
pub struct Job {
    pub repo: String,
    pub status: String,
    pub done: bool,
    pub error: Option<String>,
}

pub fn random_hex(bytes: usize) -> String {
    use rand::RngCore;
    let mut b = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl App {
    pub fn new(home: PathBuf, port: u16) -> Arc<Self> {
        let _ = std::fs::create_dir_all(git::repos_dir(&home));
        let state = store::load(&home.join("state.json"));
        Arc::new(Self {
            home,
            port,
            state: Mutex::new(state),
            form_token: random_hex(16),
            approvals: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            jobs: Mutex::new(vec![]),
            run_slot: tokio::sync::Semaphore::new(1),
        })
    }

    pub fn save(&self) {
        let snapshot = self.state.lock().unwrap().clone();
        if let Err(e) = store::save(&self.home.join("state.json"), &snapshot) {
            eprintln!("upstream: could not save state: {e}");
        }
    }

    pub fn clone_url(&self, repo: &str) -> String {
        format!("http://127.0.0.1:{}/git/{repo}.git", self.port)
    }

    pub fn session(&self, id: &str) -> Option<(String, String, Vec<github::RemoteRepo>)> {
        let mut s = self.sessions.lock().unwrap();
        s.retain(|_, v| v.created.elapsed().as_secs() < 30 * 60);
        s.get(id)
            .map(|v| (v.token.clone(), v.login.clone(), v.repos.clone()))
    }
}

/// Opens a change (or returns the open one for the same branches) and makes sure its tip has a run.
pub fn open_change(
    app: &Arc<App>,
    repo: &str,
    from: &str,
    into: &str,
    title: &str,
    by: &str,
) -> Result<u64, String> {
    {
        let path = git::repo_path(&app.home, repo);
        if !path.exists() {
            return Err("No such repo.".into());
        }
        if !git::valid_branch(from) || !git::valid_branch(into) || from == into {
            return Err("Pick two different branches.".into());
        }
        let tips = git::tips(&path);
        if !tips.contains_key(from) || !tips.contains_key(into) {
            return Err("Both branches must exist.".into());
        }
        let tip = tips[from].clone();
        let mut st = app.state.lock().unwrap();
        if let Some(c) = st.changes.iter().find(|c| {
            c.repo == repo && c.from == from && c.into == into && c.state == ChangeState::Open
        }) {
            return Ok(c.id);
        }
        let id = st.new_id();
        let title = if title.trim().is_empty() {
            git::subject(&path, from)
        } else {
            title.trim().to_string()
        };
        st.changes.push(Change {
            id,
            repo: repo.into(),
            title,
            from: from.into(),
            into: into.into(),
            opened_by: by.into(),
            created: store::now(),
            state: ChangeState::Open,
            merged_sha: None,
        });
        let has_run = st.runs.iter().any(|r| r.repo == repo && r.sha == tip);
        drop(st);
        app.save();
        if !has_run {
            runner::enqueue(app, repo, from, &tip, None, 0);
        }
        Ok(id)
    }
}

pub fn router(app: Arc<App>) -> axum::Router {
    use axum::routing::any;
    web::routes()
        .route("/git/{*rest}", any(git_http))
        .route("/mcp", any(mcp::handle))
        .layer(axum::extract::DefaultBodyLimit::max(1 << 30))
        .layer(axum::middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}

/// Loopback only: refuses other Host names (DNS rebinding) and cross-site requests.
async fn guard(State(app): State<Arc<App>>, req: Request, next: Next) -> Response {
    let ok_host =
        |h: &str| h == format!("127.0.0.1:{}", app.port) || h == format!("localhost:{}", app.port);
    let host = req
        .headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if !ok_host(host) {
        return (StatusCode::FORBIDDEN, "Upstream only answers on 127.0.0.1").into_response();
    }
    if let Some(origin) = req.headers().get("origin").and_then(|h| h.to_str().ok()) {
        let o = origin.trim_start_matches("http://");
        if !ok_host(o) {
            return (StatusCode::FORBIDDEN, "Cross-site request refused").into_response();
        }
    }
    next.run(req).await
}

pub async fn serve(home: PathBuf, port: u16) -> std::io::Result<()> {
    let app = App::new(home, port);
    prune_logs(&app.home, 30);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    println!("Upstream is running at http://127.0.0.1:{port}/  (private to this Mac)");
    axum::serve(listener, router(app)).await
}

pub fn default_home() -> PathBuf {
    if let Ok(h) = std::env::var("UPSTREAM_HOME") {
        return PathBuf::from(h);
    }
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()));
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/org.rigspark.upstream")
    } else {
        std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".local/share"))
            .join("rigspark-upstream")
    }
}

/// Git smart HTTP through `git http-backend`. A push starts runs for every branch it moved.
async fn git_http(
    State(app): State<Arc<App>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let rest = uri.path().trim_start_matches("/git");
    let repo = rest
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("")
        .trim_end_matches(".git")
        .to_string();
    if !git::valid_name(&repo) || rest.contains("..") || !git::repo_path(&app.home, &repo).exists()
    {
        return (StatusCode::NOT_FOUND, "No such repo").into_response();
    }
    let pushing = rest.ends_with("/git-receive-pack") && method == Method::POST;
    let path = git::repo_path(&app.home, &repo);
    let before = if pushing {
        git::tips(&path)
    } else {
        Default::default()
    };

    let h = |k: &str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("http-backend")
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("GIT_PROJECT_ROOT", git::repos_dir(&app.home))
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("PATH_INFO", rest)
        .env("REQUEST_METHOD", method.as_str())
        .env("QUERY_STRING", uri.query().unwrap_or(""))
        .env("CONTENT_TYPE", h("content-type"))
        .env("CONTENT_LENGTH", body.len().to_string())
        .env("HTTP_CONTENT_ENCODING", h("content-encoding"))
        .env("HTTP_GIT_PROTOCOL", h("git-protocol"))
        .env("REMOTE_ADDR", "127.0.0.1")
        .env("REMOTE_USER", "upstream")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let Ok(mut child) = cmd.spawn() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "git http-backend failed to start",
        )
            .into_response();
    };
    let mut stdin = child.stdin.take().unwrap();
    let writer = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let _ = stdin.write_all(&body).await;
    });
    let out = match child.wait_with_output().await {
        Ok(o) => o.stdout,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let _ = writer.await;
    let resp = cgi_response(&out);
    if pushing {
        let after = git::tips(&path);
        for (branch, sha) in &after {
            if before.get(branch) != Some(sha) {
                runner::enqueue(&app, &repo, branch, sha, None, 0);
            }
        }
    }
    resp
}

fn cgi_response(out: &[u8]) -> Response {
    let split = out
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| (i, 4))
        .or_else(|| out.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2)));
    let Some((i, n)) = split else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "bad CGI output").into_response();
    };
    let head = String::from_utf8_lossy(&out[..i]);
    let mut builder = Response::builder().status(200);
    for line in head.lines() {
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim();
            if k.eq_ignore_ascii_case("status") {
                let code = v
                    .split_whitespace()
                    .next()
                    .and_then(|c| c.parse().ok())
                    .unwrap_or(200);
                builder = builder.status(code);
            } else {
                builder = builder.header(k.trim(), v);
            }
        }
    }
    builder
        .body(Body::from(out[i + n..].to_vec()))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Moves one GitHub repo onto this Mac. Runs on a blocking thread; progress goes to `app.jobs`.
pub fn move_in(app: &Arc<App>, token: &str, login: &str, remote: &github::RemoteRepo) {
    let name = remote.name.clone();
    let set = |status: &str, done: bool, error: Option<String>| {
        let mut jobs = app.jobs.lock().unwrap();
        if let Some(j) = jobs.iter_mut().find(|j| j.repo == name) {
            j.status = status.into();
            j.done = done;
            j.error = error;
        }
    };
    let fail = |e: String| set("Couldn’t move in", true, Some(e));
    if !git::valid_name(&name) {
        return fail("That name can’t be used here.".into());
    }
    let path = git::repo_path(&app.home, &name);
    if path.exists() {
        return fail("A repo with that name is already here.".into());
    }
    set("Copying code and history", false, None);
    let env = github::git_auth_env(token);
    let url = github::remote_url(&remote.owner, &remote.name);
    if let Err(e) = git::git_env(
        &git::repos_dir(&app.home),
        &[
            "clone",
            "-q",
            "--mirror",
            &url,
            path.to_str().unwrap_or_default(),
        ],
        &env,
    ) {
        let _ = std::fs::remove_dir_all(&path);
        return fail(e);
    }
    // Keep GitHub as a remote named `github` for Send, without mirror pushes or ref clobbering.
    let _ = git::git(&path, &["remote", "rename", "origin", "github"]);
    let _ = git::git(&path, &["config", "--unset", "remote.github.mirror"]);
    let _ = git::git(
        &path,
        &[
            "config",
            "remote.github.fetch",
            "+refs/heads/*:refs/remotes/github/*",
        ],
    );
    // GitHub's pull request refs came along with the mirror; they aren't branches here.
    if let Ok(refs) = git::git(
        &path,
        &["for-each-ref", "--format=%(refname)", "refs/pull/"],
    ) {
        for r in refs.lines() {
            let _ = git::git(&path, &["update-ref", "-d", r]);
        }
    }
    let _ = git::enable_push(&path);
    let tips = git::tips(&path);

    set("Converting workflows", false, None);
    let main = git::default_branch(&path);
    let workflows: Vec<String> = git::tree(&path, &main, ".github/workflows")
        .unwrap_or_default()
        .iter()
        .filter(|e| !e.dir && (e.name.ends_with(".yml") || e.name.ends_with(".yaml")))
        .filter_map(|e| git::read_file(&path, &main, &format!(".github/workflows/{}", e.name)).ok())
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .collect();
    let (converted, needs) = runner::convert_actions(&workflows);

    set("Reading pull requests and billing", false, None);
    let ids = github::workflow_ids(token, &remote.owner, &remote.name);
    let spend = github::actions_spend(token, login, &remote.name);
    let prs = open_pull_requests(token, &remote.owner, &remote.name);
    {
        let mut st = app.state.lock().unwrap();
        let m = st.meta(&name);
        m.github = Some(GithubLink {
            owner: remote.owner.clone(),
            name: remote.name.clone(),
            private: remote.private,
            workflow_ids: ids,
        });
        m.left_github = tips.clone();
        m.converted = converted;
        m.needs_you = needs;
        if let Some((total, last)) = spend {
            m.lost_usd = Some(total);
            m.last_month_usd = Some(last);
        }
    }
    app.save();
    for (title, head, base, by) in prs {
        if tips.contains_key(&head) && tips.contains_key(&base) {
            let _ = open_change(app, &name, &head, &base, &title, &by);
        }
    }
    set("Moved in", true, None);
}

fn open_pull_requests(
    token: &str,
    owner: &str,
    name: &str,
) -> Vec<(String, String, String, String)> {
    let url = format!(
        "{}/repos/{owner}/{name}/pulls?state=open&per_page=100",
        github::api_base()
    );
    let Ok(resp) = ureq::get(&url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("User-Agent", "rigspark-upstream")
        .call()
    else {
        return vec![];
    };
    let v: serde_json::Value = resp.into_json().unwrap_or_default();
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            Some((
                p["title"].as_str()?.into(),
                p["head"]["ref"].as_str()?.into(),
                p["base"]["ref"].as_str()?.into(),
                p["user"]["login"].as_str().unwrap_or("you").into(),
            ))
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct BranchSync {
    pub name: String,
    pub local: String,
    /// Commits made here that GitHub doesn't have.
    pub new: usize,
    pub state: SyncState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncState {
    Ready,
    NewBranch,
    UpToDate,
    GithubChanged,
}

/// Where each branch stands against GitHub right now.
pub fn branch_sync(
    app: &App,
    repo: &str,
    token: &str,
    owner: &str,
    name: &str,
) -> Result<Vec<BranchSync>, String> {
    let path = git::repo_path(&app.home, repo);
    let env = github::git_auth_env(token);
    let out = git::git_env(
        &path,
        &["ls-remote", "--heads", &github::remote_url(owner, name)],
        &env,
    )?;
    let remote: HashMap<String, String> = String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|l| {
            let (sha, r) = l.split_once('\t')?;
            Some((r.strip_prefix("refs/heads/")?.to_string(), sha.to_string()))
        })
        .collect();
    let have =
        |sha: &str| git::git(&path, &["cat-file", "-e", &format!("{sha}^{{commit}}")]).is_ok();
    Ok(git::branches(&path)
        .into_iter()
        .map(|b| {
            let (state, new) = match remote.get(&b.name) {
                None => (SyncState::NewBranch, git::count(&path, &b.sha)),
                Some(r) if *r == b.sha => (SyncState::UpToDate, 0),
                Some(r) if have(r) && git::is_ancestor(&path, r, &b.sha) => (
                    SyncState::Ready,
                    git::count(&path, &format!("{r}..{}", b.sha)),
                ),
                Some(_) => (SyncState::GithubChanged, 0),
            };
            BranchSync {
                name: b.name,
                local: b.sha,
                new,
                state,
            }
        })
        .collect())
}

/// Pushes the chosen branches (never forced) and records where GitHub now stands.
pub fn send(
    app: &App,
    repo: &str,
    token: &str,
    owner: &str,
    name: &str,
    branches: &[String],
) -> Result<usize, String> {
    let path = git::repo_path(&app.home, repo);
    let env = github::git_auth_env(token);
    let url = github::remote_url(owner, name);
    let mut sent = 0;
    for b in branches {
        if !git::valid_branch(b) {
            continue;
        }
        git::git_env(
            &path,
            &[
                "push",
                "-q",
                &url,
                &format!("refs/heads/{b}:refs/heads/{b}"),
            ],
            &env,
        )?;
        let sha = git::tips(&path).get(b).cloned().unwrap_or_default();
        app.state
            .lock()
            .unwrap()
            .meta(repo)
            .left_github
            .insert(b.clone(), sha);
        sent += 1;
    }
    app.save();
    Ok(sent)
}

/// Problems that must be fixed before a repo goes public.
pub fn public_blockers(app: &App, repo: &str) -> Vec<String> {
    let path = git::repo_path(&app.home, repo);
    let main = git::default_branch(&path);
    let mut out = vec![];
    let history =
        git::git(&path, &["log", "-p", "--no-color", "-n", "5000", &main]).unwrap_or_default();
    let found = scan::scan_text(&history);
    if !found.is_empty() {
        out.push(format!(
            "Ward found {} possible secret{} in history ({}).",
            found.len(),
            if found.len() == 1 { "" } else { "s" },
            found[0].kind
        ));
    }
    let personal: Vec<String> = git::author_emails(&path, &main)
        .into_iter()
        .filter(|e| scan::personal_email(e))
        .collect();
    if !personal.is_empty() {
        out.push(format!(
            "{} commit email{} would become public: {}.",
            personal.len(),
            if personal.len() == 1 { "" } else { "s" },
            personal.join(", ")
        ));
    }
    out
}

pub fn repo_exists(home: &Path, repo: &str) -> bool {
    git::valid_name(repo) && git::repo_path(home, repo).exists()
}

/// Logs and traces are kept on this Mac for `days` days.
pub fn prune_logs(home: &Path, days: u64) {
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86_400);
    for e in std::fs::read_dir(home.join("logs"))
        .into_iter()
        .flatten()
        .flatten()
    {
        if e.metadata()
            .and_then(|m| m.modified())
            .map(|t| t < cutoff)
            .unwrap_or(false)
        {
            let _ = std::fs::remove_file(e.path());
        }
    }
}
