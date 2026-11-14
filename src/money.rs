//! What GitHub would have charged for the minutes Upstream ran locally.
//!
//! Rates are GitHub's published per-minute prices for standard hosted runners (January 2026),
//! with GitHub's quota multipliers. Free minutes are used up first, in the order GitHub bills,
//! so minutes that would have been free on GitHub count as $0 saved.

use crate::store::{Run, RunStatus};

pub fn rate(os: &str) -> f64 {
    match os {
        "mac" => 0.062,
        "windows" => 0.010,
        _ => 0.006,
    }
}

pub fn multiplier(os: &str) -> f64 {
    match os {
        "mac" => 10.0,
        "windows" => 2.0,
        _ => 1.0,
    }
}

pub fn included_minutes(plan: &str) -> f64 {
    if plan == "pro" {
        3000.0
    } else {
        2000.0
    }
}

/// GitHub's bill for one month of minutes by runner type: (os, minutes).
pub fn bill(minutes: &[(String, f64)], plan: &str) -> f64 {
    let mut left = included_minutes(plan);
    let mut cost = 0.0;
    for os in ["linux", "windows", "mac"] {
        let m: f64 = minutes
            .iter()
            .filter(|(o, _)| o == os)
            .map(|(_, m)| m)
            .sum();
        let equiv = m * multiplier(os);
        let covered = left.min(equiv);
        left -= covered;
        cost += (equiv - covered) / multiplier(os) * rate(os);
    }
    (cost * 100.0).round() / 100.0
}

/// GitHub bills each started minute.
fn billed_minutes(r: &Run) -> f64 {
    r.duration_ms()
        .map(|ms| (ms as f64 / 60_000.0).ceil())
        .unwrap_or(0.0)
}

fn month_key(unix: u64) -> (i64, u32) {
    // Civil date from days since epoch (Howard Hinnant's algorithm).
    let z = (unix / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m)
}

pub fn month_name(unix: u64) -> &'static str {
    const M: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    M[(month_key(unix).1 - 1) as usize]
}

/// Saved this month and saved in total, across all repos.
pub fn saved(runs: &[Run], plan: &str, now: u64) -> (f64, f64) {
    let mut by_month: std::collections::BTreeMap<(i64, u32), Vec<(String, f64)>> =
        Default::default();
    for r in runs
        .iter()
        .filter(|r| r.status.finished() && r.status != RunStatus::Denied)
    {
        if let Some(t) = r.finished {
            by_month
                .entry(month_key(t))
                .or_default()
                .push((r.runs_on.clone(), billed_minutes(r)));
        }
    }
    let this = by_month
        .get(&month_key(now))
        .map(|m| bill(m, plan))
        .unwrap_or(0.0);
    let total = by_month.values().map(|m| bill(m, plan)).sum::<f64>();
    (this, (total * 100.0).round() / 100.0)
}

/// What a repo's runs would cost per month on GitHub, from the last 30 days.
pub fn monthly_estimate(runs: &[Run], repo: &str, plan: &str, now: u64) -> f64 {
    let m: Vec<(String, f64)> = runs
        .iter()
        .filter(|r| {
            r.repo == repo
                && r.finished
                    .map(|t| now.saturating_sub(t) < 30 * 86_400)
                    .unwrap_or(false)
        })
        .map(|r| (r.runs_on.clone(), billed_minutes(r)))
        .collect();
    bill(&m, plan)
}

pub fn usd(v: f64) -> String {
    let cents = (v * 100.0).round() as i64;
    let whole = cents / 100;
    let mut s = whole.to_string();
    let mut i = s.len() as i64 - 3;
    while i > 0 {
        s.insert(i as usize, ',');
        i -= 3;
    }
    format!("${s}.{:02}", cents % 100)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn free_minutes_first_then_mac_overage() {
        // 1,840 Linux + 680 macOS on Free: 160 free equivalent minutes cover 16 macOS minutes.
        let b = bill(&[("linux".into(), 1840.0), ("mac".into(), 680.0)], "free");
        assert!((b - 41.17).abs() < 0.01, "{b}");
        assert_eq!(bill(&[("linux".into(), 1500.0)], "free"), 0.0);
    }
    #[test]
    fn months_and_money() {
        assert_eq!(month_key(1_763_078_400), (2025, 11)); // 2025-11-14
        assert_eq!(usd(1234.5), "$1,234.50");
    }
}
