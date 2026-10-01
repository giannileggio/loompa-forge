use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::spec::{Mode, OnFinish};

/// Written by `lf init`, with `{agent}` replaced by [`default_config_toml`].
/// Must stay in sync with `Config::default()` (enforced by a test).
const CONFIG_TEMPLATE: &str = r#"# loompa-forge configuration.
# Every value below is the built-in default; delete what you don't change.

[runner]
max_parallel = 3            # tasks running at once, overall
max_parallel_per_repo = 1   # tasks running at once in the same repo
tmux_session = "loompa"     # one session, one window per task
poll_interval = "30s"       # how often `lf run` rescans the folders
# daily_budget_usd = 20.0   # stop starting tasks once today's reported cost reaches this

# Fallbacks for any field a task or schedule doesn't set.
[defaults]
{agent}
mode = "headless"           # headless | interactive
worktree = true             # run each task in its own git worktree
on_finish = "none"          # none | commit | push | pr
retries = 1                 # extra attempts after a failure
retry_delay = "5m"
timeout = "2h"

# How to launch each agent, as argv lists (no shell involved).
# Placeholders: {prompt} {id}. `model_args` is appended only when a task (or
# the agent's `model = "..."`) sets a model, filling in {model}; without one
# the agent uses its own default. Add [agents.<name>] tables for any other
# CLI agent; these presets stay available unless you redefine them.
#
# Headless runs can't answer permission prompts: add each agent's
# auto-approve flags (e.g. claude "--permission-mode", "acceptEdits";
# codex "--full-auto"; gemini "--yolo") to suit your trust level.
#
# claude's headless preset asks for `--output-format json`: lf parses that
# final object for token/cost usage (see `tokens_in`/`tokens_out`/`cost_usd`
# in FORMAT.md). Drop the flag and lf just won't record usage for it.
[agents.claude]
headless = ["claude", "-p", "{prompt}", "--output-format", "json"]
interactive = ["claude", "{prompt}"]
model_args = ["--model", "{model}"]

[agents.codex]
headless = ["codex", "exec", "{prompt}"]
interactive = ["codex", "{prompt}"]
model_args = ["--model", "{model}"]

[agents.gemini]
headless = ["gemini", "--prompt", "{prompt}"]
interactive = ["gemini", "--prompt-interactive", "{prompt}"]
model_args = ["--model", "{model}"]

[agents.opencode]
headless = ["opencode", "run", "{prompt}"]
interactive = ["opencode", "--prompt", "{prompt}"]
model_args = ["--model", "{model}"]

[agents.pi]
headless = ["pi", "-p", "{prompt}"]
interactive = ["pi", "{prompt}"]
model_args = ["--model", "{model}"]
"#;

/// The config `lf init` writes, with `agent` as the default agent (left
/// commented out if none was chosen).
pub fn default_config_toml(agent: Option<&str>) -> String {
    let line = match agent {
        Some(a) => format!("{:<28}# any [agents.*] below", format!("agent = \"{a}\"")),
        None => format!(
            "{:<28}# any [agents.*] below; tasks without one fail",
            "# agent = \"<name>\""
        ),
    };
    CONFIG_TEMPLATE.replace("{agent}", &line)
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub runner: Runner,
    pub defaults: Defaults,
    pub agents: BTreeMap<String, Agent>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Runner {
    pub max_parallel: usize,
    pub max_parallel_per_repo: usize,
    pub tmux_session: String,
    #[serde(with = "humantime_serde")]
    pub poll_interval: Duration,
    /// Once the cost agents reported for tasks run today reaches this,
    /// `lf run` starts no more until tomorrow. Unset: no limit. Only agents
    /// that report a cost (see `cost_usd`) count towards it.
    pub daily_budget_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Defaults {
    pub agent: Option<String>,
    pub mode: Mode,
    pub worktree: bool,
    pub on_finish: OnFinish,
    pub retries: u32,
    #[serde(with = "humantime_serde")]
    pub retry_delay: Duration,
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    pub headless: Vec<String>,
    pub interactive: Vec<String>,
    /// Appended when a model is set, with `{model}` filled in.
    #[serde(default)]
    pub model_args: Vec<String>,
    /// Model used when the task doesn't name one.
    #[serde(default)]
    pub model: Option<String>,
}

impl Default for Runner {
    fn default() -> Self {
        Self {
            max_parallel: 3,
            max_parallel_per_repo: 1,
            tmux_session: "loompa".into(),
            poll_interval: Duration::from_secs(30),
            daily_budget_usd: None,
        }
    }
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            agent: None,
            mode: Mode::Headless,
            worktree: true,
            on_finish: OnFinish::None,
            retries: 1,
            retry_delay: Duration::from_secs(5 * 60),
            timeout: Duration::from_secs(2 * 60 * 60),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            runner: Runner::default(),
            defaults: Defaults::default(),
            agents: builtin_agents(),
        }
    }
}

/// Presets for common CLI agents: `(name, headless, interactive)`. All take
/// `--model`.
const PRESETS: &[(&str, &[&str], &[&str])] = &[
    (
        "claude",
        &["claude", "-p", "{prompt}", "--output-format", "json"],
        &["claude", "{prompt}"],
    ),
    (
        "codex",
        &["codex", "exec", "{prompt}"],
        &["codex", "{prompt}"],
    ),
    (
        "gemini",
        &["gemini", "--prompt", "{prompt}"],
        &["gemini", "--prompt-interactive", "{prompt}"],
    ),
    (
        "opencode",
        &["opencode", "run", "{prompt}"],
        &["opencode", "--prompt", "{prompt}"],
    ),
    ("pi", &["pi", "-p", "{prompt}"], &["pi", "{prompt}"]),
];

fn builtin_agents() -> BTreeMap<String, Agent> {
    let argv = |parts: &[&str]| parts.iter().map(|s| s.to_string()).collect();
    PRESETS
        .iter()
        .map(|(name, headless, interactive)| {
            let agent = Agent {
                headless: argv(headless),
                interactive: argv(interactive),
                model_args: argv(&["--model", "{model}"]),
                model: None,
            };
            (name.to_string(), agent)
        })
        .collect()
}

/// Preset agents whose program is on `$PATH`, in preset order.
pub fn installed_presets() -> Vec<&'static str> {
    PRESETS
        .iter()
        .filter(|(_, headless, _)| on_path(headless[0]))
        .map(|(name, _, _)| *name)
        .collect()
}

pub fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

impl Config {
    /// Loads `path`, or the defaults if it doesn't exist.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let mut config: Config = toml::from_str(text)?;
        // Built-in agents stay available unless overridden by name.
        for (name, agent) in builtin_agents() {
            config.agents.entry(name).or_insert(agent);
        }
        config.check()?;
        Ok(config)
    }

    fn check(&self) -> Result<()> {
        if self.runner.max_parallel == 0 || self.runner.max_parallel_per_repo == 0 {
            bail!("runner.max_parallel and runner.max_parallel_per_repo must be at least 1");
        }
        if self
            .runner
            .daily_budget_usd
            .is_some_and(|b| b.is_nan() || b <= 0.0)
        {
            bail!("runner.daily_budget_usd must be a positive number");
        }
        for (name, agent) in &self.agents {
            if agent.headless.is_empty() || agent.interactive.is_empty() {
                bail!("agents.{name}: `headless` and `interactive` must not be empty");
            }
        }
        if let Some(agent) = &self.defaults.agent
            && !self.agents.contains_key(agent)
        {
            bail!("defaults.agent `{agent}` is not defined under [agents]");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_template_matches_builtin_defaults() {
        assert_eq!(
            Config::parse(&default_config_toml(None)).unwrap(),
            Config::default()
        );
        let with_agent = Config::parse(&default_config_toml(Some("codex"))).unwrap();
        assert_eq!(with_agent.defaults.agent.as_deref(), Some("codex"));
    }

    #[test]
    fn partial_config_keeps_other_defaults() {
        let c = Config::parse("[runner]\nmax_parallel = 8\n").unwrap();
        assert_eq!(c.runner.max_parallel, 8);
        assert_eq!(c.runner.tmux_session, "loompa");
        assert_eq!(c.agents.len(), PRESETS.len());
    }

    #[test]
    fn custom_agents_are_added_to_builtins() {
        let c = Config::parse(
            "[agents.aider]\nheadless = [\"aider\", \"--message\", \"{prompt}\"]\ninteractive = [\"aider\"]\n",
        )
        .unwrap();
        assert!(c.agents.contains_key("claude"));
        assert!(c.agents["aider"].model_args.is_empty());
    }

    #[test]
    fn rejects_typos() {
        assert!(Config::parse("[runner]\nmax_paralel = 2\n").is_err());
    }
}
