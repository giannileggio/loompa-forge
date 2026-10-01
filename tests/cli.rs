//! End-to-end tests against the built `lf` binary.
//!
//! The unit tests under `src/` cover each command's logic directly; these
//! instead go through `Cli::parse` and the real filesystem, to catch
//! problems in argument wiring (flags, conflicts, global `--home`) and in
//! how commands compose through the process boundary (exit codes, stdin,
//! files one command leaves for the next to read).

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

/// A fresh `lf` invocation against `home`. `$HOME` is also pointed at it, so
/// the global-skill links `lf init` may touch land inside the temp dir
/// instead of the real user home.
fn lf(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lf"));
    cmd.env("HOME", home)
        .arg("--home")
        .arg(home)
        .stdin(Stdio::null());
    cmd
}

fn run(cmd: &mut Command) -> Output {
    cmd.output().expect("spawning lf")
}

fn ok(out: Output) -> Output {
    assert!(
        out.status.success(),
        "expected success, got {:?}\nstdout: {}\nstderr: {}",
        out.status,
        stdout(&out),
        stderr(&out)
    );
    out
}

fn failed(out: Output) -> Output {
    assert!(
        !out.status.success(),
        "expected failure, got success\nstdout: {}",
        stdout(&out)
    );
    out
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn init(home: &Path) {
    ok(run(lf(home).args([
        "init",
        "--agent",
        "claude",
        "--no-global-skill",
    ])));
}

fn add(home: &Path, repo: &Path, id: &str, prompt: &str) {
    ok(run(lf(home).args([
        "add",
        "--repo",
        &repo.display().to_string(),
        "--no-worktree",
        "--id",
        id,
        "--prompt",
        prompt,
    ])));
}

#[test]
fn init_creates_the_home_layout() {
    let home = tempfile::tempdir().unwrap();

    let out = ok(run(lf(home.path()).args([
        "init",
        "--agent",
        "claude",
        "--no-global-skill",
    ])));
    assert!(stdout(&out).contains("wrote"));
    assert!(stdout(&out).contains("initialized"));
    assert!(home.path().join("config.toml").is_file());
    assert!(home.path().join("AGENTS.md").is_file());
    for dir in ["tasks", "schedules", "archive", "logs", "worktrees"] {
        assert!(home.path().join(dir).is_dir(), "missing {dir}/");
    }

    // Re-running keeps the user's config instead of overwriting it.
    let out = ok(run(lf(home.path()).args([
        "init",
        "--agent",
        "claude",
        "--no-global-skill",
    ])));
    assert!(stdout(&out).contains("kept existing"));
}

#[test]
fn add_ls_and_validate_roundtrip() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init(home.path());

    let out = ok(run(lf(home.path()).args([
        "add",
        "--repo",
        &repo.path().display().to_string(),
        "--no-worktree",
        "--id",
        "my-task",
        "--prompt",
        "do the thing",
    ])));
    assert!(stdout(&out).trim_end().ends_with("my-task.md"));

    let out = ok(run(lf(home.path()).arg("ls")));
    let listing = stdout(&out);
    assert!(listing.contains("my-task"));
    assert!(listing.contains("pending"));

    let out = ok(run(lf(home.path()).arg("validate")));
    let report = stdout(&out);
    assert!(report.contains("ok"));
    assert!(report.contains("1 file(s) checked, 0 with errors"));
}

#[test]
fn add_reads_the_prompt_from_stdin() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init(home.path());

    let mut child = lf(home.path())
        .args([
            "add",
            "--repo",
            &repo.path().display().to_string(),
            "--no-worktree",
            "--id",
            "from-stdin",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"  fix the thing  \n")
        .unwrap();
    let out = ok(child.wait_with_output().unwrap());
    assert!(stdout(&out).trim_end().ends_with("from-stdin.md"));

    let body = std::fs::read_to_string(home.path().join("tasks/from-stdin.md")).unwrap();
    assert!(body.trim_end().ends_with("fix the thing"));
}

#[test]
fn add_rejects_an_empty_prompt() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init(home.path());

    let out = failed(run(lf(home.path()).args([
        "add",
        "--repo",
        &repo.path().display().to_string(),
        "--no-worktree",
        "--id",
        "empty",
    ])));
    assert!(stderr(&out).contains("prompt is empty"));
}

#[test]
fn add_rejects_a_missing_repo() {
    let home = tempfile::tempdir().unwrap();
    init(home.path());

    let out = failed(run(lf(home.path()).args([
        "add",
        "--repo",
        "/no/such/repo",
        "--no-worktree",
        "--prompt",
        "x",
    ])));
    assert!(stderr(&out).contains("does not exist"));
}

#[test]
fn add_rejects_a_duplicate_id() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init(home.path());
    add(home.path(), repo.path(), "dup", "first");

    let out = failed(run(lf(home.path()).args([
        "add",
        "--repo",
        &repo.path().display().to_string(),
        "--no-worktree",
        "--id",
        "dup",
        "--prompt",
        "second",
    ])));
    assert!(stderr(&out).contains("already exists"));
}

#[test]
fn cancel_then_retry_via_cli() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init(home.path());
    add(home.path(), repo.path(), "t1", "do it");

    ok(run(lf(home.path()).args(["cancel", "t1"])));
    let archived = stdout(&ok(run(lf(home.path()).args(["ls", "--archive"]))));
    assert!(archived.contains("t1") && archived.contains("cancelled"));
    let pending = stdout(&ok(run(lf(home.path()).arg("ls"))));
    assert!(pending.contains("(none)"));

    ok(run(lf(home.path()).args(["retry", "t1"])));
    let pending = stdout(&ok(run(lf(home.path()).arg("ls"))));
    assert!(pending.contains("t1") && pending.contains("pending"));
    let archived = stdout(&ok(run(lf(home.path()).args(["ls", "--archive"]))));
    assert!(archived.contains("(none)"));
}

#[test]
fn logs_reads_a_saved_attempt_via_the_cli() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init(home.path());
    add(home.path(), repo.path(), "t1", "do it");

    let out = failed(run(lf(home.path()).args(["logs", "t1"])));
    assert!(stderr(&out).contains("hasn't started"));

    // Fake a finished attempt: bump `attempts` on the task file and write
    // the log `lf run` would have saved.
    let task_path = home.path().join("tasks/t1.md");
    let content = std::fs::read_to_string(&task_path).unwrap();
    std::fs::write(
        &task_path,
        content.replacen("---\n", "---\nattempts: 1\n", 1),
    )
    .unwrap();
    std::fs::write(home.path().join("logs/t1.1.log"), "hello from the agent\n").unwrap();

    let out = stdout(&ok(run(lf(home.path()).args(["logs", "t1"]))));
    assert_eq!(out, "hello from the agent\n");

    let out = failed(run(lf(home.path()).args(["logs", "t1", "--attempt", "2"])));
    assert!(stderr(&out).contains("1 attempt"));
}

#[test]
fn commands_that_need_existing_state_fail_clearly_before_init() {
    let home = tempfile::tempdir().unwrap();

    let out = failed(run(lf(home.path()).args(["done", "some-task"])));
    assert!(stderr(&out).contains("not initialized"));
}

#[test]
fn first_use_sets_everything_up() {
    let home = tempfile::tempdir().unwrap();

    let out = ok(run(lf(home.path()).arg("ls")));
    assert!(stderr(&out).contains("First run"));
    assert!(!stdout(&out).contains("First run"));
    assert!(home.path().join("config.toml").is_file());
    assert!(home.path().join("tasks").is_dir());

    // The second time it's just the listing.
    let out = ok(run(lf(home.path()).arg("ls")));
    assert!(!stderr(&out).contains("First run"));
}

/// A throwaway git repository.
fn git_repo() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    let status = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(repo.path())
        .status()
        .expect("running git");
    assert!(status.success());
    repo
}

#[test]
fn add_needs_no_flags_inside_a_repo() {
    let home = tempfile::tempdir().unwrap();
    let repo = git_repo();
    init(home.path());

    let out = ok(run(lf(home.path())
        .current_dir(repo.path())
        .args(["add", "Fix the login redirect"])));
    assert!(
        stdout(&out)
            .trim_end()
            .ends_with("fix-the-login-redirect.md")
    );

    let task =
        std::fs::read_to_string(home.path().join("tasks/fix-the-login-redirect.md")).unwrap();
    let repo_name = repo.path().file_name().unwrap().to_str().unwrap();
    assert!(task.contains(repo_name), "{task}");
    assert!(task.contains("Fix the login redirect"));
}

#[test]
fn add_outside_a_repo_says_what_to_do() {
    let home = tempfile::tempdir().unwrap();
    let not_a_repo = tempfile::tempdir().unwrap();
    init(home.path());

    let out = failed(run(lf(home.path())
        .current_dir(not_a_repo.path())
        .args(["add", "Fix it"])));
    let err = stderr(&out);
    assert!(err.contains("isn't inside a git repository"), "{err}");
    assert!(err.contains("--repo"), "{err}");
}

#[test]
fn the_prompt_is_positional_or_a_flag_but_not_both() {
    let home = tempfile::tempdir().unwrap();
    let repo = git_repo();
    init(home.path());

    ok(run(lf(home.path())
        .current_dir(repo.path())
        .args(["add", "--prompt", "via flag", "--id", "flagged"])));
    failed(run(lf(home.path()).current_dir(repo.path()).args([
        "add",
        "positional",
        "--prompt",
        "and flag",
    ])));
}

#[test]
fn bare_lf_explains_how_to_start() {
    let home = tempfile::tempdir().unwrap();

    let out = stdout(&ok(run(&mut lf(home.path()))));
    assert!(out.contains("lf add"), "{out}");
    assert!(out.contains("lf start"), "{out}");
    // It only explains: nothing gets created.
    assert!(!home.path().join("tasks").exists());

    init(home.path());
    let out = stdout(&ok(run(&mut lf(home.path()))));
    assert!(out.contains("runner"), "{out}");
    assert!(out.contains("lf start"), "{out}");
}

#[test]
fn ls_watch_refreshes_until_killed() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init(home.path());
    add(home.path(), repo.path(), "watched", "do it");

    let mut child = lf(home.path())
        .args(["ls", "--watch", "--interval", "20ms"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    child.kill().unwrap();
    let out = child.wait_with_output().unwrap();

    let text = stdout(&out);
    assert!(
        text.matches("watched").count() >= 2,
        "expected multiple refreshes, got:\n{text}"
    );
    assert!(text.contains("\x1B[2J\x1B[H"));
}

/// A raw HTTP request over TCP (no HTTP client dependency). `host` is the
/// Host header sent. Retries the connection until the server is up, and
/// panics if it never comes up within a few seconds.
fn http(
    port: u16,
    method: &str,
    path: &str,
    host: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> String {
    use std::io::Read as _;
    use std::net::TcpStream;
    use std::time::{Duration, Instant};

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(mut stream) => {
                let mut req = format!(
                    "{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Length: {}\r\n",
                    body.len()
                );
                for (k, v) in headers {
                    req.push_str(&format!("{k}: {v}\r\n"));
                }
                write!(stream, "{req}\r\n{body}").unwrap();
                let mut resp = String::new();
                stream.read_to_string(&mut resp).unwrap();
                return resp;
            }
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(30)),
            Err(e) => panic!("could not connect to 127.0.0.1:{port}: {e}"),
        }
    }
}

fn http_get(port: u16, path: &str) -> String {
    http(port, "GET", path, "127.0.0.1", &[], "")
}

/// A POST as the dashboard page itself would send it.
fn http_post(port: u16, token: &str, path: &str, body: &str) -> String {
    http(
        port,
        "POST",
        path,
        &format!("127.0.0.1:{port}"),
        &[
            ("X-LF-Token", token),
            ("Origin", &format!("http://127.0.0.1:{port}")),
            ("Content-Type", "application/json"),
        ],
        body,
    )
}

/// The per-run token `lf web` embeds in the dashboard page.
fn page_token(port: u16) -> String {
    let index = http_get(port, "/");
    let marker = "name=\"lf-token\" content=\"";
    let start = index.find(marker).expect("token meta tag") + marker.len();
    index[start..start + 32].to_string()
}

#[test]
fn web_actions_mutate_tasks_through_the_api() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init(home.path());
    add(home.path(), repo.path(), "w1", "do it");

    let port = 18338u16;
    let mut child = lf(home.path())
        .args(["web", "--port", &port.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let token = page_token(port); // also waits for the server to come up

    let cancel = http_post(port, &token, "/api/tasks/w1/cancel", "");
    assert!(cancel.starts_with("HTTP/1.1 200"), "{cancel}");
    assert!(cancel.contains("\"ok\":true"));

    let state = http_get(port, "/api/state");
    assert!(state.contains("\"cancelled\":1"));
    assert!(state.contains("\"pending\":0"));

    let retry = http_post(port, &token, "/api/tasks/w1/retry", "");
    assert!(retry.starts_with("HTTP/1.1 200"), "{retry}");
    let state = http_get(port, "/api/state");
    assert!(state.contains("\"pending\":1"));

    let bad = http_post(port, &token, "/api/tasks/w1/fail", "");
    assert!(bad.starts_with("HTTP/1.1 400"), "{bad}");
    assert!(bad.contains("\"error\""));

    let logs = http_get(port, "/api/tasks/w1/logs");
    assert!(logs.starts_with("HTTP/1.1 404"), "{logs}");
    assert!(logs.contains("hasn't started"));

    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn web_refuses_cross_site_and_rebound_requests() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init(home.path());
    add(home.path(), repo.path(), "w1", "do it");

    let port = 18339u16;
    let mut child = lf(home.path())
        .args(["web", "--port", &port.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let token = page_token(port);
    let path = "/api/tasks/w1/cancel";
    let own_host = format!("127.0.0.1:{port}");

    // A page on another site: no token it could have read.
    let forged = http(
        port,
        "POST",
        path,
        &own_host,
        &[("Origin", "http://evil.example")],
        "",
    );
    assert!(forged.starts_with("HTTP/1.1 403"), "{forged}");
    // Right token but a foreign Origin.
    let origin = http(
        port,
        "POST",
        path,
        &own_host,
        &[("X-LF-Token", &token), ("Origin", "http://evil.example")],
        "",
    );
    assert!(origin.starts_with("HTTP/1.1 403"), "{origin}");
    // Wrong token.
    let wrong = http(port, "POST", path, &own_host, &[("X-LF-Token", "nope")], "");
    assert!(wrong.starts_with("HTTP/1.1 403"), "{wrong}");
    // DNS rebinding: the browser sends the attacker's hostname.
    let rebound = http(port, "GET", "/api/state", "evil.example", &[], "");
    assert!(rebound.starts_with("HTTP/1.1 403"), "{rebound}");
    let rebound_page = http(port, "GET", "/", &format!("evil.example:{port}"), &[], "");
    assert!(rebound_page.starts_with("HTTP/1.1 403"), "{rebound_page}");
    assert!(!rebound_page.contains(&token));

    // None of that touched the task.
    let state = http_get(port, "/api/state");
    assert!(state.contains("\"pending\":1"), "{state}");
    // The real thing still works.
    let ok = http_post(port, &token, path, "");
    assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");

    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn web_survives_a_corrupt_task_file() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init(home.path());
    add(home.path(), repo.path(), "w1", "do it");
    std::fs::write(home.path().join("tasks/broken.md"), "not a task").unwrap();

    let port = 18340u16;
    let mut child = lf(home.path())
        .args(["web", "--port", &port.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    http_get(port, "/");

    let state = http_get(port, "/api/state");
    assert!(state.starts_with("HTTP/1.1 200"), "{state}");
    assert!(state.contains("\"w1\""));
    assert!(state.contains("broken.md"), "reported as invalid: {state}");
    assert!(state.contains("\"invalid\":1"));

    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn web_serves_the_dashboard_and_state() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init(home.path());
    add(home.path(), repo.path(), "w1", "do it");

    let port = 18337u16;
    let mut child = lf(home.path())
        .args(["web", "--port", &port.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let index = http_get(port, "/");
    assert!(index.starts_with("HTTP/1.1 200"));
    assert!(index.contains("lf web"));

    let state = http_get(port, "/api/state");
    assert!(state.starts_with("HTTP/1.1 200"));
    assert!(state.contains("\"w1\""));
    assert!(state.contains("\"pending\":1"));

    let missing = http_get(port, "/nope");
    assert!(missing.starts_with("HTTP/1.1 404"));

    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn web_new_task_form_queues_a_task() {
    let home = tempfile::tempdir().unwrap();
    let repo = git_repo();
    init(home.path());

    let port = 18341u16;
    let mut child = lf(home.path())
        .args(["web", "--no-open", "--port", &port.to_string()])
        .current_dir(repo.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let token = page_token(port);

    // Started inside a repo, so the form suggests it.
    let repo_name = repo.path().file_name().unwrap().to_str().unwrap();
    let state = http_get(port, "/api/state");
    assert!(state.contains("\"form\""), "{state}");
    assert!(state.contains(repo_name), "{state}");

    let form = serde_json::json!({
        "prompt": "Tidy up the README\n\nMore detail here.",
        "repo": repo.path(),
        "start": "1h",
        "on_finish": "commit",
        "agent": "claude",
    })
    .to_string();
    let made = http_post(port, &token, "/api/tasks", &form);
    assert!(made.starts_with("HTTP/1.1 200"), "{made}");
    assert!(made.contains("\"id\":\"tidy-up-the-readme\""), "{made}");

    let task = std::fs::read_to_string(home.path().join("tasks/tidy-up-the-readme.md")).unwrap();
    assert!(task.contains("on_finish: commit"), "{task}");
    assert!(task.contains("created_by: web"), "{task}");
    assert!(task.contains("scheduled_at"), "{task}");
    assert!(task.contains("More detail here."), "{task}");
    assert!(http_get(port, "/api/state").contains("\"pending\":1"));

    // Mistakes come back as a message the form can show, not a stack trace.
    for (bad, expect) in [
        (
            r#"{"prompt":"  ","repo":"/tmp","agent":"claude"}"#,
            "prompt is empty",
        ),
        (
            r#"{"prompt":"x","repo":"","agent":"claude"}"#,
            "choose the repository",
        ),
        (
            r#"{"prompt":"x","repo":"/no/such/dir","agent":"claude"}"#,
            "does not exist",
        ),
        (
            r#"{"prompt":"x","repo":"/tmp","start":"soonish"}"#,
            "can't start after",
        ),
        (r#"not json"#, "couldn't read the form"),
    ] {
        let out = http_post(port, &token, "/api/tasks", bad);
        assert!(out.starts_with("HTTP/1.1 400"), "{bad}: {out}");
        assert!(out.contains(expect), "{bad}: {out}");
        assert!(!out.contains("invalid task"), "{bad}: {out}");
    }

    // Like every POST, it needs the page's token.
    let out = http(
        port,
        "POST",
        "/api/tasks",
        &format!("127.0.0.1:{port}"),
        &[("Content-Type", "application/json")],
        &form,
    );
    assert!(out.starts_with("HTTP/1.1 403"), "{out}");

    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn start_runs_the_runner_and_the_dashboard_together() {
    let home = tempfile::tempdir().unwrap();
    init(home.path());

    let port = 18342u16;
    let mut child = lf(home.path())
        .args(["start", "--no-open", "--port", &port.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    page_token(port); // waits for the dashboard

    // The runner comes up a moment after the page does.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let status = loop {
        let status = stdout(&ok(run(lf(home.path()).arg("status"))));
        if status.contains("running (pid") || std::time::Instant::now() > deadline {
            break status;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert!(status.contains("running (pid"), "{status}");
    assert!(http_get(port, "/api/state").contains("\"alive\":true"));

    // Ctrl-C reaches both in a terminal; here, stop each by hand.
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.path().join("runner.json")).unwrap())
            .unwrap();
    let runner_pid = state["pid"].as_u64().unwrap().to_string();
    child.kill().unwrap();
    child.wait().unwrap();
    Command::new("kill").arg(runner_pid).status().unwrap();
}
