//! Diagnostics that only speak up when something is off.

use crate::store::{Run, RunStatus};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct Attention {
    pub title: String,
    pub detail: String,
    /// A run to open from this item.
    pub run: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Report {
    pub attention: Vec<Attention>,
    pub pass_rate: Option<u32>,
    pub median_run: Option<u64>,
    pub median_before: Option<u64>,
    pub median_wait: Option<u64>,
    pub disk_used: Option<u32>,
    /// Daily median run time in seconds, oldest first, for the last 14 days with runs.
    pub trend: Vec<u64>,
}

fn median(mut v: Vec<u64>) -> Option<u64> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    Some(v[v.len() / 2])
}

pub fn report(runs: &[Run], now: u64, home: &Path) -> Report {
    let done: Vec<&Run> = runs
        .iter()
        .filter(|r| r.status == RunStatus::Passed || r.status == RunStatus::Failed)
        .collect();
    let week = |r: &&Run, from: u64, to: u64| {
        r.finished
            .map(|t| t + from * 86_400 > now && t + to * 86_400 <= now)
            .unwrap_or(false)
    };
    let this: Vec<&Run> = done.iter().copied().filter(|r| week(r, 7, 0)).collect();
    let before: Vec<&Run> = done.iter().copied().filter(|r| week(r, 14, 7)).collect();
    let pass_rate = (!this.is_empty()).then(|| {
        (100 * this
            .iter()
            .filter(|r| r.status == RunStatus::Passed)
            .count()
            / this.len()) as u32
    });
    let median_run = median(this.iter().filter_map(|r| r.duration_ms()).collect());
    let median_before = median(before.iter().filter_map(|r| r.duration_ms()).collect());
    let median_wait = median(this.iter().filter_map(|r| r.wait_ms()).collect());
    let mut attention = vec![];

    // Flaky: a test that failed in 2+ of a repo's last 20 runs, never in two runs in a row.
    let mut by_repo: BTreeMap<&str, Vec<&Run>> = BTreeMap::new();
    for r in &done {
        by_repo.entry(r.repo.as_str()).or_default().push(r);
    }
    for (repo, mut rs) in by_repo {
        rs.sort_by_key(|r| r.id);
        let last: Vec<&Run> = rs.iter().rev().take(20).rev().copied().collect();
        let mut seen: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (i, r) in last.iter().enumerate() {
            if let Some(t) = r.failed_test.as_deref() {
                seen.entry(t).or_default().push(i);
            }
        }
        for (test, idx) in seen {
            let consecutive = idx.windows(2).any(|w| w[1] == w[0] + 1);
            if idx.len() >= 2 && !consecutive {
                attention.push(Attention {
                    title: format!("Flaky test: {test}"),
                    detail: format!(
                        "{repo}: failed {} of the last {} runs, never twice in a row",
                        idx.len(),
                        last.len()
                    ),
                    run: idx.last().map(|i| last[*i].id),
                });
            }
        }
    }
    // Slowdown: this week's median is 15% and 20 s slower than last week's.
    if let (Some(a), Some(b)) = (median_run, median_before) {
        if a > b + 20_000 && a * 100 > b * 115 {
            attention.push(Attention {
                title: format!("Runs are {} slower this week", secs(a - b)),
                detail: format!("Median run went from {} to {}", secs(b), secs(a)),
                run: None,
            });
        }
    }
    // Runs stuck waiting to start.
    if let Some(w) = median_wait {
        if w > 120_000 {
            attention.push(Attention {
                title: "Runs wait a long time to start".into(),
                detail: format!("Median wait {}", secs(w)),
                run: None,
            });
        }
    }
    let disk_used = disk_percent(home);
    if let Some(d) = disk_used {
        if d >= 90 {
            attention.push(Attention {
                title: "This Mac’s disk is almost full".into(),
                detail: format!("{d}% used. Runs may start failing."),
                run: None,
            });
        }
    }
    let mut days: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    for r in &done {
        if let (Some(t), Some(d)) = (r.finished, r.duration_ms()) {
            if t + 14 * 86_400 > now {
                days.entry(t / 86_400).or_default().push(d / 1000);
            }
        }
    }
    let trend = days.into_values().filter_map(median).collect();
    Report {
        attention,
        pass_rate,
        median_run,
        median_before,
        median_wait,
        disk_used,
        trend,
    }
}

fn disk_percent(home: &Path) -> Option<u32> {
    let out = std::process::Command::new("df")
        .arg("-Pk")
        .arg(home)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let pct = text.lines().nth(1)?.split_whitespace().nth(4)?;
    pct.trim_end_matches('%').parse().ok()
}

pub fn secs(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 60 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Run;
    fn run(id: u64, test: Option<&str>, dur: u64, t: u64) -> Run {
        Run {
            id,
            repo: "r".into(),
            branch: "main".into(),
            sha: "x".into(),
            title: "t".into(),
            runs_on: "linux".into(),
            queued: t - dur,
            started: Some(t - dur),
            finished: Some(t),
            status: if test.is_some() {
                RunStatus::Failed
            } else {
                RunStatus::Passed
            },
            steps: vec![],
            from_step: 0,
            cause: None,
            failed_test: test.map(str::to_string),
        }
    }
    #[test]
    fn flaky_and_slow() {
        let now = 10_000_000;
        let mut rs = vec![];
        for i in 0..10 {
            let t = if i == 2 || i == 6 {
                Some("auth::plan_waits")
            } else {
                None
            };
            rs.push(run(i, t, 190, now - 8 * 86_400 - i * 60));
        }
        for i in 10..20 {
            let t = if i == 15 {
                Some("auth::plan_waits")
            } else {
                None
            };
            rs.push(run(i, t, 240, now - 86_400 - i * 60));
        }
        let r = report(&rs, now, Path::new("/"));
        assert!(r
            .attention
            .iter()
            .any(|a| a.title.starts_with("Flaky test: auth::plan_waits")));
        assert!(
            r.attention.iter().any(|a| a.title.contains("slower")),
            "{:?}",
            r.attention
        );
        assert_eq!(r.pass_rate, Some(90));
    }
}
