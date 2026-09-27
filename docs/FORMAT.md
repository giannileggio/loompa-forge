# File formats

All state lives under one folder: `$LF_HOME`, default `~/.loompa-forge`.

```
~/.loompa-forge/
  config.toml    user preferences and defaults
  tasks/         queue: one <id>.md per task
  schedules/     recurring task templates: one <id>.md per schedule
  archive/       finished tasks, moved out of tasks/
  logs/          <id>.<attempt>.log (pane output) and .exit (exit status) per run
  worktrees/     <id>/: the git worktree a task runs in (kept until `lf clean`)
  AGENTS.md      guide for an agent opened in this folder (CLAUDE.md imports it)
  FORMAT.md      this document
  .claude/skills/lf-tasks/SKILL.md   how an agent creates and edits tasks
```

Open your coding agent in this folder and ask it to "add a task to..." or
"every night, ...": the `lf-tasks` skill tells it how. `lf init` keeps your
`AGENTS.md` and `CLAUDE.md` if they exist, and rewrites `FORMAT.md` and the
skill so they match the installed `lf`.

Tasks and schedules are Markdown files with a YAML frontmatter block. The
**body is the prompt** given to the agent. Unknown frontmatter keys are
errors, so typos don't silently fall back to defaults. Run `lf validate` after
editing any file.

## Task — `tasks/<id>.md`

```markdown
---
id: fix-login-redirect
repo: ~/Projects/myapp
branch: fix/login-redirect
on_finish: pr
retries: 3
scheduled_at: 2026-09-28T02:00:00+02:00
---

Fix the redirect loop after login when the session cookie has expired.
Add a regression test.
```

### Fields you write

| Field          | Required | Default (config `[defaults]`) | Meaning |
|----------------|----------|-------------------------------|---------|
| `id`           | yes      | —                 | `a-z`, `0-9`, `-`, max 64 chars. Must equal the file name without `.md`. |
| `repo`         | yes      | —                 | Folder the task runs in: absolute, or starting with `~`. |
| `branch`       | no       | `lf/<id>` if `worktree` | Branch to work on. |
| `worktree`     | no       | `true`            | Run in a dedicated git worktree for the branch. |
| `agent`        | no       | `claude`          | Name of an agent defined in `config.toml` `[agents.*]`. |
| `model`        | no       | `claude-sonnet-5` | Passed to the agent as `{model}`. |
| `mode`         | no       | `headless`        | `headless`: non-interactive, exit code decides the outcome. `interactive`: a live session that finishes with `lf done` / `lf fail`. |
| `on_finish`    | no       | `none`            | `none` \| `commit` \| `push` \| `pr` |
| `retries`      | no       | `1`               | Extra attempts after a failure. |
| `retry_delay`  | no       | `5m`              | Wait between attempts (`30s`, `5m`, `1h`...). |
| `timeout`      | no       | `2h`              | Per attempt. |
| `scheduled_at` | no       | now               | Don't start before this RFC 3339 time. |
| `created_by`   | no       | —                 | Free text: `cli`, `agent`, `schedule:<id>`... |
| `created_at`   | no       | —                 | RFC 3339; set by `lf add`. |

### Fields loompa-forge writes

Don't edit these by hand. Use `lf` commands to change the status.

| Field         | Meaning |
|---------------|---------|
| `status`      | `pending` → `running` → `done` \| `failed` \| `cancelled` \| `needs_review` |
| `attempts`    | Attempts started so far. |
| `started_at`, `finished_at` | RFC 3339. |
| `exit_code`   | Headless mode only. |
| `tmux_window` | Where the task is or was running. |
| `error`       | Last failure reason. |

`needs_review` means an interactive session ended without signalling
`lf done` or `lf fail`.

### Changing the status by hand

| Command | Works on | Effect |
|---------|----------|--------|
| `lf done [id]`   | running, needs_review, failed | Ends the session if running, runs `on_finish`, archives as `done` (or `failed` if `on_finish` fails). |
| `lf fail [id] [-r reason]` | running, needs_review | Ends the session if running, archives as `failed` without retrying. |
| `lf cancel [id]` | pending, running | Ends the session if running, archives as `cancelled`. `on_finish` doesn't run. |
| `lf retry <id>`  | failed, cancelled, needs_review | Moves it back to `tasks/` as `pending`, to start as soon as possible in the same worktree. |
| `lf attach [id]` | running | Opens the task's tmux window, or the whole session without an id. |

`id` defaults to `$LF_TASK_ID`, so inside a task's window a plain `lf done`
works: the user can type `! lf done` in the agent, or the agent can run it.
Ending a session saves its output to the attempt's log first.

`lf clean` removes `worktrees/<id>` of archived `done` tasks (`--all`: any
archived task, and worktrees with no task file; `--older-than 7d`: only
tasks finished that long ago; `-n`: dry run). It never removes a worktree
with uncommitted changes, and never deletes branches.

`attempts` keeps counting across `lf retry`, so earlier logs are kept. That
also means automatic retries aren't renewed: if the retried attempt fails,
the task is `failed` unless `retries` still covers the new count.

A failed attempt (non-zero exit, signal, timeout, or a start error such as a
worktree that can't be created) goes back to `pending` with `scheduled_at`
pushed out by `retry_delay` while retries remain; then it's `failed`. A task
that doesn't pass `lf validate` when its turn comes is failed without
running. `timeout` applies to headless tasks only. If `on_finish` fails after
the agent succeeded, the task is `failed` without a retry; its worktree
keeps the work.

The agent runs with `LF_HOME` and `LF_TASK_ID` set.

## Schedule — `schedules/<id>.md`

A schedule has the same fields as a task (except the ones loompa-forge writes
on tasks), plus:

| Field              | Required | Meaning |
|--------------------|----------|---------|
| `cron`             | yes      | 5-field cron expression in local time, e.g. `"0 2 * * *"`. Quote it. |
| `enabled`          | no       | Default `true`. |
| `last_enqueued_at` | —        | Written by loompa-forge. |

```markdown
---
id: nightly-deps
cron: "0 2 * * *"
repo: ~/Projects/myapp
on_finish: pr
---

Update dependencies, run the test suite, and fix any breakage.
```

Each time the schedule fires, `lf run` creates a task `<id>-<YYYYMMDD-HHMM>`
in `tasks/` with the schedule's fields and prompt and sets
`created_by: schedule:<id>`. The first time `lf run` sees a schedule it only
records `last_enqueued_at`; it fires from then on. Firings missed while
`lf run` wasn't running collapse into one task, not one per missed slot.

## Config — `config.toml`

`lf init` writes a `config.toml` with every default, commented. Agents are argv
lists with `{prompt}`, `{model}` and `{id}` placeholders. No shell is involved,
so prompts need no quoting.
