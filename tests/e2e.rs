//! End to end: a real server on loopback, real git pushes over HTTP, real runs.

use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

struct Server {
    base: String,
    home: tempfile::TempDir,
}

fn start() -> Server {
    let home = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let h = home.path().to_path_buf();
    std::thread::spawn(move || {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(rigspark_upstream::serve(h, port))
            .unwrap();
    });
    let base = format!("http://127.0.0.1:{port}");
    for _ in 0..100 {
        if ureq::get(&base).call().is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Server { base, home }
}

impl Server {
    fn get(&self, path: &str) -> String {
        ureq::get(&format!("{}{path}", self.base))
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    }
    fn token(&self) -> String {
        let page = self.get("/new");
        let i = page.find(r#"name="t" value=""#).unwrap() + 16;
        page[i..i + 32].to_string()
    }
    fn post(&self, path: &str, form: &[(&str, &str)]) -> (u16, String) {
        let t = self.token();
        let mut f: Vec<(&str, &str)> = vec![("t", &t)];
        f.extend_from_slice(form);
        match ureq::post(&format!("{}{path}", self.base)).send_form(&f) {
            Ok(r) => (r.status(), r.into_string().unwrap()),
            Err(ureq::Error::Status(c, r)) => (c, r.into_string().unwrap_or_default()),
            Err(e) => panic!("{e}"),
        }
    }
    fn state(&self) -> Value {
        serde_json::from_slice(
            &std::fs::read(self.home.path().join("state.json")).unwrap_or_default(),
        )
        .unwrap_or(json!({}))
    }
    /// Waits for the newest run on `branch` to finish (or to wait for approval).
    fn wait_run(&self, branch: &str, statuses: &[&str]) -> Value {
        let t = Instant::now();
        loop {
            let st = self.state();
            if let Some(r) = st["runs"]
                .as_array()
                .and_then(|a| a.iter().rev().find(|r| r["branch"] == branch))
            {
                if statuses.contains(&r["status"].as_str().unwrap_or("")) {
                    return r.clone();
                }
            }
            assert!(
                t.elapsed() < Duration::from_secs(60),
                "run on {branch} never reached {statuses:?}: {st}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    fn mcp(&self, key: &str, method: &str, params: Value) -> Value {
        ureq::post(&format!("{}/mcp", self.base))
            .set("Authorization", &format!("Bearer {key}"))
            .send_json(json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
            .unwrap()
            .into_json()
            .unwrap()
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn write(dir: &Path, path: &str, text: &str) {
    let p = dir.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

const RUN: &str = "runs-on: mac\nsteps:\n  - name: Build\n    run: echo building && sleep 1\n  - name: Test\n    run: |\n      echo running 1 test\n      test -f ok.txt || { echo 'test engine::resume ... FAILED'; echo 'missing field `retries`'; exit 1; }\n      echo 'test result: ok'\n";

#[test]
fn maintainer_day() {
    let s = start();

    // Loopback guard: other Host names and token-less forms are refused.
    let evil = ureq::get(&s.base).set("Host", "evil.example:80").call();
    assert!(matches!(evil, Err(ureq::Error::Status(403, _))));
    let no_token = ureq::post(&format!("{}/new", s.base)).send_form(&[("name", "x")]);
    assert!(matches!(no_token, Err(ureq::Error::Status(403, _))));

    // New repo, then push over HTTP.
    let (code, page) = s.post("/new", &[("name", "demo")]);
    assert_eq!(code, 200);
    assert!(
        page.contains("git push upstream main"),
        "empty repo shows push help"
    );
    let work = tempfile::tempdir().unwrap();
    let w = work.path();
    git(w, &["init", "-q", "-b", "main"]);
    git(w, &["config", "user.email", "me@example.com"]);
    git(w, &["config", "user.name", "Me"]);
    write(w, ".upstream/run.yml", RUN);
    write(w, "ok.txt", "ok");
    write(w, "README.md", "# demo");
    git(w, &["add", "-A"]);
    git(w, &["commit", "-q", "-m", "First"]);
    let url = format!("{}/git/demo.git", s.base);
    git(w, &["remote", "add", "upstream", &url]);
    git(w, &["push", "-q", "upstream", "main"]);
    let r = s.wait_run("main", &["passed", "failed"]);
    assert_eq!(r["status"], "passed", "{r}");
    assert_eq!(r["steps"].as_array().unwrap().len(), 2);

    // Clone works, and the code page shows the files and clone URL.
    let clone = tempfile::tempdir().unwrap();
    git(clone.path(), &["clone", "-q", &url, "c"]);
    assert!(clone.path().join("c/README.md").exists());
    let code_page = s.get("/r/demo");
    assert!(code_page.contains("README.md") && code_page.contains(&format!("git clone {url}")));

    // A failing branch: the run names the failing test and shows the last lines.
    git(w, &["checkout", "-q", "-b", "feat"]);
    git(w, &["rm", "-q", "ok.txt"]);
    git(w, &["commit", "-q", "-m", "Break it"]);
    git(w, &["push", "-q", "upstream", "feat"]);
    let r = s.wait_run("feat", &["passed", "failed"]);
    assert_eq!(r["status"], "failed");
    assert_eq!(r["failed_test"], "engine::resume");
    let page = s.get(&format!("/run/{}", r["id"]));
    assert!(
        page.contains("Checks failed")
            && page.contains("engine::resume")
            && page.contains("missing field")
    );
    assert!(page.contains("Re-run failed step"));
    assert!(s
        .get(&format!("/run/{}?view=trace", r["id"]))
        .contains("priced as GitHub macOS"));
    assert!(s
        .get(&format!("/run/{}?view=log&q=retries", r["id"]))
        .contains("retries"));

    // Claude connects over MCP: no key, no entry; the tools never include merge or send.
    let denied = ureq::post(&format!("{}/mcp", s.base))
        .send_json(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}));
    assert!(matches!(denied, Err(ureq::Error::Status(401, _))));
    s.post("/connect/Claude", &[("action", "on")]);
    let key = s.state()["settings"]["assistants"]["Claude"]["key"]
        .as_str()
        .unwrap()
        .to_string();
    let init = s.mcp(&key, "initialize", json!({"protocolVersion": "2025-06-18"}));
    assert_eq!(init["result"]["serverInfo"]["name"], "rigspark-upstream");
    let tools = s.mcp(&key, "tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"open_change"));
    assert!(
        names
            .iter()
            .all(|n| !n.contains("merge") && !n.contains("send") && !n.contains("approve")),
        "{names:?}"
    );
    let file = s.mcp(
        &key,
        "tools/call",
        json!({"name": "read_file", "arguments": {"repo": "demo", "path": "README.md"}}),
    );
    assert_eq!(file["result"]["content"][0]["text"], "# demo");
    let log = s.mcp(
        &key,
        "tools/call",
        json!({"name": "get_run_log", "arguments": {"run_id": r["id"]}}),
    );
    assert!(log["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("engine::resume"));

    // Claude pushes a fix and opens a change; the inbox shows who opened it.
    git(w, &["checkout", "-q", "main"]);
    git(w, &["checkout", "-q", "-b", "fix"]);
    write(w, "hello.txt", "hi");
    git(w, &["add", "-A"]);
    git(w, &["commit", "-q", "-m", "Say hello"]);
    git(w, &["push", "-q", "upstream", "fix"]);
    s.wait_run("fix", &["passed"]);
    let opened = s.mcp(&key, "tools/call", json!({"name": "open_change", "arguments": {"repo": "demo", "from": "fix", "title": "Say hello"}}));
    assert!(
        opened["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Opened change"),
        "{opened}"
    );
    let inbox = s.get("/changes");
    assert!(
        inbox.contains("Waiting for you")
            && inbox.contains("opened by Claude")
            && inbox.contains("Ready to merge"),
        "{inbox}"
    );
    let id = s.state()["changes"][0]["id"].as_u64().unwrap();
    let change = s.get(&format!("/c/{id}"));
    assert!(
        change.contains("Join RigSpark")
            && change.contains(" checked")
            && change.contains("hello.txt")
    );

    // Merge with the co-author switch on: the trailer lands in the commit on main.
    let (code, _) = s.post(&format!("/c/{id}/merge"), &[("coauthor", "on")]);
    assert_eq!(code, 200);
    let bare = s.home.path().join("repos/demo.git");
    let msg = git(&bare, &["log", "-1", "--format=%B", "main"]);
    assert!(
        msg.starts_with("Say hello (#")
            && msg.contains("Co-authored-by: RigSpark <noreply@rigspark.si>"),
        "{msg}"
    );
    assert_eq!(s.state()["changes"][0]["state"], "merged");

    // A second change merged with the switch off: no trailer, and the choice is remembered.
    git(w, &["checkout", "-q", "-b", "fix2"]);
    write(w, "two.txt", "2");
    git(w, &["add", "-A"]);
    git(w, &["commit", "-q", "-m", "Two"]);
    git(w, &["push", "-q", "upstream", "fix2"]);
    s.wait_run("fix2", &["passed"]);
    let (_, _) = s.post(
        "/changes",
        &[
            ("repo", "demo"),
            ("from", "fix2"),
            ("into", "main"),
            ("title", ""),
        ],
    );
    let id2 = s.state()["changes"][1]["id"].as_u64().unwrap();
    s.post(&format!("/c/{id2}/merge"), &[]);
    let msg = git(&bare, &["log", "-1", "--format=%B", "main"]);
    assert!(
        msg.starts_with("Two (#") && !msg.contains("Co-authored-by"),
        "{msg}"
    );
    assert_eq!(s.state()["repos"]["demo"]["coauthor"], false);

    // A step that needs approval waits on Runs until approved.
    git(w, &["checkout", "-q", "-b", "deploy"]);
    write(w, ".upstream/run.yml", "steps:\n  - name: Build\n    run: echo ok\n  - name: Deploy\n    run: echo deployed\n    approve: true\n");
    git(w, &["add", "-A"]);
    git(w, &["commit", "-q", "-m", "Deploy step"]);
    git(w, &["push", "-q", "upstream", "deploy"]);
    let r = s.wait_run("deploy", &["waiting"]);
    assert!(s.get("/runs").contains("wants to run <b>Deploy</b>"));
    // While it waits, other runs still go ahead.
    git(w, &["checkout", "-q", "fix2"]);
    git(w, &["checkout", "-q", "-b", "meanwhile"]);
    git(w, &["commit", "-q", "--allow-empty", "-m", "Meanwhile"]);
    git(w, &["push", "-q", "upstream", "meanwhile"]);
    assert_eq!(
        s.wait_run("meanwhile", &["passed", "failed"])["status"],
        "passed"
    );
    s.post(&format!("/run/{}/approve", r["id"]), &[("ok", "yes")]);
    assert_eq!(
        s.wait_run("deploy", &["passed", "failed", "denied"])["status"],
        "passed"
    );

    // Money and diagnostics render; macOS minutes are priced.
    let runs = s.get("/runs");
    assert!(
        (runs.contains("saved in") || runs.contains("No bill")) && runs.contains("Diagnostics")
    );
    assert!(s.get("/diagnostics").contains("Everything else is normal"));

    // Copilot warning shows until answered, then stays gone.
    assert!(s.get("/").contains("Copilot may be training on your code"));
    s.post("/copilot", &[("choice", "off"), ("back", "/")]);
    assert!(!s.get("/").contains("Copilot may be training"));
}

#[test]
fn crew_link_and_byte_changes() {
    let crew = tempfile::tempdir().unwrap();
    // Only this test sets it; the other test never reads Crew's folder.
    std::env::set_var("RIGSPARK_CREW_HOME", crew.path());
    let s = start();
    s.post("/new", &[("name", "app")]);
    let work = tempfile::tempdir().unwrap();
    let w = work.path();
    git(w, &["init", "-q", "-b", "main"]);
    git(w, &["config", "user.email", "me@example.com"]);
    git(w, &["config", "user.name", "Me"]);
    write(w, "README.md", "# app");
    git(w, &["add", "-A"]);
    git(w, &["commit", "-q", "-m", "First"]);
    git(w, &["remote", "add", "up", &format!("{}/git/app.git", s.base)]);
    git(w, &["push", "-q", "up", "main"]);
    git(w, &["checkout", "-q", "-b", "byte/hello"]);
    write(w, "hello.txt", "hi");
    git(w, &["add", "-A"]);
    git(w, &["commit", "-q", "-m", "Hello"]);
    git(w, &["push", "-q", "up", "byte/hello"]);

    // Switching Crew on hands it the address and its key; nothing to paste.
    let (code, page) = s.post("/connect/RigSpark%20Crew", &[("action", "on")]);
    assert_eq!(code, 200);
    assert!(page.contains("Linked to RigSpark Crew"), "{page}");
    let link: Value = serde_json::from_str(&std::fs::read_to_string(crew.path().join("upstream.json")).unwrap()).unwrap();
    let key = s.state()["settings"]["assistants"]["RigSpark Crew"]["key"].as_str().unwrap().to_string();
    assert_eq!(link["key"], key.as_str());
    assert_eq!(link["url"], s.base.as_str());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(crew.path().join("upstream.json")).unwrap().permissions().mode() & 0o777, 0o600);
    }

    // Crew names the bot that opened the change; other assistants can't.
    let r = s.mcp(&key, "tools/call", json!({"name": "open_change", "arguments": {"repo": "app", "from": "byte/hello", "title": "Hello", "by": "Byte"}}));
    assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("Opened change"), "{r}");
    assert!(s.get("/changes").contains("opened by Byte"));
    s.post("/connect/Claude", &[("action", "on")]);
    let claude = s.state()["settings"]["assistants"]["Claude"]["key"].as_str().unwrap().to_string();
    git(w, &["checkout", "-q", "-b", "other"]);
    git(w, &["push", "-q", "up", "other"]);
    s.mcp(&claude, "tools/call", json!({"name": "open_change", "arguments": {"repo": "app", "from": "other", "title": "Other", "by": "Byte"}}));
    let st = s.state();
    let other = st["changes"].as_array().unwrap().iter().find(|c| c["from"] == "other").unwrap();
    assert_eq!(other["opened_by"], "Claude");

    // Off takes the key back.
    s.post("/connect/RigSpark%20Crew", &[("action", "off")]);
    assert!(!crew.path().join("upstream.json").exists());
}
