//! Asks the model RigSpark is serving on this Mac for a one-line likely cause of a failure.
//! Optional: set `UPSTREAM_MODEL_URL` (an OpenAI-compatible base such as
//! `http://127.0.0.1:11434/v1`) and `UPSTREAM_MODEL`. Only loopback addresses are used, so the
//! log never leaves this Mac.

use std::time::Duration;

pub fn likely_cause(tail: &[String]) -> Option<String> {
    let base = std::env::var("UPSTREAM_MODEL_URL").ok()?;
    if !(base.starts_with("http://127.0.0.1") || base.starts_with("http://localhost")) {
        return None;
    }
    let model = std::env::var("UPSTREAM_MODEL").unwrap_or_else(|_| "qwen3:8b".into());
    let prompt = format!(
        "A CI step failed. In one short sentence for the developer, say the most likely cause. No preamble.\n\nLast log lines:\n{}",
        tail.join("\n")
    );
    let body = serde_json::json!({"model": model, "messages": [{"role": "user", "content": prompt}], "max_tokens": 80, "temperature": 0});
    let resp: serde_json::Value =
        ureq::post(&format!("{}/chat/completions", base.trim_end_matches('/')))
            .timeout(Duration::from_secs(30))
            .send_json(body)
            .ok()?
            .into_json()
            .ok()?;
    let text = resp["choices"][0]["message"]["content"].as_str()?.trim();
    // Reasoning models may wrap thoughts; keep the last line that reads like an answer.
    let line = text.split("</think>").last()?.trim().lines().last()?.trim();
    (!line.is_empty()).then(|| format!("{line} (Diagnosed on this Mac.)"))
}
