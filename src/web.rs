//! `lf web`: a Sidekiq-style dashboard over the queue, with actions.
//!
//! Serves one HTML shell, a `/api/state` JSON endpoint the page polls, a
//! `/api/tasks/<id>/logs` endpoint, and `/api/tasks/<id>/{done,fail,cancel,retry}`
//! POST routes that call straight into the same code `lf done|fail|cancel|retry`
//! use. Only binds to 127.0.0.1, but a web page you visit can still send
//! requests there, so every request must carry a loopback `Host` (defeats DNS
//! rebinding), and POSTs a per-run token that only the dashboard page itself
//! can read (defeats cross-site forgery) and a loopback `Origin`, if any.

use std::cmp::Reverse;
use std::io::Read;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::cmd::md_files;
use crate::config::Config;
use crate::control;
use crate::home::{Home, contract_tilde};
use crate::run::RunnerState;
use crate::schedule::Schedule;
use crate::task::{Status, Task, fmt_cost, fmt_tokens, now};

#[derive(clap::Args)]
pub struct WebArgs {
    /// Port to listen on.
    #[arg(long, default_value_t = 7433)]
    port: u16,
}

const INDEX_HTML: &str = include_str!("web_dashboard.html");
const ACTIONS: [&str; 4] = ["done", "fail", "cancel", "retry"];
/// Requests are handled on this many threads, so one slow one (say, an
/// action waiting for the lock) doesn't stall the dashboard's polling.
const WORKERS: usize = 4;
/// Largest request body read; the only body anyone sends is a short reason.
const MAX_BODY_BYTES: u64 = 16 * 1024;
const TOKEN_HEADER: &str = "X-LF-Token";

struct Ctx<'a> {
    home: &'a Home,
    token: String,
    index: String,
}

struct Reply {
    status: u16,
    content_type: &'static str,
    body: String,
}

fn reply(status: u16, content_type: &'static str, body: impl Into<String>) -> Reply {
    Reply {
        status,
        content_type,
        body: body.into(),
    }
}

fn json_reply(status: u16, body: String) -> Reply {
    reply(status, "application/json", body)
}

pub fn serve(home: &Home, args: WebArgs) -> Result<()> {
    home.ensure_initialized()?;
    let addr = format!("127.0.0.1:{}", args.port);
    let server = Server::http(&addr)
        .map_err(|e| anyhow::anyhow!("binding {addr}: {e}"))
        .with_context(|| format!("starting `lf web` on {addr}"))?;
    let token = random_token()?;
    let ctx = Ctx {
        home,
        index: INDEX_HTML.replace("__LF_TOKEN__", &token),
        token,
    };
    println!("lf web on http://{addr}  (Ctrl-C to stop)");

    std::thread::scope(|scope| {
        for _ in 0..WORKERS {
            scope.spawn(|| {
                for request in server.incoming_requests() {
                    handle(&ctx, request);
                }
            });
        }
    });
    Ok(())
}

fn handle(ctx: &Ctx, mut request: Request) {
    let url = request.url().to_string();
    let header = |name: &'static str| {
        request
            .headers()
            .iter()
            .find(|h| h.field.equiv(name))
            .map(|h| h.value.as_str().to_string())
    };
    let host = header("Host");
    let origin = header("Origin");
    let token = header(TOKEN_HEADER);
    let is_post = *request.method() == Method::Post;

    let result = if !host.as_deref().is_some_and(is_loopback_authority) {
        reply(403, "text/plain", "forbidden: not a loopback host")
    } else if is_post
        && !(origin.as_deref().is_none_or(is_loopback_origin)
            && token.as_deref().is_some_and(|t| t == ctx.token))
    {
        reply(403, "text/plain", "forbidden: missing or wrong token")
    } else {
        let mut body = String::new();
        if is_post {
            let _ = request
                .as_reader()
                .take(MAX_BODY_BYTES)
                .read_to_string(&mut body);
        }
        route(ctx, request.method(), &url, &body)
    };
    let is_index = result.content_type.starts_with("text/html");
    let mut response = Response::from_string(result.body).with_status_code(result.status);
    let mut add = |name: &str, value: &str| {
        if let Ok(h) = Header::from_bytes(name.as_bytes(), value.as_bytes()) {
            response.add_header(h);
        }
    };
    add("Content-Type", result.content_type);
    add("Cache-Control", "no-store");
    add("X-Content-Type-Options", "nosniff");
    if is_index {
        add("X-Frame-Options", "DENY");
        add(
            "Content-Security-Policy",
            "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
             connect-src 'self'; frame-ancestors 'none'",
        );
    }
    let _ = request.respond(response);
}

fn route(ctx: &Ctx, method: &Method, url: &str, body: &str) -> Reply {
    let home = ctx.home;
    let path = url.split('?').next().unwrap_or("");
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match method {
        Method::Get => match segments.as_slice() {
            [] => reply(200, "text/html; charset=utf-8", ctx.index.clone()),
            ["api", "state"] => match state_json(home) {
                Ok(json) => json_reply(200, json),
                Err(e) => json_reply(500, json_err(&e)),
            },
            ["api", "tasks", id, "logs"] => {
                let attempt = query_param(url, "attempt").and_then(|v| v.parse().ok());
                match control::logs(home, id, attempt) {
                    Ok(log) => json_reply(200, json_log(&log)),
                    Err(e) => json_reply(404, json_err(&e)),
                }
            }
            _ => reply(404, "text/plain", "not found"),
        },
        Method::Post => match segments.as_slice() {
            ["api", "tasks", id, action] if ACTIONS.contains(action) => {
                match perform_action(home, id, action, body) {
                    Ok(()) => json_reply(200, "{\"ok\":true}".into()),
                    Err(e) => json_reply(400, json_err(&e)),
                }
            }
            _ => reply(404, "text/plain", "not found"),
        },
        _ => reply(404, "text/plain", "not found"),
    }
}

/// `host`, `host:port` or `[::1]:port`: is the host part a loopback name?
fn is_loopback_authority(authority: &str) -> bool {
    let host = match authority.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(""),
        None => authority.split(':').next().unwrap_or(""),
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

/// `http://127.0.0.1:7433` and the like. (`null`, sent by sandboxed or
/// cross-site contexts, is not.)
fn is_loopback_origin(origin: &str) -> bool {
    origin
        .strip_prefix("http://")
        .is_some_and(is_loopback_authority)
}

/// 128 random bits from the OS, hex-encoded.
fn random_token() -> Result<String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .context("reading /dev/urandom for the session token")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[derive(Deserialize, Default)]
struct ActionBody {
    reason: Option<String>,
}

/// Runs one of the `ACTIONS` against a task, the same way `lf <action> <id>`
/// would. `body` is the POST body, parsed for `fail`'s optional `reason`.
fn perform_action(home: &Home, id: &str, action: &str, body: &str) -> Result<()> {
    match action {
        "done" => control::done(home, Some(id.to_string())),
        "fail" => {
            let reason = serde_json::from_str::<ActionBody>(body)
                .ok()
                .and_then(|b| b.reason)
                .filter(|r| !r.trim().is_empty());
            control::fail(home, Some(id.to_string()), reason)
        }
        "cancel" => control::cancel(home, Some(id.to_string())),
        "retry" => control::retry(home, id),
        _ => unreachable!("route only matches ACTIONS"),
    }
}

fn json_err(e: &anyhow::Error) -> String {
    serde_json::to_string(&serde_json::json!({ "error": format!("{e:#}") }))
        .unwrap_or_else(|_| "{\"error\":\"internal error\"}".to_string())
}

fn json_log(log: &str) -> String {
    serde_json::to_string(&serde_json::json!({ "log": log }))
        .unwrap_or_else(|_| "{\"log\":\"\"}".to_string())
}

fn query_param<'a>(url: &'a str, key: &str) -> Option<&'a str> {
    let query = url.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then_some(v)
    })
}

#[derive(Serialize)]
struct Row {
    id: String,
    status: &'static str,
    when: String,
    repo: String,
    branch: String,
    agent: String,
    tokens: String,
    cost: String,
}

#[derive(Serialize)]
struct ScheduleRow {
    id: String,
    enabled: bool,
    cron: String,
    next: String,
    repo: String,
}

/// A task, schedule or config file that couldn't be read.
#[derive(Serialize)]
struct InvalidRow {
    file: String,
    error: String,
}

#[derive(Serialize)]
struct RunnerRow {
    alive: bool,
    last_tick: Option<String>,
}

#[derive(Serialize, Default)]
struct Counts {
    pending: usize,
    running: usize,
    done: usize,
    failed: usize,
    cancelled: usize,
    needs_review: usize,
    schedules: usize,
    invalid: usize,
}

#[derive(Serialize)]
struct State {
    home: String,
    generated_at: String,
    counts: Counts,
    queue: Vec<Row>,
    running: Vec<Row>,
    archive: Vec<Row>,
    schedules: Vec<ScheduleRow>,
    invalid: Vec<InvalidRow>,
    runner: RunnerRow,
}

/// Loads every file in `paths`, setting aside the ones that don't parse
/// instead of failing the whole dashboard over one bad file.
fn load_all<T>(
    paths: Vec<std::path::PathBuf>,
    load: impl Fn(&std::path::Path) -> Result<T>,
    invalid: &mut Vec<InvalidRow>,
) -> Vec<T> {
    let mut ok = Vec::new();
    for path in paths {
        match load(&path) {
            Ok(v) => ok.push(v),
            Err(e) => invalid.push(InvalidRow {
                file: contract_tilde(&path),
                error: format!("{e:#}"),
            }),
        }
    }
    ok
}

fn state_json(home: &Home) -> Result<String> {
    let mut invalid = Vec::new();
    let config = Config::load(&home.config_path()).unwrap_or_else(|e| {
        invalid.push(InvalidRow {
            file: contract_tilde(&home.config_path()),
            error: format!("{e:#} (showing built-in defaults)"),
        });
        Config::default()
    });
    let mut counts = Counts::default();

    let (mut running_tasks, mut queue_tasks): (Vec<_>, Vec<_>) =
        load_all(md_files(&home.tasks())?, Task::load, &mut invalid)
            .into_iter()
            .partition(|t| t.status == Status::Running);
    queue_tasks.sort_by_key(|t| (t.scheduled_at.or(t.created_at), t.id.clone()));
    running_tasks.sort_by_key(|t| (t.started_at, t.id.clone()));
    counts.pending = queue_tasks.len();
    counts.running = running_tasks.len();

    let mut archive_tasks = load_all(md_files(&home.archive())?, Task::load, &mut invalid);
    for t in &archive_tasks {
        match t.status {
            Status::Done => counts.done += 1,
            Status::Failed => counts.failed += 1,
            Status::Cancelled => counts.cancelled += 1,
            Status::NeedsReview => counts.needs_review += 1,
            Status::Pending | Status::Running => {}
        }
    }
    archive_tasks.sort_by_key(|t| Reverse(t.finished_at));

    let schedules: Vec<ScheduleRow> =
        load_all(md_files(&home.schedules())?, Schedule::load, &mut invalid)
            .into_iter()
            .map(|s| {
                let next = match s.next_after(chrono::Local::now()) {
                    Ok(t) if s.enabled => fmt_time(t.fixed_offset()),
                    Ok(_) => "-".into(),
                    Err(_) => "invalid".into(),
                };
                ScheduleRow {
                    id: s.id.clone(),
                    enabled: s.enabled,
                    cron: s.cron.clone(),
                    next,
                    repo: contract_tilde(&s.spec.resolve(&config.defaults).repo),
                }
            })
            .collect();
    counts.schedules = schedules.len();
    counts.invalid = invalid.len();

    let state = State {
        home: contract_tilde(home.root()),
        generated_at: fmt_time(now()),
        counts,
        queue: queue_tasks.iter().map(|t| to_row(t, &config)).collect(),
        running: running_tasks.iter().map(|t| to_row(t, &config)).collect(),
        archive: archive_tasks.iter().map(|t| to_row(t, &config)).collect(),
        schedules,
        invalid,
        runner: RunnerRow {
            alive: home.runner_alive(),
            last_tick: RunnerState::load(home)
                .and_then(|s| s.last_tick_at)
                .map(fmt_time),
        },
    };
    Ok(serde_json::to_string(&state)?)
}

fn to_row(t: &Task, config: &Config) -> Row {
    let eff = t.spec.resolve(&config.defaults);
    let when = match (t.finished_at, t.started_at, t.scheduled_at) {
        (Some(at), _, _) | (None, Some(at), _) | (None, None, Some(at)) => fmt_time(at),
        _ => "asap".into(),
    };
    Row {
        id: t.id.clone(),
        status: t.status.as_str(),
        when,
        repo: contract_tilde(&eff.repo),
        branch: t.effective_branch(config).unwrap_or_else(|| "-".into()),
        agent: match (eff.agent, eff.model) {
            (Some(a), Some(m)) => format!("{a}/{m}"),
            (Some(a), None) => a,
            (None, _) => "-".into(),
        },
        tokens: fmt_tokens(t.tokens_in, t.tokens_out),
        cost: fmt_cost(t.cost_usd),
    }
}

fn fmt_time(t: chrono::DateTime<chrono::FixedOffset>) -> String {
    t.with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_hosts_and_origins_pass() {
        for ok in ["127.0.0.1", "127.0.0.1:7433", "localhost:80", "[::1]:7433"] {
            assert!(is_loopback_authority(ok), "{ok}");
        }
        for bad in [
            "evil.com",
            "evil.com:7433",
            "127.0.0.1.evil.com",
            "",
            "10.0.0.5:7433",
        ] {
            assert!(!is_loopback_authority(bad), "{bad}");
        }
        assert!(is_loopback_origin("http://localhost:7433"));
        assert!(!is_loopback_origin("https://localhost:7433"));
        assert!(!is_loopback_origin("http://evil.com"));
        assert!(!is_loopback_origin("null"));
    }

    #[test]
    fn tokens_are_random_hex() {
        let (a, b) = (random_token().unwrap(), random_token().unwrap());
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn query_param_finds_and_misses() {
        assert_eq!(
            query_param("/api/tasks/x/logs?attempt=2", "attempt"),
            Some("2")
        );
        assert_eq!(query_param("/api/tasks/x/logs?a=1&b=2", "b"), Some("2"));
        assert_eq!(query_param("/api/tasks/x/logs", "attempt"), None);
        assert_eq!(query_param("/api/tasks/x/logs?a=1", "attempt"), None);
    }

    #[test]
    fn action_body_parses_reason_or_defaults() {
        let with: ActionBody = serde_json::from_str(r#"{"reason":"bad approach"}"#).unwrap();
        assert_eq!(with.reason.as_deref(), Some("bad approach"));

        let empty: ActionBody = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.reason, None);
    }

    #[test]
    fn json_err_and_json_log_produce_valid_json() {
        let err = json_err(&anyhow::anyhow!("boom"));
        let v: serde_json::Value = serde_json::from_str(&err).unwrap();
        assert_eq!(v["error"], "boom");

        let log = json_log("line one\nline two");
        let v: serde_json::Value = serde_json::from_str(&log).unwrap();
        assert_eq!(v["log"], "line one\nline two");
    }
}
