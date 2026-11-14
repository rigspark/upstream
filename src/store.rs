//! Everything Upstream remembers, in one JSON file next to the repos.
//!
//! The file lives in the data folder (mode 0600) and is rewritten atomically. Access tokens for
//! GitHub are never stored here; assistant keys for MCP are, because they must survive restarts.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::runner::RunFile;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub next_id: u64,
    #[serde(default)]
    pub changes: Vec<Change>,
    #[serde(default)]
    pub runs: Vec<Run>,
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub repos: BTreeMap<String, RepoMeta>,
}

impl State {
    pub fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
    pub fn meta(&mut self, repo: &str) -> &mut RepoMeta {
        self.repos.entry(repo.to_string()).or_default()
    }
    pub fn run(&self, id: u64) -> Option<&Run> {
        self.runs.iter().find(|r| r.id == id)
    }
    pub fn run_mut(&mut self, id: u64) -> Option<&mut Run> {
        self.runs.iter_mut().find(|r| r.id == id)
    }
    pub fn change(&self, id: u64) -> Option<&Change> {
        self.changes.iter().find(|c| c.id == id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// The user's answer to the Copilot training warning: "off" or "none". None: not answered.
    #[serde(default)]
    pub copilot_ack: Option<String>,
    /// GitHub plan used to price savings: "free" (2,000 minutes) or "pro" (3,000 minutes).
    #[serde(default = "free")]
    pub plan: String,
    #[serde(default)]
    pub assistants: BTreeMap<String, Assistant>,
}

fn free() -> String {
    "free".into()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            copilot_ack: None,
            plan: free(),
            assistants: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Assistant {
    pub enabled: bool,
    pub key: String,
    #[serde(default)]
    pub last_used: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoMeta {
    /// Where this repo came from on GitHub, if it moved in from there.
    #[serde(default)]
    pub github: Option<GithubLink>,
    /// Branch tips as they were on GitHub when the repo moved in or was last sent.
    #[serde(default)]
    pub left_github: BTreeMap<String, String>,
    /// "Join RigSpark's mission" co-author credit on merges. On by default, remembered per repo.
    #[serde(default = "yes")]
    pub coauthor: bool,
    /// Steps converted from GitHub Actions, used when the repo has no `.upstream/run.yml`.
    #[serde(default)]
    pub converted: Option<RunFile>,
    /// Workflow steps that could not be converted automatically.
    #[serde(default)]
    pub needs_you: Vec<String>,
    /// What GitHub billed for Actions on this repo since tracking began, if it could be read.
    #[serde(default)]
    pub lost_usd: Option<f64>,
    /// Actions bill for the last full month, if it could be read.
    #[serde(default)]
    pub last_month_usd: Option<f64>,
}

fn yes() -> bool {
    true
}

impl Default for RepoMeta {
    fn default() -> Self {
        Self {
            github: None,
            left_github: BTreeMap::new(),
            coauthor: true,
            converted: None,
            needs_you: vec![],
            lost_usd: None,
            last_month_usd: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubLink {
    pub owner: String,
    pub name: String,
    pub private: bool,
    #[serde(default)]
    pub workflow_ids: Vec<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChangeState {
    Open,
    Merged,
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Change {
    pub id: u64,
    pub repo: String,
    pub title: String,
    pub from: String,
    pub into: String,
    /// "you", or the assistant or bot that opened it ("Claude", "Codex", …).
    pub opened_by: String,
    pub created: u64,
    pub state: ChangeState,
    #[serde(default)]
    pub merged_sha: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunStatus {
    Queued,
    Running,
    /// Paused on a step that needs the user's approval.
    Waiting,
    Passed,
    Failed,
    Denied,
}

impl RunStatus {
    pub fn finished(self) -> bool {
        matches!(self, Self::Passed | Self::Failed | Self::Denied)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StepStatus {
    Pending,
    Running,
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    pub name: String,
    pub status: StepStatus,
    /// Milliseconds from the start of the run.
    #[serde(default)]
    pub start_ms: u64,
    #[serde(default)]
    pub dur_ms: u64,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub id: u64,
    pub repo: String,
    pub branch: String,
    pub sha: String,
    pub title: String,
    /// "linux", "mac" or "windows": which GitHub runner this run replaces, for pricing.
    pub runs_on: String,
    pub queued: u64,
    #[serde(default)]
    pub started: Option<u64>,
    #[serde(default)]
    pub finished: Option<u64>,
    pub status: RunStatus,
    pub steps: Vec<StepResult>,
    /// First step to execute; earlier steps are reused from the run this one repeats.
    #[serde(default)]
    pub from_step: usize,
    #[serde(default)]
    pub cause: Option<String>,
    #[serde(default)]
    pub failed_test: Option<String>,
}

impl Run {
    pub fn duration_ms(&self) -> Option<u64> {
        Some(self.finished?.saturating_sub(self.started?) * 1000)
    }
    pub fn wait_ms(&self) -> Option<u64> {
        Some(self.started?.saturating_sub(self.queued) * 1000)
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn load(path: &Path) -> State {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save(path: &Path, state: &State) -> std::io::Result<()> {
    let tmp: PathBuf = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(state)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(tmp, path)
}
