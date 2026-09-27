# loompa-forge home

This folder is the queue of coding-agent tasks run by `lf` (loompa-forge).
Users open an agent here and ask things like "add a task to fix the login
redirect in ~/Projects/myapp tonight" or "every Monday, update deps in myapp".

To create, schedule, edit or cancel tasks, follow the `lf-tasks` skill:
`.agents/skills/lf-tasks/SKILL.md`. Read it even if your agent doesn't load
skills automatically. The file format is in `FORMAT.md`.

Layout:

- `tasks/`: the queue, one `<id>.md` per task
- `schedules/`: recurring task templates, one `<id>.md` each
- `archive/`: finished tasks; read-only history
- `logs/`: output of each run; read-only
- `worktrees/`: git checkouts where tasks run; not yours to edit
- `config.toml`: runner settings, defaults, and the agents tasks can use

Useful commands: `lf ls`, `lf ls --schedules`, `lf ls --archive`,
`lf ls --watch`, `lf validate`.
