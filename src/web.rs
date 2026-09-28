//! `lf web`: a read-only, Sidekiq-style dashboard over the queue.
//!
//! Serves one HTML shell and a `/api/state` JSON endpoint the page polls.
//! No mutating routes: acting on a task still goes through `lf` itself.

use std::cmp::Reverse;

use anyhow::{Context, Result};
use serde::Serialize;
use tiny_http::{Header, Response, Server};

use crate::cmd::md_files;
use crate::config::Config;
use crate::home::{Home, contract_tilde};
use crate::schedule::Schedule;
use crate::task::{Status, Task, now};

#[derive(clap::Args)]
pub struct WebArgs {
    /// Port to listen on.
    #[arg(long, default_value_t = 7433)]
    port: u16,
}

const INDEX_HTML: &str = include_str!("web_dashboard.html");

pub fn serve(home: &Home, args: WebArgs) -> Result<()> {
    home.ensure_initialized()?;
    let addr = format!("127.0.0.1:{}", args.port);
    let server = Server::http(&addr)
        .map_err(|e| anyhow::anyhow!("binding {addr}: {e}"))
        .with_context(|| format!("starting `lf web` on {addr}"))?;
    println!("lf web on http://{addr}  (Ctrl-C to stop)");

    for request in server.incoming_requests() {
        let (status, content_type, body) = match request.url() {
            "/" => (200, "text/html; charset=utf-8", INDEX_HTML.to_string()),
            "/api/state" => match state_json(home) {
                Ok(json) => (200, "application/json", json),
                Err(e) => (500, "text/plain", format!("error: {e:#}")),
            },
            _ => (404, "text/plain", "not found".to_string()),
        };
        let header = Header::from_bytes(&b"Content-Type"[..], content_type.as_bytes())
            .expect("static content-type is valid ASCII");
        let response = Response::from_string(body)
            .with_status_code(status)
            .with_header(header);
        let _ = request.respond(response);
    }
    Ok(())
}

#[derive(Serialize)]
struct Row {
    id: String,
    status: &'static str,
    when: String,
    repo: String,
    branch: String,
    agent: String,
}

#[derive(Serialize)]
struct ScheduleRow {
    id: String,
    enabled: bool,
    cron: String,
    next: String,
    repo: String,
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
}

fn state_json(home: &Home) -> Result<String> {
    let config = Config::load(&home.config_path())?;
    let mut counts = Counts::default();

    let mut queue_tasks = Vec::new();
    let mut running_tasks = Vec::new();
    for path in md_files(&home.tasks())? {
        let t = Task::load(&path)?;
        if t.status == Status::Running {
            running_tasks.push(t);
        } else {
            queue_tasks.push(t);
        }
    }
    queue_tasks.sort_by_key(|t| (t.scheduled_at.or(t.created_at), t.id.clone()));
    running_tasks.sort_by_key(|t| (t.started_at, t.id.clone()));
    counts.pending = queue_tasks.len();
    counts.running = running_tasks.len();

    let mut archive_tasks = Vec::new();
    for path in md_files(&home.archive())? {
        let t = Task::load(&path)?;
        match t.status {
            Status::Done => counts.done += 1,
            Status::Failed => counts.failed += 1,
            Status::Cancelled => counts.cancelled += 1,
            Status::NeedsReview => counts.needs_review += 1,
            Status::Pending | Status::Running => {}
        }
        archive_tasks.push(t);
    }
    archive_tasks.sort_by_key(|t| Reverse(t.finished_at));

    let mut schedules = Vec::new();
    for path in md_files(&home.schedules())? {
        let s = Schedule::load(&path)?;
        let next = match s.next_after(chrono::Local::now()) {
            Ok(t) if s.enabled => fmt_time(t.fixed_offset()),
            Ok(_) => "-".into(),
            Err(_) => "invalid".into(),
        };
        schedules.push(ScheduleRow {
            id: s.id.clone(),
            enabled: s.enabled,
            cron: s.cron.clone(),
            next,
            repo: contract_tilde(&s.spec.resolve(&config.defaults).repo),
        });
    }
    counts.schedules = schedules.len();

    let state = State {
        home: contract_tilde(home.root()),
        generated_at: fmt_time(now()),
        counts,
        queue: queue_tasks.iter().map(|t| to_row(t, &config)).collect(),
        running: running_tasks.iter().map(|t| to_row(t, &config)).collect(),
        archive: archive_tasks.iter().map(|t| to_row(t, &config)).collect(),
        schedules,
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
    }
}

fn fmt_time(t: chrono::DateTime<chrono::FixedOffset>) -> String {
    t.with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}
