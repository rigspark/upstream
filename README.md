# RigSpark Upstream

Your code stays private to you only, till you are ready to show the world.

Upstream is source control that runs on your Mac. It hosts your git repos, reviews and merges
changes, runs your checks, and sends branches to GitHub only when you say so. It listens on
127.0.0.1 and nowhere else.

```bash
cargo run --release            # http://127.0.0.1:7900
upstream --port 7900 --home <folder>
```

Data lives in `~/Library/Application Support/org.rigspark.upstream` on a Mac
(`$XDG_DATA_HOME/rigspark-upstream` elsewhere, or `UPSTREAM_HOME`).

## What's in it

- **Repos.** Bare git repos you clone and push over HTTP: `git clone http://127.0.0.1:7900/git/<repo>.git`.
- **Changes.** An inbox of open changes, the ones waiting for you on top, each showing who opened it.
  Merges are squash merges. You can merge your own change: the safety check (a secret scan) stands in as the review.
  The co-author switch (on by default, remembered per repo) adds
  `Co-authored-by: RigSpark <noreply@rigspark.si>`.
- **Runs.** Every push runs `.upstream/run.yml` in a throwaway checkout with a scrubbed environment.
  A failed run opens on its cause and last lines; Trace and Full log are a tap away. Steps marked
  `approve: true` wait for you without holding up other runs.
- **Money.** Minutes are priced at GitHub's hosted-runner rates after free minutes, so "saved" is
  what GitHub would have billed. Moving in reads what Actions already cost you, if the token can
  read billing.
- **Diagnostics.** Only what's abnormal: flaky tests, slowdowns, long waits, a full disk.
- **Move in** from GitHub (mirror clone, workflows converted, open pull requests become changes) and
  **Send to GitHub** per branch, never forced, with the cost of turning workflows back on and the
  Copilot training warning in front of you. "Show the world" refuses to publish while the safety check finds
  secrets in history or commits carry personal emails.
- **Connect AI.** An MCP endpoint at `/mcp` for Claude, Codex and other apps on this Mac, one key
  each. They can read code, open changes and read runs. Merge, send and approve are not tools at
  all. Apps on this Mac read their key from `apps.json` in Upstream's data folder (mode 0600), so
  there's nothing to paste, and may name the agent that opened a change.

```yaml
# .upstream/run.yml
runs-on: linux        # linux, mac or windows: the GitHub runner this replaces, for pricing
steps:
  - name: Test
    run: cargo test
  - name: Deploy
    run: ./deploy.sh
    approve: true
```

Optional: `UPSTREAM_MODEL_URL=http://127.0.0.1:11434/v1` and `UPSTREAM_MODEL=qwen3:8b` let the model
RigSpark is serving write a one-line likely cause for failed runs. Loopback addresses only.

GitHub tokens are pasted per task, kept in memory for that task, and passed to git through the
environment. They are never written to disk or logs.

## Not yet

Runs execute on the Mac itself (no Linux VM), Windows jobs can't run locally, GitLab, Bitbucket
and Azure moves are not built, issues don't come along, and notifications and search are next.

## Develop

```bash
cargo test                       # unit tests + an end-to-end maintainer day over real git pushes
cargo clippy --all-targets -- -D warnings
```
