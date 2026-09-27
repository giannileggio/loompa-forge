use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::spec::{Mode, OnFinish};

/// Written by `lf init`. Must stay in sync with `Config::default()`
/// (enforced by a test).
pub const DEFAULT_CONFIG_TOML: &str = r#"# loompa-forge configuration.
# Every value below is the built-in default; delete what you don't change.

[runner]
max_parallel = 3            # tasks running at once, overall
max_parallel_per_repo = 1   # tasks running at once in the same repo
tmux_session = "loompa"     # one session, one window per task
poll_interval = "30s"       # how often `lf run` rescans the folders

# Fallbacks for any field a task or schedule doesn't set.
[defaults]
agent = "claude"
model = "claude-sonnet-5"
mode = "headless"           # headless | interactive
worktree = true             # run each task in its own git worktree
on_finish = "none"          # none | commit | push | pr
retries = 1                 # extra attempts after a failure
retry_delay = "5m"
timeout = "2h"

# How to launch each agent, as argv lists (no shell involved).
# Placeholders: {prompt} {model} {id}
# Headless runs can't answer permission prompts: add flags such as
# "--permission-mode", "acceptEdits" (or an allowlist) to suit your trust level.
[agents.claude]
headless = ["claude", "-p", "{prompt}", "--model", "{model}"]
interactive = ["claude", "--model", "{model}", "{prompt}"]
"#;

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
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Defaults {
    pub agent: String,
    pub model: String,
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
}

impl Default for Runner {
    fn default() -> Self {
        Self {
            max_parallel: 3,
            max_parallel_per_repo: 1,
            tmux_session: "loompa".into(),
            poll_interval: Duration::from_secs(30),
        }
    }
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            agent: "claude".into(),
            model: "claude-sonnet-5".into(),
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

fn builtin_agents() -> BTreeMap<String, Agent> {
    let argv = |parts: &[&str]| parts.iter().map(|s| s.to_string()).collect();
    BTreeMap::from([(
        "claude".to_string(),
        Agent {
            headless: argv(&["claude", "-p", "{prompt}", "--model", "{model}"]),
            interactive: argv(&["claude", "--model", "{model}", "{prompt}"]),
        },
    )])
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
        for (name, agent) in &self.agents {
            if agent.headless.is_empty() || agent.interactive.is_empty() {
                bail!("agents.{name}: `headless` and `interactive` must not be empty");
            }
        }
        if !self.agents.contains_key(&self.defaults.agent) {
            bail!(
                "defaults.agent `{}` is not defined under [agents]",
                self.defaults.agent
            );
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
            Config::parse(DEFAULT_CONFIG_TOML).unwrap(),
            Config::default()
        );
    }

    #[test]
    fn partial_config_keeps_other_defaults() {
        let c = Config::parse("[runner]\nmax_parallel = 8\n").unwrap();
        assert_eq!(c.runner.max_parallel, 8);
        assert_eq!(c.runner.tmux_session, "loompa");
        assert!(c.agents.contains_key("claude"));
    }

    #[test]
    fn custom_agents_are_added_to_builtins() {
        let c = Config::parse(
            "[agents.opencode]\nheadless = [\"opencode\", \"run\", \"{prompt}\"]\ninteractive = [\"opencode\"]\n",
        )
        .unwrap();
        assert!(c.agents.contains_key("claude"));
        assert!(c.agents.contains_key("opencode"));
    }

    #[test]
    fn rejects_typos() {
        assert!(Config::parse("[runner]\nmax_paralel = 2\n").is_err());
    }
}
