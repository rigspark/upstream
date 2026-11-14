//! MCP endpoint for Claude and Codex (streamable HTTP, JSON responses).
//!
//! Each assistant has its own key and can be switched off on its own. The tools read code, open
//! changes and read runs. Merging, sending to GitHub and approving deploys are deliberately not
//! tools at all, so nothing an assistant reads can talk it into doing them.

use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::store::{now, ChangeState};
use crate::{git, runner, App};

pub const ASSISTANTS: [&str; 2] = ["Claude", "Codex"];
const MAX_FILE: usize = 256 * 1024;

fn who(app: &App, headers: &HeaderMap) -> Option<String> {
    let key = headers
        .get("authorization")?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?
        .trim()
        .to_string();
    let mut st = app.state.lock().unwrap();
    let (name, a) = st
        .settings
        .assistants
        .iter_mut()
        .find(|(_, a)| a.enabled && !a.key.is_empty() && a.key == key)?;
    a.last_used = Some(now());
    Some(name.clone())
}

pub async fn handle(
    State(app): State<Arc<App>>,
    method: Method,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if method != Method::POST {
        return (StatusCode::METHOD_NOT_ALLOWED, "POST JSON-RPC to /mcp").into_response();
    }
    let Some(name) = who(&app, &headers) else {
        return (
            StatusCode::UNAUTHORIZED,
            [("www-authenticate", "Bearer")],
            "Switch this assistant on in Upstream › Connect AI and use its key.",
        )
            .into_response();
    };
    app.save();
    let Ok(msg) = serde_json::from_slice::<Value>(&body) else {
        return Json(err(Value::Null, -32700, "parse error")).into_response();
    };
    let app2 = app.clone();
    let reply = tokio::task::spawn_blocking(move || dispatch(&app2, &name, &msg))
        .await
        .unwrap_or(None);
    match reply {
        Some(v) => Json(v).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

fn err(id: Value, code: i64, msg: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}})
}

fn ok(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// One JSON-RPC message in, at most one out (notifications get none).
pub fn dispatch(app: &Arc<App>, who: &str, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg["method"].as_str().unwrap_or("");
    let id = id?;
    Some(match method {
        "initialize" => ok(
            id,
            json!({
                "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "rigspark-upstream", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "Upstream hosts this user's private repos on their Mac. Read code, open changes for the user to review, and read runs. You cannot merge, send to GitHub or approve deploys: ask the user."
            }),
        ),
        "ping" => ok(id, json!({})),
        "tools/list" => ok(id, json!({"tools": tools()})),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or("");
            let args = &msg["params"]["arguments"];
            match call(app, who, name, args) {
                Ok(v) => ok(
                    id,
                    json!({"content": [{"type": "text", "text": v}], "isError": false}),
                ),
                Err(e) => ok(
                    id,
                    json!({"content": [{"type": "text", "text": e}], "isError": true}),
                ),
            }
        }
        _ => err(id, -32601, "method not found"),
    })
}

fn tools() -> Value {
    let s = |props: Value, req: &[&str]| json!({"type": "object", "properties": props, "required": req});
    json!([
        {"name": "list_repos", "description": "List the user's repos on Upstream with their default branch.", "inputSchema": s(json!({}), &[])},
        {"name": "list_files", "description": "List files and folders in a repo at a branch or commit.", "inputSchema": s(json!({"repo": {"type": "string"}, "ref": {"type": "string"}, "path": {"type": "string"}}), &["repo"])},
        {"name": "read_file", "description": "Read one file (up to 256 KB).", "inputSchema": s(json!({"repo": {"type": "string"}, "ref": {"type": "string"}, "path": {"type": "string"}}), &["repo", "path"])},
        {"name": "list_changes", "description": "Open changes, newest first.", "inputSchema": s(json!({"repo": {"type": "string"}}), &[])},
        {"name": "open_change", "description": "Open a change from a branch you pushed, for the user to review and merge. Push the branch with git first (the clone URL is in list_repos).", "inputSchema": s(json!({"repo": {"type": "string"}, "from": {"type": "string"}, "into": {"type": "string"}, "title": {"type": "string"}}), &["repo", "from", "title"])},
        {"name": "list_runs", "description": "Recent runs with status and duration.", "inputSchema": s(json!({"repo": {"type": "string"}}), &[])},
        {"name": "get_run_log", "description": "A run's log. By default only the failing step's last 80 lines.", "inputSchema": s(json!({"run_id": {"type": "integer"}, "full": {"type": "boolean"}}), &["run_id"])}
    ])
}

fn arg<'a>(args: &'a Value, k: &str) -> Option<&'a str> {
    args.get(k)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

fn repo_arg(app: &App, args: &Value) -> Result<String, String> {
    let r = arg(args, "repo").ok_or("repo is required")?;
    if !crate::repo_exists(&app.home, r) {
        return Err(format!("No repo named {r}. Call list_repos."));
    }
    Ok(r.to_string())
}

fn call(app: &Arc<App>, who: &str, name: &str, args: &Value) -> Result<String, String> {
    match name {
        "list_repos" => {
            let v: Vec<Value> = git::list_repos(&app.home)
                .into_iter()
                .map(|r| json!({"repo": r, "default_branch": git::default_branch(&git::repo_path(&app.home, &r)), "clone_url": app.clone_url(&r)}))
                .collect();
            Ok(serde_json::to_string_pretty(&v).unwrap_or_default())
        }
        "list_files" => {
            let repo = repo_arg(app, args)?;
            let path = git::repo_path(&app.home, &repo);
            let rev = arg(args, "ref")
                .map(str::to_string)
                .unwrap_or_else(|| git::default_branch(&path));
            let dir = arg(args, "path").unwrap_or("").trim_matches('/');
            if !git::valid_path(dir) {
                return Err("bad path".into());
            }
            let entries = git::tree(&path, &rev, dir)?;
            Ok(entries
                .iter()
                .map(|e| {
                    if e.dir {
                        format!("{}/", e.name)
                    } else {
                        e.name.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"))
        }
        "read_file" => {
            let repo = repo_arg(app, args)?;
            let path = git::repo_path(&app.home, &repo);
            let rev = arg(args, "ref")
                .map(str::to_string)
                .unwrap_or_else(|| git::default_branch(&path));
            let file = arg(args, "path")
                .ok_or("path is required")?
                .trim_matches('/');
            if !git::valid_path(file) {
                return Err("bad path".into());
            }
            let bytes = git::read_file(&path, &rev, file)?;
            if bytes.len() > MAX_FILE {
                return Err(format!(
                    "{file} is {} KB; files over 256 KB aren't returned.",
                    bytes.len() / 1024
                ));
            }
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        }
        "list_changes" => {
            let filter = arg(args, "repo");
            let st = app.state.lock().unwrap();
            let v: Vec<Value> = st
                .changes
                .iter()
                .rev()
                .filter(|c| c.state == ChangeState::Open && filter.map(|r| r == c.repo).unwrap_or(true))
                .map(|c| json!({"id": c.id, "repo": c.repo, "title": c.title, "from": c.from, "into": c.into, "opened_by": c.opened_by}))
                .collect();
            Ok(serde_json::to_string_pretty(&v).unwrap_or_default())
        }
        "open_change" => {
            let repo = repo_arg(app, args)?;
            let from = arg(args, "from").ok_or("from is required")?;
            let into = arg(args, "into")
                .map(str::to_string)
                .unwrap_or_else(|| git::default_branch(&git::repo_path(&app.home, &repo)));
            let title = arg(args, "title").unwrap_or("");
            let id = crate::open_change(app, &repo, from, &into, title, who)?;
            Ok(format!(
                "Opened change #{id}. The user reviews and merges it in Upstream › Changes."
            ))
        }
        "list_runs" => {
            let filter = arg(args, "repo");
            let st = app.state.lock().unwrap();
            let v: Vec<Value> = st
                .runs
                .iter()
                .rev()
                .filter(|r| filter.map(|f| f == r.repo).unwrap_or(true))
                .take(30)
                .map(|r| json!({"id": r.id, "repo": r.repo, "branch": r.branch, "commit": r.sha, "title": r.title, "status": r.status, "seconds": r.duration_ms().map(|m| m / 1000), "failed_test": r.failed_test, "cause": r.cause}))
                .collect();
            Ok(serde_json::to_string_pretty(&v).unwrap_or_default())
        }
        "get_run_log" => {
            let id = args
                .get("run_id")
                .and_then(|v| v.as_u64())
                .ok_or("run_id is required")?;
            let run = app
                .state
                .lock()
                .unwrap()
                .run(id)
                .cloned()
                .ok_or("No such run")?;
            let lines = runner::read_log(&app.home, id);
            let full = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
            let failed = run
                .steps
                .iter()
                .position(|s| s.status == crate::store::StepStatus::Failed);
            let text: Vec<String> = match (full, failed) {
                (false, Some(i)) => runner::tail(&lines, i, 80),
                _ => lines
                    .iter()
                    .map(|l| {
                        format!(
                            "[{}] {}",
                            run.steps.get(l.s).map(|s| s.name.as_str()).unwrap_or("?"),
                            l.l
                        )
                    })
                    .collect(),
            };
            Ok(text.join("\n"))
        }
        _ => Err(format!("Unknown tool {name}")),
    }
}
