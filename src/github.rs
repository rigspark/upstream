//! The few GitHub calls Upstream makes, always with a token the user pastes for that one task.
//! Tokens live in memory for the task and are passed to git through the environment, never argv
//! and never disk. `UPSTREAM_GITHUB_API` and `UPSTREAM_GITHUB_GIT` point elsewhere for testing.

use base64::Engine;
use serde_json::Value;
use std::time::Duration;

pub fn api_base() -> String {
    std::env::var("UPSTREAM_GITHUB_API").unwrap_or_else(|_| "https://api.github.com".into())
}

pub fn git_base() -> String {
    std::env::var("UPSTREAM_GITHUB_GIT").unwrap_or_else(|_| "https://github.com".into())
}

pub fn remote_url(owner: &str, name: &str) -> String {
    format!("{}/{owner}/{name}.git", git_base())
}

/// Env for git so it authenticates to GitHub without the token touching argv or config files.
pub fn git_auth_env(token: &str) -> Vec<(String, String)> {
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"));
    vec![
        ("GIT_CONFIG_COUNT".into(), "1".into()),
        (
            "GIT_CONFIG_KEY_0".into(),
            format!("http.{}/.extraheader", git_base()),
        ),
        (
            "GIT_CONFIG_VALUE_0".into(),
            format!("AUTHORIZATION: basic {basic}"),
        ),
    ]
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .build()
}

fn call(method: &str, path: &str, token: &str, body: Option<Value>) -> Result<Value, String> {
    let req = agent()
        .request(method, &format!("{}{path}", api_base()))
        .set("Authorization", &format!("Bearer {token}"))
        .set("Accept", "application/vnd.github+json")
        .set("X-GitHub-Api-Version", "2022-11-28")
        .set("User-Agent", "rigspark-upstream");
    let resp = match body {
        Some(b) => req.send_json(b),
        None => req.call(),
    };
    match resp {
        Ok(r) => Ok(r.into_json().unwrap_or(Value::Null)),
        Err(ureq::Error::Status(401, _)) => Err("GitHub didn’t accept that token.".into()),
        Err(ureq::Error::Status(code, r)) => {
            let msg = r
                .into_json::<Value>()
                .ok()
                .and_then(|v| v["message"].as_str().map(str::to_string))
                .unwrap_or_default();
            Err(format!("GitHub said {code}: {msg}"))
        }
        Err(e) => Err(format!("Couldn’t reach GitHub: {e}")),
    }
}

pub fn login(token: &str) -> Result<String, String> {
    call("GET", "/user", token, None)?["login"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "No login in GitHub’s answer".into())
}

#[derive(Debug, Clone)]
pub struct RemoteRepo {
    pub owner: String,
    pub name: String,
    pub private: bool,
}

/// Every repo you own or reach through an organisation you belong to, a page of 100 at a time.
pub fn repos(token: &str) -> Result<Vec<RemoteRepo>, String> {
    let mut all = Vec::new();
    for page in 1..=50 {
        let v = call(
            "GET",
            &format!(
                "/user/repos?per_page=100&page={page}&affiliation=owner,organization_member&sort=pushed"
            ),
            token,
            None,
        )?;
        let batch: Vec<RemoteRepo> = v
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| {
                Some(RemoteRepo {
                    owner: r["owner"]["login"].as_str()?.into(),
                    name: r["name"].as_str()?.into(),
                    private: r["private"].as_bool().unwrap_or(true),
                })
            })
            .collect();
        let last = batch.len() < 100;
        all.extend(batch);
        if last {
            break;
        }
    }
    Ok(all)
}

pub fn workflow_ids(token: &str, owner: &str, name: &str) -> Vec<u64> {
    call(
        "GET",
        &format!("/repos/{owner}/{name}/actions/workflows"),
        token,
        None,
    )
    .ok()
    .and_then(|v| {
        v["workflows"]
            .as_array()
            .map(|a| a.iter().filter_map(|w| w["id"].as_u64()).collect())
    })
    .unwrap_or_default()
}

/// Switches workflows off (or back on) without touching the YAML in the repo.
pub fn set_workflows(
    token: &str,
    owner: &str,
    name: &str,
    ids: &[u64],
    enabled: bool,
) -> Result<(), String> {
    let verb = if enabled { "enable" } else { "disable" };
    for id in ids {
        call(
            "PUT",
            &format!("/repos/{owner}/{name}/actions/workflows/{id}/{verb}"),
            token,
            None,
        )?;
    }
    Ok(())
}

pub fn make_public(token: &str, owner: &str, name: &str) -> Result<(), String> {
    call(
        "PATCH",
        &format!("/repos/{owner}/{name}"),
        token,
        Some(serde_json::json!({"private": false})),
    )
    .map(|_| ())
}

/// Actions spending per repo from GitHub's billing usage report: (total, last full month).
/// Needs a token that can read billing; None when GitHub won't say.
pub fn actions_spend(token: &str, login: &str, repo: &str) -> Option<(f64, f64)> {
    let v = call(
        "GET",
        &format!("/users/{login}/settings/billing/usage"),
        token,
        None,
    )
    .ok()?;
    let items = v["usageItems"].as_array()?;
    let mut total = 0.0;
    let mut months: std::collections::BTreeMap<String, f64> = Default::default();
    for i in items {
        let product = i["product"].as_str().unwrap_or("").to_ascii_lowercase();
        let r = i["repositoryName"].as_str().unwrap_or("");
        if product == "actions" && (r == repo || r.ends_with(&format!("/{repo}"))) {
            let amt = i["netAmount"].as_f64().unwrap_or(0.0);
            total += amt;
            let month = i["date"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(7)
                .collect::<String>();
            *months.entry(month).or_default() += amt;
        }
    }
    let last_full = months
        .values()
        .rev()
        .nth(1)
        .copied()
        .or_else(|| months.values().last().copied())
        .unwrap_or(0.0);
    Some((
        (total * 100.0).round() / 100.0,
        (last_full * 100.0).round() / 100.0,
    ))
}
