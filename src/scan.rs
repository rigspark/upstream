//! Ward's safety check: looks for secrets in what a change adds, or in a repo's whole history
//! before it goes public. Pattern-based, local, and deliberately conservative.

use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub kind: &'static str,
    pub line: String,
}

fn patterns() -> &'static [(&'static str, Regex)] {
    static P: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    P.get_or_init(|| {
        [
            ("AWS access key", r"AKIA[0-9A-Z]{16}"),
            ("GitHub token", r"\b(ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{36}\b|github_pat_[A-Za-z0-9_]{40,}"),
            ("Private key", r"-----BEGIN (RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----"),
            ("Slack token", r"xox[abprs]-[A-Za-z0-9-]{10,}"),
            ("API key or password", r#"(?i)\b(api[_-]?key|secret|password|passwd|token)\b\s*[:=]\s*["'][^"'\s]{12,}["']"#),
        ]
        .into_iter()
        .map(|(k, r)| (k, Regex::new(r).expect("pattern")))
        .collect()
    })
}

pub fn scan_text(text: &str) -> Vec<Finding> {
    let mut out = vec![];
    for line in text.lines() {
        for (kind, re) in patterns() {
            if re.is_match(line) {
                out.push(Finding {
                    kind,
                    line: redact(line),
                });
                break;
            }
        }
    }
    out
}

/// Only lines a diff adds count; removing a secret is good news.
pub fn scan_diff(diff: &str) -> Vec<Finding> {
    let added: String = diff
        .lines()
        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
        .map(|l| &l[1..])
        .collect::<Vec<_>>()
        .join("\n");
    scan_text(&added)
}

/// Keeps a finding readable without repeating the secret.
fn redact(line: &str) -> String {
    let t = line.trim();
    let keep: String = t.chars().take(24).collect();
    format!("{keep}… (hidden)")
}

/// A personal email in public history: anything that isn't a no-reply or local address.
pub fn personal_email(e: &str) -> bool {
    !(e.ends_with("noreply.github.com")
        || e.ends_with("@localhost")
        || e.contains("noreply")
        || e.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finds_added_secrets_only() {
        let d = "+++ b/x\n+let k = \"AKIAABCDEFGHIJKLMNOP\";\n-password = \"oldoldoldoldold\"\n+fine line\n";
        let f = scan_diff(d);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, "AWS access key");
        assert!(!f[0].line.contains("MNOP"));
    }
    #[test]
    fn emails() {
        assert!(personal_email("me@gmail.com"));
        assert!(!personal_email("1+me@users.noreply.github.com"));
    }
}
