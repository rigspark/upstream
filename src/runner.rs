//! Runs `.upstream/run.yml` on this Mac: one throwaway checkout per run, a scrubbed environment,
//! per-step logs with timings, and steps that wait for the user's approval.
//!
//! ```yaml
//! runs-on: linux        # which GitHub runner this replaces (linux, mac, windows): for savings
//! steps:
//!   - name: Test
//!     run: cargo test
//!   - name: Deploy
//!     run: ./deploy.sh
//!     approve: true     # waits on Runs until you approve
//! ```

use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::store::{now, Run, RunStatus, StepResult, StepStatus};
use crate::{git, App};

pub const RUN_FILE: &str = ".upstream/run.yml";
const STEP_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_LOG_LINES: usize = 50_000;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RunFile {
    #[serde(default = "linux", rename = "runs-on")]
    pub runs_on: String,
    #[serde(default)]
    pub steps: Vec<Step>,
}

fn linux() -> String {
    "linux".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Step {
    pub name: String,
    pub run: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub approve: bool,
}

pub fn parse(text: &str) -> Result<RunFile, String> {
    let f: RunFile = serde_yaml::from_str(text).map_err(|e| format!("{RUN_FILE}: {e}"))?;
    if f.steps.is_empty() {
        return Err(format!("{RUN_FILE} has no steps"));
    }
    Ok(f)
}

/// The steps for a commit: its own run file, else what was converted from GitHub Actions.
pub fn spec_for(app: &App, repo: &str, sha: &str) -> Option<Result<RunFile, String>> {
    let path = git::repo_path(&app.home, repo);
    if let Ok(bytes) = git::read_file(&path, sha, RUN_FILE) {
        return Some(parse(&String::from_utf8_lossy(&bytes)));
    }
    app.state
        .lock()
        .unwrap()
        .repos
        .get(repo)
        .and_then(|m| m.converted.clone())
        .map(Ok)
}

/// Converts GitHub Actions workflows: `run:` steps carry over, `actions/checkout` is implied,
/// anything else that `uses:` an action is listed for the user. Returns (steps, needs you).
pub fn convert_actions(workflows: &[String]) -> (Option<RunFile>, Vec<String>) {
    let mut out = RunFile {
        runs_on: "linux".into(),
        steps: vec![],
    };
    let mut needs = vec![];
    let mut mac = false;
    for text in workflows {
        let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(text) else {
            continue;
        };
        let Some(jobs) = doc.get("jobs").and_then(|j| j.as_mapping()) else {
            continue;
        };
        for (job_name, job) in jobs {
            let job_name = job_name.as_str().unwrap_or("job");
            let runs_on = format!("{:?}", job.get("runs-on"));
            if runs_on.contains("macos") {
                mac = true;
            }
            for step in job
                .get("steps")
                .and_then(|s| s.as_sequence())
                .into_iter()
                .flatten()
            {
                let name = step.get("name").and_then(|n| n.as_str());
                if let Some(run) = step.get("run").and_then(|r| r.as_str()) {
                    let first = run.lines().next().unwrap_or("run").trim();
                    out.steps.push(Step {
                        name: name.unwrap_or(first).to_string(),
                        run: run.trim_end().to_string(),
                        approve: false,
                    });
                } else if let Some(uses) = step.get("uses").and_then(|u| u.as_str()) {
                    if !uses.starts_with("actions/checkout") {
                        needs.push(format!("{job_name}: {}", name.unwrap_or(uses)));
                    }
                }
            }
        }
    }
    if mac {
        out.runs_on = "mac".into();
    }
    ((!out.steps.is_empty()).then_some(out), needs)
}

/// Queues a run for a branch tip, if the repo has steps. Returns the run id.
pub fn enqueue(
    app: &Arc<App>,
    repo: &str,
    branch: &str,
    sha: &str,
    from: Option<&Run>,
    from_step: usize,
) -> Option<u64> {
    let spec = match spec_for(app, repo, sha)? {
        Ok(s) => s,
        Err(e) => {
            // A broken run file is a failed run the user can read, not a silent skip.
            let id = {
                let mut st = app.state.lock().unwrap();
                let id = st.new_id();
                st.runs.push(Run {
                    id,
                    repo: repo.into(),
                    branch: branch.into(),
                    sha: sha.into(),
                    title: git::subject(&git::repo_path(&app.home, repo), sha),
                    runs_on: "linux".into(),
                    queued: now(),
                    started: Some(now()),
                    finished: Some(now()),
                    status: RunStatus::Failed,
                    steps: vec![],
                    from_step: 0,
                    cause: Some(e),
                    failed_test: None,
                });
                id
            };
            app.save();
            return Some(id);
        }
    };
    let title = git::subject(&git::repo_path(&app.home, repo), sha);
    let id = {
        let mut st = app.state.lock().unwrap();
        let id = st.new_id();
        let steps = spec
            .steps
            .iter()
            .enumerate()
            .map(
                |(i, s)| match from.and_then(|f| f.steps.get(i)).filter(|_| i < from_step) {
                    Some(prev) => prev.clone(),
                    None => StepResult {
                        name: s.name.clone(),
                        status: StepStatus::Pending,
                        start_ms: 0,
                        dur_ms: 0,
                        note: String::new(),
                    },
                },
            )
            .collect();
        st.runs.push(Run {
            id,
            repo: repo.into(),
            branch: branch.into(),
            sha: sha.into(),
            title,
            runs_on: spec.runs_on.clone(),
            queued: now(),
            started: None,
            finished: None,
            status: RunStatus::Queued,
            steps,
            from_step,
            cause: None,
            failed_test: None,
        });
        id
    };
    app.save();
    let app2 = app.clone();
    tokio::spawn(async move {
        let mut slot = app2.run_slot.acquire().await.ok();
        execute(&app2, id, spec, &mut slot).await;
    });
    Some(id)
}

pub fn log_path(home: &Path, id: u64) -> PathBuf {
    home.join("logs").join(format!("run-{id}.log"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogLine {
    pub s: usize,
    pub t: u64,
    pub l: String,
}

pub fn read_log(home: &Path, id: u64) -> Vec<LogLine> {
    std::fs::read_to_string(log_path(home, id))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn set(app: &App, id: u64, f: impl FnOnce(&mut Run)) {
    if let Some(r) = app.state.lock().unwrap().run_mut(id) {
        f(r);
    }
    app.save();
}

/// `slot` is this run's turn on the Mac. It is given back while a step waits for approval, so
/// one waiting deploy never holds up everyone else's checks.
async fn execute<'a>(
    app: &'a Arc<App>,
    id: u64,
    spec: RunFile,
    slot: &mut Option<tokio::sync::SemaphorePermit<'a>>,
) {
    let (repo, sha, from_step) = {
        let st = app.state.lock().unwrap();
        let Some(r) = st.run(id) else { return };
        (r.repo.clone(), r.sha.clone(), r.from_step)
    };
    set(app, id, |r| {
        r.status = RunStatus::Running;
        r.started = Some(now());
    });
    let _ = std::fs::create_dir_all(app.home.join("logs"));
    let mut log = match std::fs::File::create(log_path(&app.home, id)) {
        Ok(f) => f,
        Err(_) => return,
    };
    let work = match tempfile::tempdir() {
        Ok(t) => t,
        Err(_) => return,
    };
    let dir = work.path().join("src");
    let start = Instant::now();
    if let Err(e) = git::checkout(&git::repo_path(&app.home, &repo), &sha, &dir) {
        finish(
            app,
            id,
            RunStatus::Failed,
            Some(format!("Checkout failed: {e}")),
            None,
        );
        return;
    }
    let mut lines = 0usize;
    let mut failed: Option<usize> = None;
    let mut denied = false;
    for (i, step) in spec.steps.iter().enumerate() {
        if i < from_step {
            continue;
        }
        if failed.is_some() || denied {
            set(app, id, |r| r.steps[i].status = StepStatus::Skipped);
            continue;
        }
        if step.approve {
            set(app, id, |r| r.status = RunStatus::Waiting);
            let (tx, rx) = tokio::sync::oneshot::channel();
            app.approvals.lock().unwrap().insert(id, tx);
            drop(slot.take());
            let ok = rx.await.unwrap_or(false);
            *slot = app.run_slot.acquire().await.ok();
            if !ok {
                denied = true;
                set(app, id, |r| {
                    r.steps[i].status = StepStatus::Skipped;
                    r.steps[i].note = "denied".into();
                });
                continue;
            }
            set(app, id, |r| r.status = RunStatus::Running);
        }
        let t0 = start.elapsed().as_millis() as u64;
        set(app, id, |r| {
            r.steps[i].status = StepStatus::Running;
            r.steps[i].start_ms = t0;
        });
        let ok = run_step(&dir, &step.run, i, start, &mut log, &mut lines).await;
        let dur = start.elapsed().as_millis() as u64 - t0;
        set(app, id, |r| {
            r.steps[i].dur_ms = dur;
            r.steps[i].status = if ok.is_ok() {
                StepStatus::Passed
            } else {
                StepStatus::Failed
            };
            if let Err(note) = &ok {
                r.steps[i].note = note.clone();
            }
        });
        if ok.is_err() {
            failed = Some(i);
        }
    }
    let _ = log.flush();
    if denied {
        finish(
            app,
            id,
            RunStatus::Denied,
            Some("You denied a step that needed approval.".into()),
            None,
        );
    } else if let Some(i) = failed {
        let lines = read_log(&app.home, id);
        let (test, cause) = diagnose(&spec.steps[i].name, &lines, i);
        finish(app, id, RunStatus::Failed, None, test);
        let app2 = app.clone();
        tokio::task::spawn_blocking(move || {
            let c = cause.or_else(|| crate::model::likely_cause(&tail(&lines, i, 40)));
            set(&app2, id, |r| r.cause = c);
        });
    } else {
        finish(app, id, RunStatus::Passed, None, None);
    }
}

fn finish(app: &App, id: u64, status: RunStatus, cause: Option<String>, test: Option<String>) {
    set(app, id, |r| {
        r.status = status;
        r.finished = Some(now());
        if cause.is_some() {
            r.cause = cause;
        }
        r.failed_test = test;
    });
}

async fn run_step(
    dir: &Path,
    script: &str,
    idx: usize,
    start: Instant,
    log: &mut std::fs::File,
    lines: &mut usize,
) -> Result<(), String> {
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg(script).current_dir(dir).env_clear();
    for k in [
        "PATH",
        "HOME",
        "LANG",
        "USER",
        "TMPDIR",
        "CARGO_HOME",
        "RUSTUP_HOME",
    ] {
        if let Ok(v) = std::env::var(k) {
            cmd.env(k, v);
        }
    }
    cmd.env("CI", "true")
        .env("UPSTREAM", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| format!("could not start: {e}"))?;
    let out = BufReader::new(child.stdout.take().unwrap()).lines();
    let err = BufReader::new(child.stderr.take().unwrap()).lines();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::spawn(pump(out, tx.clone()));
    tokio::spawn(pump(err, tx.clone()));
    drop(tx);
    let wait = async {
        while let Some(l) = rx.recv().await {
            if *lines < MAX_LOG_LINES {
                let rec = LogLine {
                    s: idx,
                    t: start.elapsed().as_millis() as u64,
                    l,
                };
                let _ = writeln!(log, "{}", serde_json::to_string(&rec).unwrap_or_default());
                *lines += 1;
            }
        }
        child.wait().await
    };
    match tokio::time::timeout(STEP_TIMEOUT, wait).await {
        Err(_) => Err("timed out after 30 min".into()),
        Ok(Err(e)) => Err(e.to_string()),
        Ok(Ok(s)) if s.success() => Ok(()),
        Ok(Ok(s)) => Err(match s.code() {
            Some(c) => format!("exit {c}"),
            None => "killed".into(),
        }),
    }
}

async fn pump<R: tokio::io::AsyncBufRead + Unpin>(
    mut lines: tokio::io::Lines<R>,
    tx: tokio::sync::mpsc::UnboundedSender<String>,
) {
    while let Ok(Some(l)) = lines.next_line().await {
        if tx.send(l).is_err() {
            break;
        }
    }
}

pub fn tail(lines: &[LogLine], step: usize, n: usize) -> Vec<String> {
    let v: Vec<String> = lines
        .iter()
        .filter(|l| l.s == step)
        .map(|l| l.l.clone())
        .collect();
    v[v.len().saturating_sub(n)..].to_vec()
}

/// A failing test name and a plain cause, when the log makes it obvious.
pub fn diagnose(step: &str, lines: &[LogLine], idx: usize) -> (Option<String>, Option<String>) {
    let mine: Vec<&str> = lines
        .iter()
        .filter(|l| l.s == idx)
        .map(|l| l.l.as_str())
        .collect();
    let test = mine.iter().find_map(|l| {
        let l = l.trim();
        // cargo: "test a::b ... FAILED"; jest/pytest style: "FAIL x", "FAILED x::y"
        if let Some(rest) = l.strip_prefix("test ") {
            return rest.strip_suffix(" ... FAILED").map(str::to_string);
        }
        l.strip_prefix("FAILED ")
            .map(|r| r.split_whitespace().next().unwrap_or(r).to_string())
    });
    let cause = if mine.iter().any(|l| l.contains("command not found")) {
        Some(format!(
            "A command in “{step}” isn’t installed on this Mac."
        ))
    } else {
        None
    };
    (test, cause)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn converts_actions() {
        let wf = "jobs:\n  test:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/checkout@v4\n      - uses: dtolnay/rust-toolchain@stable\n      - name: Test\n        run: cargo test\n      - run: |\n          cargo fmt --check\n          cargo clippy\n";
        let (f, needs) = convert_actions(&[wf.into()]);
        let f = f.unwrap();
        assert_eq!(f.runs_on, "linux");
        assert_eq!(f.steps.len(), 2);
        assert_eq!(f.steps[1].name, "cargo fmt --check");
        assert_eq!(
            needs,
            vec!["test: dtolnay/rust-toolchain@stable".to_string()]
        );
    }
    #[test]
    fn finds_failed_test() {
        let l = vec![LogLine {
            s: 1,
            t: 0,
            l: "test engine::resume ... FAILED".into(),
        }];
        assert_eq!(diagnose("Test", &l, 1).0.as_deref(), Some("engine::resume"));
    }
}
