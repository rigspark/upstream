//! The screens, rendered on the server: three tabs, one main button per screen.

use axum::extract::{Path as P, Query, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use std::collections::HashMap;
use std::sync::Arc;

use crate::store::{now, ChangeState, Run, RunStatus, StepStatus};
use crate::{diag, git, github, mcp, money, runner, scan, App, SyncState};

type Q = Query<HashMap<String, String>>;
type F = Form<HashMap<String, String>>;

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/", get(repos))
        .route("/new", get(new_repo_page).post(new_repo))
        .route("/r/{repo}", get(code))
        .route("/r/{repo}/send", get(send_page).post(send_post))
        .route("/r/{repo}/send/token", post(send_token))
        .route("/changes", get(changes).post(change_create))
        .route("/changes/new", get(change_new))
        .route("/c/{id}", get(change))
        .route("/c/{id}/merge", post(merge))
        .route("/c/{id}/close", post(close))
        .route("/runs", get(runs))
        .route("/run/{id}", get(run))
        .route("/run/{id}/rerun", post(rerun))
        .route("/run/{id}/approve", post(approve))
        .route("/diagnostics", get(diagnostics))
        .route("/move-in", get(move_in_page))
        .route("/move-in/token", post(move_in_token))
        .route("/move-in/start", post(move_in_start))
        .route("/connect", get(connect))
        .route("/connect/{name}", post(connect_post))
        .route("/copilot", post(copilot))
        .route("/plan", post(plan))
}

// ---------- helpers

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn ago(t: u64) -> String {
    let d = now().saturating_sub(t);
    match d {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86_399 => format!("{} h ago", d / 3600),
        86_400..=172_799 => "yesterday".into(),
        172_800..=604_799 => format!("{} days ago", d / 86_400),
        _ => format!("{} weeks ago", d / 604_800),
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

fn checked(app: &App, f: &HashMap<String, String>) -> bool {
    f.get("t").map(|t| t == &app.form_token).unwrap_or(false)
}

fn bad() -> Response {
    (
        axum::http::StatusCode::FORBIDDEN,
        "This form expired. Go back and try again.",
    )
        .into_response()
}

fn hidden(app: &App) -> String {
    format!(
        r#"<input type="hidden" name="t" value="{}">"#,
        app.form_token
    )
}

fn run_dot(s: RunStatus) -> &'static str {
    match s {
        RunStatus::Passed => "g",
        RunStatus::Failed | RunStatus::Denied => "r",
        RunStatus::Waiting => "a",
        _ => "n",
    }
}

const WARN: &str = r##"<svg width="14" height="14" viewBox="0 0 24 24" role="img" aria-label="Warning" style="flex-shrink:0;align-self:center"><path d="M12 3 2 21h20L12 3z" fill="none" stroke="#d70015" stroke-width="2.4" stroke-linejoin="round"/><path d="M12 10v5M12 18v.01" stroke="#d70015" stroke-width="2.4" stroke-linecap="round"/></svg>"##;

const LOGO: &str = r##"<svg width="22" height="22" viewBox="0 0 64 64" aria-hidden="true"><path d="M32 5C45.5 5 54 13.5 54 27V52C54 56.4 50.4 60 46 60H18C13.6 60 10 56.4 10 52V27C10 13.5 18.5 5 32 5Z" fill="#1D1D1F"/><rect x="14" y="21" width="36" height="19" rx="8.5" fill="#FFAD00"/><path d="M21 31.2q3-3.2 6 0M37 31.2q3-3.2 6 0" stroke="#1D1D1F" stroke-width="2.4" stroke-linecap="round" fill="none"/><path d="M29.6 34.2q2.4 2.6 4.8 0" stroke="#1D1D1F" stroke-width="1.9" stroke-linecap="round" fill="none"/></svg>"##;

const CSS: &str = r#"
*{box-sizing:border-box}
body{margin:0;font:15px/1.5 Inter,-apple-system,BlinkMacSystemFont,"SF Pro Text",system-ui,sans-serif;color:#1d1d1f;background:#fff;-webkit-font-smoothing:antialiased}
a{color:#1d1d1f}a:hover{color:#a35500}
.app{display:flex;flex-wrap:wrap;min-height:100vh}
.nav{flex:0 0 200px;padding:20px 14px;display:flex;flex-direction:column;gap:4px}
.brand{display:flex;align-items:center;gap:10px;padding:0 10px 28px;font-size:15px;font-weight:600}
.nav a{display:flex;align-items:center;min-height:40px;padding:0 10px;border-radius:10px;text-decoration:none;color:#6e6e73}
.nav a[aria-current=page]{color:#1d1d1f;font-weight:600;background:#f5f5f7}
.nav .count{margin-left:auto;font-size:12px;font-weight:600;color:#a35500}
.priv{display:flex;align-items:center;gap:8px;padding:8px 10px 0;font-size:13px;color:#6e6e73}
.dot{width:8px;height:8px;border-radius:50%;display:inline-block;flex-shrink:0}
.g{background:#248a3d}.a{background:#ffad00}.r{background:#d70015}.n{background:#c7c7cc}
main{flex:999 1 560px;min-width:0;padding:48px 56px 64px}
.wrap{max-width:760px;display:flex;flex-direction:column;gap:32px}
.top{display:flex;flex-wrap:wrap;align-items:baseline;gap:16px}
h1{margin:0;font-size:34px;font-weight:600;letter-spacing:-.02em;flex:1;word-break:break-word}
.sub{color:#6e6e73}.small{font-size:13px}
.btn{display:inline-flex;align-items:center;justify-content:center;min-height:44px;padding:0 22px;border:0;border-radius:22px;font:inherit;font-weight:600;background:#ffad00;color:#1d1d1f;cursor:pointer;text-decoration:none}
.btn.ghost{background:none;color:#a35500;padding:0 8px}
.btn:disabled{background:#f5f5f7;color:#6e6e73;cursor:default}
form.inline{display:inline}
.list{display:flex;flex-direction:column}
.item{display:flex;align-items:center;gap:14px;min-height:64px;border-bottom:1px solid rgba(0,0,0,.07);text-decoration:none;color:#1d1d1f}
.item:last-child{border-bottom:0}
.item .end{margin-left:auto;color:#6e6e73;font-size:14px;text-align:right;white-space:nowrap}
.two{display:flex;flex-direction:column}.two .sub{font-size:14px}
.mono,code{font-family:"JetBrains Mono",ui-monospace,SFMono-Regular,Menlo,monospace;font-size:13px}
.big{font-size:56px;font-weight:600;letter-spacing:-.03em;line-height:1}
.save{color:#1d6b30}.lost{color:#b00012}
.warn{color:#b00012;display:flex;gap:10px;align-items:baseline}.warn a,.warn button{color:#b00012;font-weight:600}
pre{margin:0;white-space:pre-wrap;word-break:break-word;font:13px/1.7 "JetBrains Mono",ui-monospace,Menlo,monospace;padding:20px;border-radius:16px;background:#f5f5f7;overflow:auto;max-height:70vh}
.add{display:block;background:rgba(36,138,61,.10)}.del{display:block;background:rgba(215,0,21,.08)}.hunk{display:block;color:#6e6e73}
.seg{display:inline-flex;align-self:flex-start;padding:3px;border-radius:12px;background:#f5f5f7;gap:2px;flex-wrap:wrap}
.seg a,.seg label{display:flex;align-items:center;min-height:38px;padding:0 16px;border-radius:9px;text-decoration:none;color:#6e6e73;cursor:pointer}
.seg a.on,.seg label:has(input:checked){background:#fff;color:#1d1d1f;font-weight:600;box-shadow:0 1px 2px rgba(0,0,0,.08)}
.seg input{position:absolute;opacity:0;width:1px;height:1px}
.seg .off{color:#c7c7cc;cursor:default}
.field{min-height:48px;width:100%;font:inherit;border:1px solid rgba(0,0,0,.12);border-radius:12px;padding:0 16px;background:#fff}
select.field{width:auto}
.foot{display:flex;flex-wrap:wrap;align-items:center;gap:16px}.foot .push{margin-left:auto}
.check{display:flex;align-items:center;gap:14px;min-height:56px;border-bottom:1px solid rgba(0,0,0,.07)}
.check input{width:18px;height:18px;accent-color:#ffad00}
.flash{padding:14px 18px;border-radius:14px;background:#f5f5f7}
.ask{display:flex;flex-wrap:wrap;gap:12px;align-items:center;padding:20px 24px;border-radius:16px;background:#fff8e6}
.switch{position:relative;width:51px;height:31px;flex-shrink:0}
.switch input{opacity:0;width:0;height:0}
.switch span{position:absolute;inset:0;border-radius:16px;background:#d1d1d6;transition:background .2s;cursor:pointer}
.switch span:before{content:"";position:absolute;top:2px;left:2px;width:27px;height:27px;border-radius:50%;background:#fff;box-shadow:0 2px 4px rgba(0,0,0,.2);transition:left .2s}
.switch input:checked+span{background:#248a3d}.switch input:checked+span:before{left:22px}
.switch input:focus-visible+span{outline:2px solid #a35500;outline-offset:2px}
details summary{cursor:pointer;color:#6e6e73}
.wf{display:grid;grid-template-columns:150px minmax(0,1fr) 140px;gap:16px;align-items:center;min-height:44px;border-bottom:1px solid rgba(0,0,0,.07)}
.bar{height:10px;border-radius:5px}
.stats{display:flex;flex-wrap:wrap;gap:24px}.stat{flex:1 1 140px;display:flex;flex-direction:column}.stat b{font-size:28px;letter-spacing:-.02em}
@media (max-width:700px){main{padding:24px 20px}.nav{flex-basis:100%;flex-direction:row;flex-wrap:wrap;padding:12px}.brand{padding:0 10px}.wf{grid-template-columns:100px minmax(0,1fr)}.wf .sub{display:none}}
"#;

fn page(app: &App, title: &str, tab: &str, body: &str) -> Html<String> {
    let waiting = {
        let st = app.state.lock().unwrap();
        st.runs
            .iter()
            .filter(|r| r.status == RunStatus::Waiting)
            .count()
    };
    let nav = |name: &str, href: &str, count: usize| {
        let cur = if name == tab {
            r#" aria-current="page""#
        } else {
            ""
        };
        let c = if count > 0 {
            format!(r#"<span class="count">{count}</span>"#)
        } else {
            String::new()
        };
        format!(r#"<a href="{href}"{cur}>{name}{c}</a>"#)
    };
    let connect_cur = if tab == "Connect" {
        r#" aria-current="page""#
    } else {
        ""
    };
    Html(format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{} · Upstream</title><style>{CSS}</style></head><body><div class="app">
<nav class="nav" aria-label="Upstream"><div class="brand">{LOGO}Upstream</div>{}{}{}<a href="/connect" style="margin-top:auto"{connect_cur}>Connect AI</a><div class="priv"><span class="dot g"></span>Private to this Mac</div></nav>
<main><div class="wrap">{body}</div></main></div>
<script>document.addEventListener('click',e=>{{const b=e.target.closest('[data-copy]');if(!b)return;navigator.clipboard.writeText(b.dataset.copy).then(()=>{{b.textContent='Copied';setTimeout(()=>b.textContent='Copy',1500)}})}})</script></body></html>"#,
        esc(title),
        nav("Repos", "/", 0),
        nav("Changes", "/changes", waiting_changes(app)),
        nav("Runs", "/runs", waiting),
    ))
}

fn flash(q: &HashMap<String, String>) -> String {
    q.get("msg")
        .map(|m| format!(r#"<div class="flash" role="status">{}</div>"#, esc(m)))
        .unwrap_or_default()
}

fn copy(text: &str) -> String {
    format!(
        r#"<button class="btn ghost" type="button" data-copy="{}">Copy</button>"#,
        esc(text)
    )
}

fn copilot_line(app: &App, back: &str) -> String {
    if app.state.lock().unwrap().settings.copilot_ack.is_some() {
        return String::new();
    }
    format!(
        r#"<div class="warn">{WARN}<span>Copilot may be training on your code. It’s on by default for personal plans. <a href="https://github.com/settings/copilot/features" target="_blank" rel="noopener">Turn it off</a>, then
<form class="inline" method="post" action="/copilot">{}<input type="hidden" name="back" value="{back}"><button class="btn ghost" name="choice" value="off" style="min-height:0;padding:0 4px">I’ve turned it off</button> or <button class="btn ghost" name="choice" value="none" style="min-height:0;padding:0 4px">I don’t use Copilot</button></form></span></div>"#,
        hidden(app)
    )
}

// ---------- Repos

async fn repos(State(app): State<Arc<App>>, q: Q) -> Html<String> {
    let names = git::list_repos(&app.home);
    let (items, saved, lost) = {
        let st = app.state.lock().unwrap();
        let items: String = names
            .iter()
            .map(|n| {
                let path = git::repo_path(&app.home, n);
                let last = git::branches(&path).iter().map(|b| b.time).max();
                let dot = st.runs.iter().rev().find(|r| &r.repo == n).map(|r| run_dot(r.status)).unwrap_or("n");
                let when = last.map(ago).unwrap_or_else(|| "empty".into());
                format!(r#"<a class="item" href="/r/{0}"><span class="dot {dot}"></span><b>{0}</b><span class="end">{when}</span></a>"#, esc(n))
            })
            .collect();
        let (saved, _) = money::saved(&st.runs, &st.settings.plan, now());
        let lost: f64 = st.repos.values().filter_map(|m| m.lost_usd).sum();
        (items, saved, lost)
    };
    let list = if names.is_empty() {
        r#"<div class="sub">No repos yet. Move them in from GitHub, or start a new one.</div>"#
            .to_string()
    } else {
        format!(r#"<div class="list">{items}</div>"#)
    };
    let money = if saved > 0.0 {
        format!(
            r#"<a href="/runs" class="save" style="text-decoration:none">Saved {} this month.</a>"#,
            money::usd(saved)
        )
    } else if lost > 0.0 {
        format!(
            r#"<a href="/runs" class="save" style="text-decoration:none">No more Actions bills. You paid GitHub {} before.</a>"#,
            money::usd(lost)
        )
    } else {
        String::new()
    };
    page(
        &app,
        "Repos",
        "Repos",
        &format!(
            r#"{}<div class="top"><h1>Repos</h1><a class="btn ghost" href="/move-in">Move in</a><a class="btn" href="/new">New</a></div>{}{list}{money}"#,
            flash(&q.0),
            copilot_line(&app, "/")
        ),
    )
}

async fn new_repo_page(State(app): State<Arc<App>>, q: Q) -> Html<String> {
    page(
        &app,
        "New repo",
        "Repos",
        &format!(
            r#"{}<div class="top"><h1>New repo</h1></div><form method="post" action="/new" style="display:flex;flex-direction:column;gap:24px">{}<label style="display:flex;flex-direction:column;gap:8px"><span class="sub">Name</span><input class="field" name="name" required autofocus pattern="[A-Za-z0-9_][A-Za-z0-9._\-]*" maxlength="100"></label><div class="foot"><button class="btn push">Create</button></div></form>"#,
            flash(&q.0),
            hidden(&app)
        ),
    )
}

async fn new_repo(State(app): State<Arc<App>>, f: F) -> Response {
    if !checked(&app, &f) {
        return bad();
    }
    let name = f.get("name").map(|s| s.trim()).unwrap_or("");
    match git::create_bare(&app.home, name) {
        Ok(()) => Redirect::to(&format!("/r/{}", enc(name))).into_response(),
        Err(e) => Redirect::to(&format!("/new?msg={}", enc(&e))).into_response(),
    }
}

// ---------- Code

async fn code(State(app): State<Arc<App>>, P(repo): P<String>, q: Q) -> Response {
    if !crate::repo_exists(&app.home, &repo) {
        return Redirect::to("/").into_response();
    }
    let path = git::repo_path(&app.home, &repo);
    let branches = git::branches(&path);
    let url = app.clone_url(&repo);
    let r = esc(&repo);
    if branches.is_empty() {
        let cmd = format!("git remote add upstream {url}\ngit push upstream main");
        return page(
            &app,
            &repo,
            "Repos",
            &format!(r#"{}<div class="top"><h1>{r}</h1></div><div class="sub">Empty. Push your code here from your project folder:</div><pre>{}</pre><div class="foot">{}</div>"#, flash(&q.0), esc(&cmd), copy(&cmd)),
        )
        .into_response();
    }
    let def = git::default_branch(&path);
    let rev = q
        .get("ref")
        .filter(|b| branches.iter().any(|x| &x.name == *b))
        .cloned()
        .unwrap_or(def);
    let sub = q
        .get("path")
        .map(|p| p.trim_matches('/').to_string())
        .unwrap_or_default();
    if !git::valid_path(&sub) {
        return Redirect::to(&format!("/r/{}", enc(&repo))).into_response();
    }
    let since = {
        let st = app.state.lock().unwrap();
        st.repos
            .get(&repo)
            .and_then(|m| m.left_github.get(&rev).cloned())
    };
    let since = since
        .map(|left| git::count(&path, &format!("{left}..{rev}")))
        .filter(|n| *n > 0)
        .map(|n| {
            format!(
                " · {n} commit{} since you moved in",
                if n == 1 { "" } else { "s" }
            )
        })
        .unwrap_or_default();
    let options: String = branches
        .iter()
        .map(|b| {
            format!(
                r#"<option{}>{}</option>"#,
                if b.name == rev { " selected" } else { "" },
                esc(&b.name)
            )
        })
        .collect();
    let switcher = format!(
        r#"<form method="get" action="/r/{}" style="display:flex;gap:8px;align-items:center"><label class="sub small" for="ref">Branch</label><select class="field mono" id="ref" name="ref" onchange="this.form.submit()" style="min-height:36px">{options}</select><noscript><button class="btn ghost">Go</button></noscript></form>"#,
        enc(&repo)
    );
    let crumbs = if sub.is_empty() {
        String::new()
    } else {
        let mut acc = String::new();
        let mut out = format!(r#"<a href="/r/{}?ref={}">{r}</a>"#, enc(&repo), enc(&rev));
        for part in sub.split('/') {
            acc = if acc.is_empty() {
                part.to_string()
            } else {
                format!("{acc}/{part}")
            };
            out += &format!(
                r#" / <a href="/r/{}?ref={}&path={}">{}</a>"#,
                enc(&repo),
                enc(&rev),
                enc(&acc),
                esc(part)
            );
        }
        format!(r#"<div class="mono">{out}</div>"#)
    };
    let content = if git::is_dir(&path, &rev, &sub) {
        let entries = git::tree(&path, &rev, &sub).unwrap_or_default();
        let items: String = entries
            .iter()
            .map(|e| {
                let p = if sub.is_empty() { e.name.clone() } else { format!("{sub}/{}", e.name) };
                format!(
                    r#"<a class="item" href="/r/{}?ref={}&path={}"><span class="dot n"></span><span>{}{}</span><span class="end">{}</span></a>"#,
                    enc(&repo),
                    enc(&rev),
                    enc(&p),
                    esc(&e.name),
                    if e.dir { "/" } else { "" },
                    ago(e.time)
                )
            })
            .collect();
        let readme = if sub.is_empty() {
            git::read_file(&path, &rev, "README.md")
                .ok()
                .map(|b| {
                    format!(
                        r#"<pre>{}</pre>"#,
                        esc(&String::from_utf8_lossy(&b[..b.len().min(20_000)]))
                    )
                })
                .unwrap_or_default()
        } else {
            String::new()
        };
        format!(r#"<div class="list">{items}</div>{readme}"#)
    } else {
        match git::read_file(&path, &rev, &sub) {
            Ok(b) if b.contains(&0) => r#"<div class="sub">Binary file.</div>"#.to_string(),
            Ok(b) if b.len() > 1 << 20 => format!(
                r#"<div class="sub">{} KB. Too big to show here.</div>"#,
                b.len() / 1024
            ),
            Ok(b) => format!("<pre>{}</pre>", esc(&String::from_utf8_lossy(&b))),
            Err(e) => format!(r#"<div class="sub">{}</div>"#, esc(&e)),
        }
    };
    let clone = format!("git clone {url}");
    page(
        &app,
        &repo,
        "Repos",
        &format!(
            r#"{}<div class="top"><h1>{r}</h1><a class="btn ghost" href="/changes/new?repo={}">New change</a><a class="btn ghost" href="/r/{}/send">Send to GitHub</a></div>
<div class="foot" style="gap:12px">{switcher}<span class="sub small">{}{since}</span></div>{crumbs}{content}
<div class="foot" style="gap:8px"><code class="sub">{}</code>{}</div>"#,
            flash(&q.0),
            enc(&repo),
            enc(&repo),
            esc(short(&branches.iter().find(|b| b.name == rev).map(|b| b.sha.clone()).unwrap_or_default())),
            esc(&clone),
            copy(&clone)
        ),
    )
    .into_response()
}

// ---------- Changes

#[derive(Clone)]
struct Status {
    dot: &'static str,
    text: String,
    waiting: bool,
    mergeable: bool,
    run: Option<u64>,
    findings: Vec<scan::Finding>,
}

fn status(app: &App, c: &crate::store::Change) -> Status {
    let path = git::repo_path(&app.home, &c.repo);
    let tips = git::tips(&path);
    let Some(tip) = tips.get(&c.from) else {
        return Status {
            dot: "n",
            text: "Branch is gone".into(),
            waiting: true,
            mergeable: false,
            run: None,
            findings: vec![],
        };
    };
    let findings = scan::scan_diff(&git::diff(&path, &c.into, &c.from).unwrap_or_default());
    let run = app
        .state
        .lock()
        .unwrap()
        .runs
        .iter()
        .rev()
        .find(|r| r.repo == c.repo && &r.sha == tip)
        .cloned();
    let has_spec = run.is_some() || runner::spec_for(app, &c.repo, tip).is_some();
    let ward = if findings.is_empty() {
        None
    } else {
        Some(format!(
            "Ward flagged {} thing{}",
            findings.len(),
            if findings.len() == 1 { "" } else { "s" }
        ))
    };
    match run.as_ref().map(|r| r.status) {
        Some(RunStatus::Failed) | Some(RunStatus::Denied) => Status {
            dot: "r",
            text: "Checks failed".into(),
            waiting: true,
            mergeable: false,
            run: run.map(|r| r.id),
            findings,
        },
        Some(RunStatus::Passed) | None if !(run.is_none() && has_spec) => match ward {
            Some(w) => Status {
                dot: "a",
                text: w,
                waiting: true,
                mergeable: true,
                run: run.map(|r| r.id),
                findings,
            },
            None => Status {
                dot: "g",
                text: "Ready to merge".into(),
                waiting: true,
                mergeable: true,
                run: run.map(|r| r.id),
                findings,
            },
        },
        Some(RunStatus::Waiting) => Status {
            dot: "a",
            text: "A step needs your approval".into(),
            waiting: true,
            mergeable: false,
            run: run.map(|r| r.id),
            findings,
        },
        _ => Status {
            dot: "n",
            text: "Checks running".into(),
            waiting: false,
            mergeable: false,
            run: run.map(|r| r.id),
            findings,
        },
    }
}

fn waiting_changes(app: &App) -> usize {
    let open: Vec<_> = app
        .state
        .lock()
        .unwrap()
        .changes
        .iter()
        .filter(|c| c.state == ChangeState::Open)
        .cloned()
        .collect();
    open.iter().filter(|c| status(app, c).waiting).count()
}

async fn changes(State(app): State<Arc<App>>, q: Q) -> Html<String> {
    let open: Vec<_> = app
        .state
        .lock()
        .unwrap()
        .changes
        .iter()
        .rev()
        .filter(|c| c.state == ChangeState::Open)
        .cloned()
        .collect();
    let rows: Vec<(bool, String)> = open
        .iter()
        .map(|c| {
            let s = status(&app, c);
            let color = match s.dot {
                "g" => "color:#1d6b30",
                "r" => "color:#b00012",
                "a" => "color:#8a4f00",
                _ => "",
            };
            let by = if c.opened_by == "you" { "opened by you".to_string() } else { format!("opened by {}", esc(&c.opened_by)) };
            (
                s.waiting,
                format!(
                    r#"<a class="item" href="/c/{}" style="min-height:76px"><span class="dot {}"></span><span class="two"><b>{}</b><span class="sub">{} · {by} · {}</span></span><span class="end" style="{color}">{}</span></a>"#,
                    c.id,
                    s.dot,
                    esc(&c.title),
                    esc(&c.repo),
                    ago(c.created),
                    esc(&s.text)
                ),
            )
        })
        .collect();
    let section = |label: &str, waiting: bool| {
        let items: Vec<&String> = rows
            .iter()
            .filter(|(w, _)| *w == waiting)
            .map(|(_, h)| h)
            .collect();
        if items.is_empty() {
            return String::new();
        }
        format!(
            r#"<div style="display:flex;flex-direction:column;gap:4px"><div class="sub small" style="font-weight:600;letter-spacing:.04em;text-transform:uppercase">{label} · {}</div><div class="list">{}</div></div>"#,
            items.len(),
            items.into_iter().cloned().collect::<String>()
        )
    };
    let body = if rows.is_empty() {
        r#"<div class="sub">Nothing open. Push a branch and open a change from its repo, or let Claude or Codex open one.</div>"#.to_string()
    } else {
        format!(
            "{}{}",
            section("Waiting for you", true),
            section("Still in progress", false)
        )
    };
    page(
        &app,
        "Changes",
        "Changes",
        &format!(
            r#"{}<div class="top"><h1>Changes</h1></div>{body}"#,
            flash(&q.0)
        ),
    )
}

async fn change_new(State(app): State<Arc<App>>, q: Q) -> Response {
    let repo = q.get("repo").cloned().unwrap_or_default();
    if !crate::repo_exists(&app.home, &repo) {
        return Redirect::to("/").into_response();
    }
    let path = git::repo_path(&app.home, &repo);
    let def = git::default_branch(&path);
    let bs = git::branches(&path);
    let opts = |sel: &str| {
        bs.iter()
            .map(|b| {
                format!(
                    r#"<option{}>{}</option>"#,
                    if b.name == sel { " selected" } else { "" },
                    esc(&b.name)
                )
            })
            .collect::<String>()
    };
    let from = bs
        .iter()
        .find(|b| b.name != def)
        .map(|b| b.name.clone())
        .unwrap_or_default();
    page(
        &app,
        "New change",
        "Changes",
        &format!(
            r#"{}<div class="top"><h1>New change</h1></div><form method="post" action="/changes" style="display:flex;flex-direction:column;gap:24px">{}<input type="hidden" name="repo" value="{}">
<div class="foot" style="gap:12px"><select class="field mono" name="from" aria-label="From branch">{}</select><span class="sub">into</span><select class="field mono" name="into" aria-label="Into branch">{}</select></div>
<label style="display:flex;flex-direction:column;gap:8px"><span class="sub">Title (optional)</span><input class="field" name="title" placeholder="Uses the latest commit message"></label><div class="foot"><button class="btn push">Open change</button></div></form>"#,
            flash(&q.0),
            hidden(&app),
            esc(&repo),
            opts(&from),
            opts(&def)
        ),
    )
    .into_response()
}

async fn change_create(State(app): State<Arc<App>>, f: F) -> Response {
    if !checked(&app, &f) {
        return bad();
    }
    let g = |k: &str| f.get(k).cloned().unwrap_or_default();
    match crate::open_change(&app, &g("repo"), &g("from"), &g("into"), &g("title"), "you") {
        Ok(id) => Redirect::to(&format!("/c/{id}")).into_response(),
        Err(e) => Redirect::to(&format!(
            "/changes/new?repo={}&msg={}",
            enc(&g("repo")),
            enc(&e)
        ))
        .into_response(),
    }
}

fn diff_html(d: &str) -> String {
    let lines: Vec<&str> = d.lines().collect();
    let shown = lines.len().min(3000);
    let mut out: String = lines[..shown]
        .iter()
        .map(|l| {
            let cls = if l.starts_with("+++")
                || l.starts_with("---")
                || l.starts_with("diff ")
                || l.starts_with("index ")
            {
                "hunk"
            } else if l.starts_with('+') {
                "add"
            } else if l.starts_with('-') {
                "del"
            } else if l.starts_with("@@") {
                "hunk"
            } else {
                ""
            };
            if cls.is_empty() {
                format!("<span>{}</span>\n", esc(l))
            } else {
                format!(r#"<span class="{cls}">{}</span>"#, esc(l))
            }
        })
        .collect();
    if lines.len() > shown {
        out += &format!("\n… {} more lines", lines.len() - shown);
    }
    out
}

async fn change(State(app): State<Arc<App>>, P(id): P<u64>, q: Q) -> Response {
    let Some(c) = app.state.lock().unwrap().change(id).cloned() else {
        return Redirect::to("/changes").into_response();
    };
    let path = git::repo_path(&app.home, &c.repo);
    let d = git::diff(&path, &c.into, &c.from).unwrap_or_default();
    let by = if c.opened_by == "you" {
        String::new()
    } else {
        format!(" · opened by {}", esc(&c.opened_by))
    };
    let head = format!(
        r#"{}<div class="top"><h1>{}</h1></div><div class="sub"><a href="/r/{}">{}</a> · <span class="mono">{}</span> into <span class="mono">{}</span>{by}</div>"#,
        flash(&q.0),
        esc(&c.title),
        enc(&c.repo),
        esc(&c.repo),
        esc(&c.from),
        esc(&c.into)
    );
    let diff = if d.trim().is_empty() {
        r#"<div class="sub">No differences.</div>"#.to_string()
    } else {
        format!("<pre>{}</pre>", diff_html(&d))
    };
    if c.state != ChangeState::Open {
        let done = match c.state {
            ChangeState::Merged => format!(
                "Merged as <span class=\"mono\">{}</span>.",
                esc(short(c.merged_sha.as_deref().unwrap_or("")))
            ),
            _ => "Closed.".into(),
        };
        return page(
            &app,
            &c.title,
            "Changes",
            &format!(r#"{head}<div class="sub">{done}</div>"#),
        )
        .into_response();
    }
    let s = status(&app, &c);
    let line = match (s.dot, s.run) {
        ("g", _) if s.run.is_some() => {
            "Checks passed. Ward reviewed it and found nothing.".to_string()
        }
        ("g", _) => "No checks set up. Ward reviewed it and found nothing.".to_string(),
        ("r", Some(r)) => format!(r#"Checks failed. <a href="/run/{r}">See why</a>"#),
        ("a", _) if !s.findings.is_empty() => {
            let f: String = s
                .findings
                .iter()
                .take(3)
                .map(|f| {
                    format!(
                        "<br><span class=\"mono small\">{}: {}</span>",
                        f.kind,
                        esc(&f.line)
                    )
                })
                .collect();
            format!("{}. Check before merging.{f}", esc(&s.text))
        }
        (_, Some(r)) => format!(r#"{}. <a href="/run/{r}">Open run</a>"#, esc(&s.text)),
        _ => esc(&s.text),
    };
    let coauthor = app
        .state
        .lock()
        .unwrap()
        .repos
        .get(&c.repo)
        .map(|m| m.coauthor)
        .unwrap_or(true);
    let body = format!(
        r#"{head}{diff}<div class="foot"><span><span class="dot {}" style="margin-right:8px"></span>{line}</span></div>
<form method="post" action="/c/{id}/merge" class="foot" style="padding-top:20px;border-top:1px solid rgba(0,0,0,.07)">{}
<label class="switch"><input type="checkbox" name="coauthor" role="switch" aria-label="Join RigSpark’s mission by crediting RigSpark as co-author"{}><span></span></label>
<span style="display:flex;flex-direction:column;gap:2px;max-width:520px"><span>Join RigSpark’s mission: help people write great code without the risk of training AI. Your code and ideas belong to you.</span><span class="mono sub small">Adds to the commit: Co-authored-by: RigSpark &lt;noreply@rigspark.si&gt;</span></span>
<button class="btn push"{}>Merge</button></form>
<form method="post" action="/c/{id}/close">{}<button class="btn ghost" style="padding:0">Close without merging</button></form>"#,
        s.dot,
        hidden(&app),
        if coauthor { " checked" } else { "" },
        if s.mergeable { "" } else { " disabled" },
        hidden(&app)
    );
    page(&app, &c.title, "Changes", &body).into_response()
}

pub const COAUTHOR: &str = "Co-authored-by: RigSpark <noreply@rigspark.si>";

async fn merge(State(app): State<Arc<App>>, P(id): P<u64>, f: F) -> Response {
    if !checked(&app, &f) {
        return bad();
    }
    let Some(c) = app.state.lock().unwrap().change(id).cloned() else {
        return Redirect::to("/changes").into_response();
    };
    if c.state != ChangeState::Open {
        return Redirect::to(&format!("/c/{id}")).into_response();
    }
    if !status(&app, &c).mergeable {
        return Redirect::to(&format!("/c/{id}?msg={}", enc("Not ready to merge yet.")))
            .into_response();
    }
    let coauthor = f.get("coauthor").is_some();
    app.state.lock().unwrap().meta(&c.repo).coauthor = coauthor;
    app.save();
    let mut msg = format!("{} (#{id})\n\nChange #{id} from {}.", c.title, c.from);
    if coauthor {
        msg += &format!("\n\n{COAUTHOR}");
    }
    let path = git::repo_path(&app.home, &c.repo);
    let (into, from) = (c.into.clone(), c.from.clone());
    let res = tokio::task::spawn_blocking(move || git::merge_squash(&path, &into, &from, &msg))
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
    match res {
        Ok(sha) => {
            {
                let mut st = app.state.lock().unwrap();
                if let Some(ch) = st.changes.iter_mut().find(|x| x.id == id) {
                    ch.state = ChangeState::Merged;
                    ch.merged_sha = Some(sha.clone());
                }
            }
            app.save();
            runner::enqueue(&app, &c.repo, &c.into, &sha, None, 0);
            Redirect::to(&format!(
                "/changes?msg={}",
                enc(&format!("Merged “{}” into {}.", c.title, c.into))
            ))
            .into_response()
        }
        Err(e) => Redirect::to(&format!("/c/{id}?msg={}", enc(&e))).into_response(),
    }
}

async fn close(State(app): State<Arc<App>>, P(id): P<u64>, f: F) -> Response {
    if !checked(&app, &f) {
        return bad();
    }
    if let Some(c) = app
        .state
        .lock()
        .unwrap()
        .changes
        .iter_mut()
        .find(|c| c.id == id && c.state == ChangeState::Open)
    {
        c.state = ChangeState::Closed;
    }
    app.save();
    Redirect::to("/changes").into_response()
}

// ---------- Runs

async fn runs(State(app): State<Arc<App>>, q: Q) -> Html<String> {
    let (list, saved, total, lost, plan, waiting) = {
        let st = app.state.lock().unwrap();
        let (saved, total) = money::saved(&st.runs, &st.settings.plan, now());
        let lost: f64 = st.repos.values().filter_map(|m| m.lost_usd).sum();
        let waiting: Vec<Run> = st
            .runs
            .iter()
            .filter(|r| r.status == RunStatus::Waiting)
            .cloned()
            .collect();
        let list: Vec<Run> = st.runs.iter().rev().take(50).cloned().collect();
        (list, saved, total, lost, st.settings.plan.clone(), waiting)
    };
    let report = diag::report(&app.state.lock().unwrap().runs, now(), &app.home);
    let diag_link = if report.attention.is_empty() {
        "Diagnostics".to_string()
    } else {
        format!("Diagnostics · {}", report.attention.len())
    };
    let lost_line = if lost > 0.0 {
        format!(
            r#" <span class="sub">You paid GitHub {} before Upstream. Now $0.</span>"#,
            money::usd(lost)
        )
    } else {
        String::new()
    };
    let total_line = if total > saved {
        format!(
            r#" <span class="sub">{} since you started.</span>"#,
            money::usd(total)
        )
    } else {
        String::new()
    };
    let other = if plan == "pro" { "free" } else { "pro" };
    let minutes: u64 = {
        let st = app.state.lock().unwrap();
        let month = money::month_name(now());
        st.runs
            .iter()
            .filter(|r| {
                r.finished
                    .map(|t| money::month_name(t) == month && now() - t < 32 * 86_400)
                    .unwrap_or(false)
            })
            .filter_map(|r| r.duration_ms())
            .sum::<u64>()
            .div_ceil(60_000)
    };
    let headline = if saved > 0.0 {
        format!(
            r#"<span class="big save">{}</span><span>saved in {}.{total_line}{lost_line}</span>"#,
            money::usd(saved),
            money::month_name(now())
        )
    } else {
        format!(
            r#"<span class="big save">No bill</span><span>{minutes} minute{} ran here in {}. GitHub’s free minutes would have covered {} too, so savings start once you go past them.{total_line}{lost_line}</span>"#,
            if minutes == 1 { "" } else { "s" },
            money::month_name(now()),
            if minutes == 1 { "it" } else { "them" }
        )
    };
    let money = format!(
        r#"<div style="display:flex;flex-direction:column;gap:12px">{headline}
<form method="post" action="/plan" class="sub small">{}<input type="hidden" name="plan" value="{other}">Priced at GitHub {} plan rates after free minutes. <button class="btn ghost" style="min-height:0;padding:0;font-size:13px">Use {} instead</button></form></div>"#,
        hidden(&app),
        if plan == "pro" { "Pro" } else { "Free" },
        if plan == "pro" { "Free" } else { "Pro" }
    );
    let asks: String = waiting
        .iter()
        .map(|r| {
            let step = r.steps.iter().find(|s| s.status == StepStatus::Pending).map(|s| s.name.clone()).unwrap_or_default();
            format!(
                r#"<form method="post" action="/run/{}/approve" class="ask">{}<span>Run #{} on <b>{}</b> wants to run <b>{}</b>.</span><button class="btn ghost push" name="ok" value="no">Deny</button><button class="btn" name="ok" value="yes">Approve</button></form>"#,
                r.id,
                hidden(&app),
                r.id,
                esc(&r.repo),
                esc(&step)
            )
        })
        .collect();
    let items: String = list
        .iter()
        .map(|r| {
            let end = match r.status {
                RunStatus::Failed => format!("failed · {}", r.duration_ms().map(diag::secs).unwrap_or_default()),
                RunStatus::Passed => r.duration_ms().map(diag::secs).unwrap_or_default(),
                RunStatus::Running => "running".into(),
                RunStatus::Queued => "queued".into(),
                RunStatus::Waiting => "waiting for you".into(),
                RunStatus::Denied => "denied".into(),
            };
            format!(
                r#"<a class="item" href="/run/{}"><span class="dot {}"></span><span class="two"><b>{}</b><span class="sub">{} · {}</span></span><span class="end">{end}</span></a>"#,
                r.id,
                run_dot(r.status),
                esc(if r.title.is_empty() { short(&r.sha) } else { &r.title }),
                esc(&r.repo),
                esc(&r.branch)
            )
        })
        .collect();
    let list = if list.is_empty() {
        format!(
            r#"<div class="sub">No runs yet. Add <span class="mono">{}</span> to a repo, or move one in with GitHub Actions.</div>"#,
            runner::RUN_FILE
        )
    } else {
        format!(r#"<div class="list">{items}</div>"#)
    };
    page(
        &app,
        "Runs",
        "Runs",
        &format!(
            r#"{}<div class="top"><span style="flex:1"></span><a class="btn ghost" href="/diagnostics">{diag_link}</a></div>{money}{asks}{list}"#,
            flash(&q.0)
        ),
    )
}

fn is_error(l: &str) -> bool {
    let l = l.to_ascii_lowercase();
    l.contains("error")
        || l.contains("failed")
        || l.contains("panicked")
        || l.contains("fatal")
        || l.contains("not found")
}

async fn run(State(app): State<Arc<App>>, P(id): P<u64>, q: Q) -> Response {
    let Some(r) = app.state.lock().unwrap().run(id).cloned() else {
        return Redirect::to("/runs").into_response();
    };
    let view = q.get("view").map(String::as_str).unwrap_or("summary");
    let lines = runner::read_log(&app.home, id);
    let failed = r.steps.iter().position(|s| s.status == StepStatus::Failed);
    let title = match r.status {
        RunStatus::Passed => "Checks passed",
        RunStatus::Failed => "Checks failed",
        RunStatus::Running | RunStatus::Queued => "Running",
        RunStatus::Waiting => "Waiting for you",
        RunStatus::Denied => "Stopped",
    };
    let refresh = if r.status.finished() {
        ""
    } else {
        r#"<meta http-equiv="refresh" content="3">"#
    };
    let seg = |v: &str, label: &str| {
        format!(
            r#"<a href="/run/{id}?view={v}"{}>{label}</a>"#,
            if v == view {
                r#" class="on" aria-current="page""#
            } else {
                ""
            }
        )
    };
    let head = format!(
        r#"{refresh}{}<div class="top"><h1>{title}</h1></div><div class="sub">{} · {} · <span class="mono">{}</span> · run #{id}{}</div><nav class="seg" aria-label="Detail">{}{}{}</nav>"#,
        flash(&q.0),
        esc(&r.title),
        esc(&r.repo),
        esc(short(&r.sha)),
        r.duration_ms()
            .map(|d| format!(" · {}", diag::secs(d)))
            .unwrap_or_default(),
        seg("summary", "Summary"),
        seg("trace", "Trace"),
        seg("log", "Full log")
    );
    let rerun = |from_failed: bool, label: &str, primary: bool| {
        format!(
            r#"<form method="post" action="/run/{id}/rerun" class="inline">{}{}<button class="btn{}">{label}</button></form>"#,
            hidden(&app),
            if from_failed {
                r#"<input type="hidden" name="failed" value="1">"#
            } else {
                ""
            },
            if primary { "" } else { " ghost" }
        )
    };
    let body = match view {
        "trace" => {
            let total = r
                .steps
                .iter()
                .map(|s| s.start_ms + s.dur_ms)
                .max()
                .unwrap_or(1)
                .max(1) as f64;
            let rows: String = r
                .steps
                .iter()
                .map(|s| {
                    let (dot, color) = match s.status {
                        StepStatus::Passed => ("g", "#248a3d"),
                        StepStatus::Failed => ("r", "#d70015"),
                        StepStatus::Running => ("a", "#ffad00"),
                        _ => ("n", "#c7c7cc"),
                    };
                    let bar = if s.dur_ms > 0 || s.status == StepStatus::Running {
                        format!(r#"<div class="bar" style="margin-left:{:.1}%;width:{:.1}%;background:{color}"></div>"#, s.start_ms as f64 / total * 100.0, (s.dur_ms as f64 / total * 100.0).max(0.8))
                    } else {
                        r#"<div class="sub small">—</div>"#.into()
                    };
                    let note = match s.status {
                        StepStatus::Skipped => "skipped".to_string(),
                        StepStatus::Pending => "not started".into(),
                        _ => format!("{} {}", diag::secs(s.dur_ms), s.note),
                    };
                    format!(r#"<div class="wf"><span style="display:flex;gap:10px;align-items:center"><span class="dot {dot}"></span>{}</span><div>{bar}</div><span class="sub small" style="text-align:right">{}</span></div>"#, esc(&s.name), esc(&note))
                })
                .collect();
            let os = match r.runs_on.as_str() {
                "mac" => "macOS",
                "windows" => "Windows",
                _ => "Linux",
            };
            format!(
                r#"<div>{rows}</div><div class="sub small" style="display:flex;flex-wrap:wrap;gap:6px 24px"><span>Ran on this Mac · priced as GitHub {os}</span><span>Branch <span class="mono">{}</span></span><span>Commit <span class="mono">{}</span></span><span>Waited {} to start</span></div>"#,
                esc(&r.branch),
                esc(short(&r.sha)),
                r.wait_ms().map(diag::secs).unwrap_or_else(|| "—".into())
            )
        }
        "log" => {
            let needle = q
                .get("q")
                .map(|s| s.to_lowercase())
                .filter(|s| !s.is_empty());
            let groups: String = r
                .steps
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let mine: Vec<&runner::LogLine> = lines.iter().filter(|l| l.s == i && needle.as_ref().map(|n| l.l.to_lowercase().contains(n)).unwrap_or(true)).collect();
                    let body: String = mine
                        .iter()
                        .map(|l| {
                            let t = format!("{:02}:{:05.2}", l.t / 60_000, (l.t % 60_000) as f64 / 1000.0);
                            if is_error(&l.l) {
                                format!(r#"<span class="del">{t}  {}</span>"#, esc(&l.l))
                            } else {
                                format!("<span>{t}  {}</span>\n", esc(&l.l))
                            }
                        })
                        .collect();
                    let open = if s.status == StepStatus::Failed || needle.is_some() { " open" } else { "" };
                    format!(
                        r#"<details{open}><summary>{} · {} · {} line{}</summary><pre style="margin-top:8px">{}</pre></details>"#,
                        esc(&s.name),
                        match s.status {
                            StepStatus::Skipped => "skipped".into(),
                            _ => diag::secs(s.dur_ms),
                        },
                        mine.len(),
                        if mine.len() == 1 { "" } else { "s" },
                        if body.is_empty() { "<span class=\"sub\">No output.</span>".into() } else { body }
                    )
                })
                .collect();
            format!(
                r#"<form method="get" action="/run/{id}"><input type="hidden" name="view" value="log"><input class="field" name="q" value="{}" placeholder="Search the log" aria-label="Search the log"></form><div style="display:flex;flex-direction:column;gap:12px">{groups}</div><div class="sub small">Logs are kept 30 days on this Mac.</div>"#,
                esc(q.get("q").map(String::as_str).unwrap_or(""))
            )
        }
        _ => match (r.status, failed) {
            (RunStatus::Failed, Some(i)) => {
                let step = &r.steps[i];
                let what = match &r.failed_test {
                    Some(t) => format!(r#"{} <span class="mono" style="font-size:16px">{}</span> failed"#, esc(&step.name), esc(t)),
                    None => format!("{} failed <span class=\"sub\" style=\"font-size:15px;font-weight:400\">({})</span>", esc(&step.name), esc(&step.note)),
                };
                let cause = r
                    .cause
                    .as_ref()
                    .map(|c| format!(r#"<span class="sub">Likely cause: {}</span>"#, esc(c)))
                    .unwrap_or_default();
                let tail: String = runner::tail(&lines, i, 12)
                    .iter()
                    .map(|l| {
                        if is_error(l) {
                            format!(r#"<span class="del">{}</span>"#, esc(l))
                        } else {
                            format!("<span>{}</span>\n", esc(l))
                        }
                    })
                    .collect();
                format!(
                    r#"<div style="display:flex;flex-direction:column;gap:6px"><span style="font-size:20px;font-weight:600">{what}</span>{cause}</div><pre>{}</pre><div class="foot"><span class="push"></span>{}{}</div>"#,
                    if tail.is_empty() {
                        "No output.".into()
                    } else {
                        tail
                    },
                    rerun(true, "Re-run failed step", false),
                    rerun(false, "Re-run", true)
                )
            }
            (RunStatus::Failed, None) => format!(
                r#"<div style="font-size:20px;font-weight:600">{}</div><div class="foot"><span class="push"></span>{}</div>"#,
                esc(r.cause.as_deref().unwrap_or("The run couldn’t start.")),
                rerun(false, "Re-run", true)
            ),
            (RunStatus::Waiting, _) => format!(
                r#"<form method="post" action="/run/{id}/approve" class="ask">{}<span>The next step needs your approval.</span><button class="btn ghost push" name="ok" value="no">Deny</button><button class="btn" name="ok" value="yes">Approve</button></form>"#,
                hidden(&app)
            ),
            (RunStatus::Passed, _) => format!(
                r#"<div class="sub">All {} steps passed.</div><div class="foot"><span class="push"></span>{}</div>"#,
                r.steps.len(),
                rerun(false, "Re-run", false)
            ),
            _ => {
                let cur = r
                    .steps
                    .iter()
                    .find(|s| s.status == StepStatus::Running)
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| "Starting".into());
                let tail: String = lines
                    .iter()
                    .rev()
                    .take(8)
                    .rev()
                    .map(|l| format!("<span>{}</span>\n", esc(&l.l)))
                    .collect();
                format!(
                    r#"<div class="sub">{}…</div><pre>{}</pre>"#,
                    esc(&cur),
                    if tail.is_empty() {
                        "Waiting for output.".into()
                    } else {
                        tail
                    }
                )
            }
        },
    };
    page(
        &app,
        &format!("Run #{id}"),
        "Runs",
        &format!("{head}{body}"),
    )
    .into_response()
}

async fn rerun(State(app): State<Arc<App>>, P(id): P<u64>, f: F) -> Response {
    if !checked(&app, &f) {
        return bad();
    }
    let Some(r) = app.state.lock().unwrap().run(id).cloned() else {
        return Redirect::to("/runs").into_response();
    };
    let from = if f.contains_key("failed") {
        r.steps
            .iter()
            .position(|s| s.status == StepStatus::Failed)
            .unwrap_or(0)
    } else {
        0
    };
    match runner::enqueue(&app, &r.repo, &r.branch, &r.sha, Some(&r), from) {
        Some(n) => Redirect::to(&format!("/run/{n}")).into_response(),
        None => Redirect::to(&format!(
            "/run/{id}?msg={}",
            enc("This commit has no steps to run.")
        ))
        .into_response(),
    }
}

async fn approve(State(app): State<Arc<App>>, P(id): P<u64>, f: F) -> Response {
    if !checked(&app, &f) {
        return bad();
    }
    let yes = f.get("ok").map(|v| v == "yes").unwrap_or(false);
    if let Some(tx) = app.approvals.lock().unwrap().remove(&id) {
        let _ = tx.send(yes);
    }
    Redirect::to(&format!("/run/{id}")).into_response()
}

async fn diagnostics(State(app): State<Arc<App>>) -> Html<String> {
    let rep = diag::report(&app.state.lock().unwrap().runs, now(), &app.home);
    let head = if rep.attention.is_empty() {
        "All normal.".to_string()
    } else {
        format!(
            "{} thing{} need{} attention.",
            rep.attention.len(),
            if rep.attention.len() == 1 { "" } else { "s" },
            if rep.attention.len() == 1 { "s" } else { "" }
        )
    };
    let items: String = rep
        .attention
        .iter()
        .map(|a| {
            let act = a.run.map(|r| format!(r#"<a class="btn ghost" href="/run/{r}">See run</a>"#)).unwrap_or_default();
            format!(r#"<div class="item" style="min-height:76px"><span class="dot a"></span><span class="two"><b>{}</b><span class="sub">{}</span></span><span class="end">{act}</span></div>"#, esc(&a.title), esc(&a.detail))
        })
        .collect();
    let stat = |v: Option<String>, label: &str, warn: bool| {
        format!(
            r#"<div class="stat"><b{}>{}</b><span class="sub small">{label}</span></div>"#,
            if warn {
                r#" style="color:#8a4f00""#
            } else {
                ""
            },
            v.unwrap_or_else(|| "—".into())
        )
    };
    let slower =
        matches!((rep.median_run, rep.median_before), (Some(a), Some(b)) if a > b + 20_000);
    let stats = format!(
        r#"<div class="stats">{}{}{}{}</div>"#,
        stat(
            rep.pass_rate.map(|p| format!("{p}%")),
            "runs passed, 7 days",
            rep.pass_rate.map(|p| p < 80).unwrap_or(false)
        ),
        stat(
            rep.median_run.map(diag::secs),
            &rep.median_before
                .map(|b| format!("median run, was {}", diag::secs(b)))
                .unwrap_or_else(|| "median run".into()),
            slower
        ),
        stat(
            rep.median_wait.map(diag::secs),
            "median wait to start",
            false
        ),
        stat(
            rep.disk_used.map(|d| format!("{d}%")),
            "disk used on this Mac",
            rep.disk_used.map(|d| d >= 90).unwrap_or(false)
        ),
    );
    let trend = if rep.trend.len() >= 2 {
        let (mx, mn) = (
            *rep.trend.iter().max().unwrap() as f64,
            *rep.trend.iter().min().unwrap() as f64,
        );
        let span = (mx - mn).max(1.0);
        let step = 520.0 / (rep.trend.len() - 1) as f64;
        let pts: String = rep
            .trend
            .iter()
            .enumerate()
            .map(|(i, v)| {
                format!(
                    "{:.0},{:.0} ",
                    i as f64 * step,
                    58.0 - (*v as f64 - mn) / span * 50.0
                )
            })
            .collect();
        format!(
            r##"<div style="display:flex;flex-direction:column;gap:8px"><span class="sub small">Run time, last {} days with runs</span><svg viewBox="0 0 520 64" width="100%" height="64" role="img" aria-label="Daily median run time from {} to {}"><polyline points="{pts}" fill="none" stroke="#ffad00" stroke-width="2.5" stroke-linejoin="round" stroke-linecap="round"/></svg></div>"##,
            rep.trend.len(),
            diag::secs(rep.trend[0] * 1000),
            diag::secs(rep.trend[rep.trend.len() - 1] * 1000)
        )
    } else {
        String::new()
    };
    let logs: u64 = std::fs::read_dir(app.home.join("logs"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum();
    let normal = format!(
        r#"<div class="sub small">Everything else is normal. Logs and traces stay on this Mac for 30 days ({:.1} MB).</div>"#,
        logs as f64 / 1e6
    );
    let list = if items.is_empty() {
        String::new()
    } else {
        format!(r#"<div class="list">{items}</div>"#)
    };
    page(
        &app,
        "Diagnostics",
        "Runs",
        &format!(
            r#"<div class="top"><h1>Diagnostics</h1></div><div style="font-size:20px;font-weight:600">{head}</div>{list}{stats}{trend}{normal}"#
        ),
    )
}

async fn plan(State(app): State<Arc<App>>, f: F) -> Response {
    if !checked(&app, &f) {
        return bad();
    }
    let p = if f.get("plan").map(String::as_str) == Some("pro") {
        "pro"
    } else {
        "free"
    };
    app.state.lock().unwrap().settings.plan = p.into();
    app.save();
    Redirect::to("/runs").into_response()
}

// ---------- Move in

async fn move_in_page(State(app): State<Arc<App>>, q: Q) -> Html<String> {
    let jobs = app.jobs.lock().unwrap().clone();
    if !jobs.is_empty() && jobs.iter().any(|j| !j.done) {
        let rows: String = jobs
            .iter()
            .map(|j| {
                let dot = if j.error.is_some() { "r" } else if j.done { "g" } else { "a" };
                format!(r#"<div class="item"><span class="dot {dot}"></span><b>{}</b><span class="end">{}</span></div>"#, esc(&j.repo), esc(j.error.as_deref().unwrap_or(&j.status)))
            })
            .collect();
        return page(
            &app,
            "Moving in",
            "Repos",
            &format!(
                r#"<meta http-equiv="refresh" content="2"><div class="top"><h1>Moving in</h1></div><div class="list">{rows}</div><div class="sub">Nothing is deleted on GitHub.</div>"#
            ),
        );
    }
    let done = if jobs.is_empty() {
        String::new()
    } else {
        let lost: f64 = {
            let st = app.state.lock().unwrap();
            jobs.iter()
                .filter_map(|j| st.repos.get(&j.repo).and_then(|m| m.lost_usd))
                .sum()
        };
        let needs: Vec<String> = {
            let st = app.state.lock().unwrap();
            jobs.iter()
                .flat_map(|j| {
                    st.repos
                        .get(&j.repo)
                        .map(|m| {
                            m.needs_you
                                .iter()
                                .map(|n| format!("{}: {n}", j.repo))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                })
                .collect()
        };
        let errs: String = jobs
            .iter()
            .filter_map(|j| {
                j.error
                    .as_ref()
                    .map(|e| format!("<br>{}: {}", esc(&j.repo), esc(e)))
            })
            .collect();
        let moved = jobs.iter().filter(|j| j.error.is_none()).count();
        app.jobs.lock().unwrap().clear();
        let lost = if lost > 0.0 {
            format!(
                r#"<div class="warn">{WARN}<span>You lost <b>{}</b> to GitHub Actions on these repos. That stops today.</span></div>"#,
                money::usd(lost)
            )
        } else {
            String::new()
        };
        let needs = if needs.is_empty() {
            String::new()
        } else {
            format!(
                r#"<div class="sub">{} workflow step{} couldn’t be converted and need you: <span class="mono small">{}</span>. Add them to <span class="mono">{}</span>.</div>"#,
                needs.len(),
                if needs.len() == 1 { "" } else { "s" },
                esc(&needs.join("; ")),
                runner::RUN_FILE
            )
        };
        format!(
            r#"<div class="flash">Moved in {moved} repo{}.{errs}</div>{lost}{needs}"#,
            if moved == 1 { "" } else { "s" }
        )
    };
    if let Some((_, _, list)) = q.get("s").and_then(|s| app.session(s)) {
        let have = git::list_repos(&app.home);
        let rows: String = list
            .iter()
            .filter(|r| !have.contains(&r.name))
            .map(|r| {
                let key = format!("r_{}/{}", r.owner, r.name);
                let note = if r.private { "" } else { r#"<span class="sub" style="margin-left:auto">Public: CI is free on GitHub</span>"# };
                format!(r#"<label class="check"><input type="checkbox" name="{}"{}><b>{}</b>{note}</label>"#, esc(&key), if r.private { " checked" } else { "" }, esc(&r.name))
            })
            .collect();
        return page(
            &app,
            "Move in",
            "Repos",
            &format!(
                r#"<div class="top"><h1>Move in</h1></div><form method="post" action="/move-in/start" style="display:flex;flex-direction:column;gap:28px">{}<input type="hidden" name="s" value="{}"><div class="list">{}</div>
<div class="sub">Everything comes along except secrets, which you re-enter once. Open pull requests become Changes. Nothing is deleted on GitHub.</div><div class="foot"><button class="btn push">Move in</button></div></form>"#,
                hidden(&app),
                esc(q.get("s").map(String::as_str).unwrap_or("")),
                if rows.is_empty() {
                    r#"<div class="sub">Every repo is already here.</div>"#.to_string()
                } else {
                    rows
                }
            ),
        );
    }
    page(
        &app,
        "Move in",
        "Repos",
        &format!(
            r#"{}{done}<div class="top"><h1>Move in</h1></div><div class="seg" aria-label="From"><a class="on" aria-current="true">GitHub</a><span class="off" style="display:flex;align-items:center;padding:0 16px">GitLab · soon</span><span class="off" style="display:flex;align-items:center;padding:0 16px">Bitbucket · soon</span><span class="off" style="display:flex;align-items:center;padding:0 16px">Azure · soon</span></div>
<form method="post" action="/move-in/token" style="display:flex;flex-direction:column;gap:24px">{}<label style="display:flex;flex-direction:column;gap:8px"><span class="sub">GitHub access token, used for this move only and never stored</span><input class="field" type="password" name="token" required autocomplete="off"></label>
<div class="sub small">Give it read access to your repos and Actions. Add billing read access to see what Actions cost you.</div>{}<div class="foot"><button class="btn push">Continue</button></div></form>"#,
            flash(&q.0),
            hidden(&app),
            copilot_line(&app, "/move-in")
        ),
    )
}

async fn move_in_token(State(app): State<Arc<App>>, f: F) -> Response {
    if !checked(&app, &f) {
        return bad();
    }
    let token = f
        .get("token")
        .map(|t| t.trim().to_string())
        .unwrap_or_default();
    let t2 = token.clone();
    let res = tokio::task::spawn_blocking(move || {
        Ok::<_, String>((github::login(&t2)?, github::repos(&t2)?))
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()));
    match res {
        Ok((login, repos)) => {
            let sid = crate::random_hex(12);
            app.sessions.lock().unwrap().insert(
                sid.clone(),
                crate::Session {
                    token,
                    login,
                    created: std::time::Instant::now(),
                    repos,
                },
            );
            Redirect::to(&format!("/move-in?s={sid}")).into_response()
        }
        Err(e) => Redirect::to(&format!("/move-in?msg={}", enc(&e))).into_response(),
    }
}

async fn move_in_start(State(app): State<Arc<App>>, f: F) -> Response {
    if !checked(&app, &f) {
        return bad();
    }
    let sid = f.get("s").cloned().unwrap_or_default();
    let Some((token, login, list)) = app.session(&sid) else {
        return Redirect::to("/move-in?msg=That+took+too+long.+Paste+the+token+again.")
            .into_response();
    };
    let chosen: Vec<github::RemoteRepo> = list
        .into_iter()
        .filter(|r| f.contains_key(&format!("r_{}/{}", r.owner, r.name)))
        .collect();
    if chosen.is_empty() {
        return Redirect::to(&format!("/move-in?s={sid}")).into_response();
    }
    app.sessions.lock().unwrap().remove(&sid);
    *app.jobs.lock().unwrap() = chosen
        .iter()
        .map(|r| crate::Job {
            repo: r.name.clone(),
            status: "Waiting".into(),
            done: false,
            error: None,
        })
        .collect();
    let app2 = app.clone();
    tokio::task::spawn_blocking(move || {
        for r in &chosen {
            crate::move_in(&app2, &token, &login, r);
        }
        // The token goes out of scope here: it was only ever in memory.
    });
    Redirect::to("/move-in").into_response()
}

// ---------- Send to GitHub

async fn send_page(State(app): State<Arc<App>>, P(repo): P<String>, q: Q) -> Response {
    if !crate::repo_exists(&app.home, &repo) {
        return Redirect::to("/").into_response();
    }
    let link = app
        .state
        .lock()
        .unwrap()
        .repos
        .get(&repo)
        .and_then(|m| m.github.clone());
    let r = esc(&repo);
    let session = q
        .get("s")
        .and_then(|s| app.session(s).map(|v| (s.clone(), v)));
    let Some((sid, (token, _, targets))) = session else {
        let target = match &link {
            Some(l) => format!(r#"<div class="sub">To <span class="mono">github.com/{}/{}</span></div>"#, esc(&l.owner), esc(&l.name)),
            None => r#"<label style="display:flex;flex-direction:column;gap:8px"><span class="sub">GitHub repo to send to (owner/name, already created on GitHub)</span><input class="field mono" name="target" required pattern="[A-Za-z0-9_.\-]+/[A-Za-z0-9_.\-]+"></label>"#.into(),
        };
        return page(
            &app,
            "Send to GitHub",
            "Repos",
            &format!(
                r#"{}<div class="top"><h1>Send to GitHub</h1></div><div class="sub"><a href="/r/{}">{r}</a></div><form method="post" action="/r/{}/send/token" style="display:flex;flex-direction:column;gap:24px">{}{target}<label style="display:flex;flex-direction:column;gap:8px"><span class="sub">GitHub access token, used for this send only and never stored</span><input class="field" type="password" name="token" required autocomplete="off"></label><div class="foot"><button class="btn push">Continue</button></div></form>"#,
                flash(&q.0),
                enc(&repo),
                enc(&repo),
                hidden(&app)
            ),
        )
        .into_response();
    };
    let Some(t) = targets.first().cloned() else {
        return Redirect::to(&format!("/r/{}/send", enc(&repo))).into_response();
    };
    let (app2, repo2, tok) = (app.clone(), repo.clone(), token.clone());
    let (o, n) = (t.owner.clone(), t.name.clone());
    let sync = tokio::task::spawn_blocking(move || crate::branch_sync(&app2, &repo2, &tok, &o, &n))
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
    let sync = match sync {
        Ok(s) => s,
        Err(e) => {
            return Redirect::to(&format!("/r/{}/send?msg={}", enc(&repo), enc(&e))).into_response()
        }
    };
    let rows: String = sync
        .iter()
        .map(|b| {
            let (check, end) = match b.state {
                SyncState::Ready => (true, format!("{} new", b.new)),
                SyncState::NewBranch => (true, format!("new branch · {} commit{}", b.new, if b.new == 1 { "" } else { "s" })),
                SyncState::UpToDate => (false, "up to date".into()),
                SyncState::GithubChanged => (false, r#"<span style="color:#8a4f00">GitHub changed too</span>"#.into()),
            };
            let disabled = matches!(b.state, SyncState::UpToDate | SyncState::GithubChanged);
            format!(
                r#"<label class="check"><input type="checkbox" name="b_{}"{}{}><span class="mono">{}</span><span class="sub" style="margin-left:auto">{end}</span></label>"#,
                esc(&b.name),
                if check { " checked" } else { "" },
                if disabled { " disabled" } else { "" },
                esc(&b.name)
            )
        })
        .collect();
    let changed = sync
        .iter()
        .filter(|b| b.state == SyncState::GithubChanged)
        .count();
    let changed_note = if changed > 0 {
        format!(
            r#"<div class="sub small">{changed} branch{} changed on GitHub too. Pull those changes into Upstream first so nothing is overwritten.</div>"#,
            if changed == 1 { "" } else { "es" }
        )
    } else {
        String::new()
    };
    let (estimate, has_runs, private) = {
        let st = app.state.lock().unwrap();
        let est = money::monthly_estimate(&st.runs, &repo, &st.settings.plan, now());
        let has = st
            .repos
            .get(&repo)
            .map(|m| {
                m.converted.is_some()
                    || m.github
                        .as_ref()
                        .map(|g| !g.workflow_ids.is_empty())
                        .unwrap_or(false)
            })
            .unwrap_or(false)
            || git::read_file(
                &git::repo_path(&app.home, &repo),
                &git::default_branch(&git::repo_path(&app.home, &repo)),
                ".github/workflows",
            )
            .is_ok();
        (est, has, t.private)
    };
    let vis = if private {
        r#"<div class="seg" role="radiogroup" aria-label="Visibility"><label><input type="radio" name="vis" value="private" checked>Private</label><label><input type="radio" name="vis" value="public">Show the world</label></div>"#
    } else {
        r#"<input type="hidden" name="vis" value="public"><div class="sub">This repo is public on GitHub.</div>"#
    };
    let runs = if has_runs && private {
        format!(
            r#"<div class="warn">{WARN}<span>Runs stay here, so GitHub’s workflows are switched off. Turned on, they’d cost about <b>{} a month</b> at today’s pace.</span></div><label class="check" style="border:0;min-height:0"><input type="checkbox" name="gh_runs"><span class="sub">Run workflows on GitHub too</span></label>"#,
            money::usd(estimate)
        )
    } else {
        String::new()
    };
    page(
        &app,
        "Send to GitHub",
        "Repos",
        &format!(
            r#"{}<div class="top"><h1>Send to GitHub</h1></div><div class="sub"><a href="/r/{}">{r}</a> → <span class="mono">github.com/{}/{}</span></div>
<form method="post" action="/r/{}/send" style="display:flex;flex-direction:column;gap:28px">{}<input type="hidden" name="s" value="{}">{vis}<div class="list">{rows}</div>{changed_note}
<div style="display:flex;flex-direction:column;gap:12px">{runs}{}</div><div class="foot"><button class="btn push">Send</button></div></form>"#,
            flash(&q.0),
            enc(&repo),
            esc(&t.owner),
            esc(&t.name),
            enc(&repo),
            hidden(&app),
            esc(&sid),
            copilot_line(&app, &format!("/r/{}/send?s={}", enc(&repo), enc(&sid)))
        ),
    )
    .into_response()
}

async fn send_token(State(app): State<Arc<App>>, P(repo): P<String>, f: F) -> Response {
    if !checked(&app, &f) {
        return bad();
    }
    let link = app
        .state
        .lock()
        .unwrap()
        .repos
        .get(&repo)
        .and_then(|m| m.github.clone());
    let token = f
        .get("token")
        .map(|t| t.trim().to_string())
        .unwrap_or_default();
    let target = match &link {
        Some(l) => Some((l.owner.clone(), l.name.clone())),
        None => f.get("target").and_then(|t| {
            t.trim()
                .split_once('/')
                .map(|(a, b)| (a.to_string(), b.to_string()))
        }),
    };
    let Some((owner, name)) = target else {
        return Redirect::to(&format!(
            "/r/{}/send?msg={}",
            enc(&repo),
            enc("Write the GitHub repo as owner/name.")
        ))
        .into_response();
    };
    let t2 = token.clone();
    let (o2, n2) = (owner.clone(), name.clone());
    let res = tokio::task::spawn_blocking(move || {
        let login = github::login(&t2)?;
        let private = github::repos(&t2)?
            .into_iter()
            .find(|r| r.owner.eq_ignore_ascii_case(&o2) && r.name.eq_ignore_ascii_case(&n2))
            .map(|r| r.private)
            .unwrap_or(true);
        Ok::<_, String>((login, private))
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()));
    match res {
        Ok((login, private)) => {
            let sid = crate::random_hex(12);
            app.sessions.lock().unwrap().insert(
                sid.clone(),
                crate::Session {
                    token,
                    login,
                    created: std::time::Instant::now(),
                    repos: vec![github::RemoteRepo {
                        owner,
                        name,
                        private,
                    }],
                },
            );
            Redirect::to(&format!("/r/{}/send?s={sid}", enc(&repo))).into_response()
        }
        Err(e) => Redirect::to(&format!("/r/{}/send?msg={}", enc(&repo), enc(&e))).into_response(),
    }
}

async fn send_post(State(app): State<Arc<App>>, P(repo): P<String>, f: F) -> Response {
    if !checked(&app, &f) {
        return bad();
    }
    let sid = f.get("s").cloned().unwrap_or_default();
    let Some((token, _, targets)) = app.session(&sid) else {
        return Redirect::to(&format!(
            "/r/{}/send?msg={}",
            enc(&repo),
            enc("That took too long. Paste the token again.")
        ))
        .into_response();
    };
    let Some(t) = targets.first().cloned() else {
        return Redirect::to(&format!("/r/{}/send", enc(&repo))).into_response();
    };
    let branches: Vec<String> = f
        .keys()
        .filter_map(|k| k.strip_prefix("b_").map(str::to_string))
        .collect();
    let public = f.get("vis").map(|v| v == "public").unwrap_or(false) && t.private;
    if public {
        let blockers = crate::public_blockers(&app, &repo);
        if !blockers.is_empty() {
            return Redirect::to(&format!(
                "/r/{}/send?s={}&msg={}",
                enc(&repo),
                enc(&sid),
                enc(&format!("Not public yet. {}", blockers.join(" ")))
            ))
            .into_response();
        }
    }
    let gh_runs = f.contains_key("gh_runs");
    let (app2, repo2) = (app.clone(), repo.clone());
    let res = tokio::task::spawn_blocking(move || {
        let sent = if branches.is_empty() {
            0
        } else {
            crate::send(&app2, &repo2, &token, &t.owner, &t.name, &branches)?
        };
        let ids = github::workflow_ids(&token, &t.owner, &t.name);
        if t.private && !public {
            github::set_workflows(&token, &t.owner, &t.name, &ids, gh_runs)?;
        }
        if public {
            github::make_public(&token, &t.owner, &t.name)?;
        }
        let mut st = app2.state.lock().unwrap();
        let m = st.meta(&repo2);
        let private = t.private && !public;
        m.github = Some(crate::store::GithubLink {
            owner: t.owner.clone(),
            name: t.name.clone(),
            private,
            workflow_ids: ids,
        });
        drop(st);
        app2.save();
        Ok::<_, String>((sent, public))
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()));
    app.sessions.lock().unwrap().remove(&sid);
    match res {
        Ok((n, public)) => {
            let msg = if public {
                format!(
                    "Sent {n} branch{} and showed it to the world.",
                    if n == 1 { "" } else { "es" }
                )
            } else {
                format!(
                    "Sent {n} branch{} to GitHub.",
                    if n == 1 { "" } else { "es" }
                )
            };
            Redirect::to(&format!("/r/{}?msg={}", enc(&repo), enc(&msg))).into_response()
        }
        Err(e) => Redirect::to(&format!("/r/{}/send?msg={}", enc(&repo), enc(&e))).into_response(),
    }
}

// ---------- Connect AI and settings

async fn connect(State(app): State<Arc<App>>, q: Q) -> Html<String> {
    let st = app.state.lock().unwrap().settings.assistants.clone();
    let url = format!("http://127.0.0.1:{}/mcp", app.port);
    let rows: String = mcp::ASSISTANTS
        .iter()
        .map(|name| {
            let a = st.get(*name).cloned().unwrap_or_default();
            let on = a.enabled && !a.key.is_empty();
            let state = match (on, a.last_used) {
                (true, Some(t)) => format!("Connected · last used {}", ago(t)),
                (true, None) => "On · not used yet".into(),
                _ => "Off".into(),
            };
            let cmd = if *name == "Claude" {
                format!("claude mcp add --transport http upstream {url} --header \"Authorization: Bearer {}\"", a.key)
            } else {
                format!("export UPSTREAM_TOKEN={}\ncodex mcp add upstream --url {url} --bearer-token-env-var UPSTREAM_TOKEN", a.key)
            };
            let detail = if on {
                format!(
                    r#"<div class="foot" style="gap:12px;align-items:flex-start"><pre style="flex:1 1 420px;padding:12px 16px">{}</pre>{}</div><form method="post" action="/connect/{name}">{}<button class="btn ghost" name="action" value="rotate" style="padding:0">New key</button></form>"#,
                    esc(&cmd),
                    copy(&cmd),
                    hidden(&app)
                )
            } else {
                String::new()
            };
            format!(
                r#"<div style="display:flex;flex-direction:column;gap:12px;padding:20px 0;border-bottom:1px solid rgba(0,0,0,.07)"><form method="post" action="/connect/{name}" class="foot" style="gap:14px">{}<span class="dot {}"></span><b style="font-size:17px">{name}</b><span class="sub">{state}</span><button class="btn{} push" name="action" value="{}">{}</button></form>{detail}</div>"#,
                hidden(&app),
                if on { "g" } else { "n" },
                if on { " ghost" } else { "" },
                if on { "off" } else { "on" },
                if on { "Turn off" } else { "Turn on" }
            )
        })
        .collect();
    page(
        &app,
        "Connect AI",
        "Connect",
        &format!(
            r#"{}<div class="top"><h1>Connect AI</h1></div><div class="sub">Let Claude or Codex work in your repos through MCP. Each gets its own key, and you can switch it off any time.</div><div>{rows}</div>
<div style="display:flex;flex-direction:column;gap:12px"><span>They can read code, open changes and see runs. <span class="sub">Merging, sending to GitHub and approving deploys stay with you.</span></span>
<div class="warn">{WARN}<span>Claude and Codex run in the cloud, so code they read leaves this Mac. Check each one’s data settings.</span></div>
<span class="sub">Want it to stay here? Diagnosis on failed runs uses RigSpark’s local model when <span class="mono">UPSTREAM_MODEL_URL</span> is set.</span></div>"#,
            flash(&q.0)
        ),
    )
}

async fn connect_post(State(app): State<Arc<App>>, P(name): P<String>, f: F) -> Response {
    if !checked(&app, &f) || !mcp::ASSISTANTS.contains(&name.as_str()) {
        return bad();
    }
    {
        let mut st = app.state.lock().unwrap();
        let a = st.settings.assistants.entry(name.clone()).or_default();
        match f.get("action").map(String::as_str) {
            Some("on") => {
                a.enabled = true;
                if a.key.is_empty() {
                    a.key = format!("ups_{}", crate::random_hex(24));
                }
            }
            Some("off") => a.enabled = false,
            Some("rotate") => {
                a.key = format!("ups_{}", crate::random_hex(24));
                a.last_used = None;
            }
            _ => {}
        }
    }
    app.save();
    Redirect::to("/connect").into_response()
}

async fn copilot(State(app): State<Arc<App>>, f: F) -> Response {
    if !checked(&app, &f) {
        return bad();
    }
    let choice = if f.get("choice").map(String::as_str) == Some("none") {
        "none"
    } else {
        "off"
    };
    app.state.lock().unwrap().settings.copilot_ack = Some(choice.into());
    app.save();
    let back = f
        .get("back")
        .filter(|b| b.starts_with('/') && !b.starts_with("//"))
        .cloned()
        .unwrap_or_else(|| "/".into());
    Redirect::to(&back).into_response()
}
