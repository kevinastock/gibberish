use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_CONFIG_CONTENTS: &str = include_str!("../gibberish.toml");

#[derive(Debug, Clone, Deserialize)]
pub struct ShellConfig {
    pub program: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionConfig {
    pub wait_ms: u64,
    #[serde(default)]
    pub yolo: bool,
    pub shell: ShellConfig,
    pub llm: LlmConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LlmConfig {
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub skin: SkinMode,
    pub initial_prompt: String,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SkinMode {
    Light,
    Dark,
    #[default]
    Default,
}

impl SessionConfig {
    pub fn terminal_size(&self) -> Result<(usize, usize)> {
        let cols = parse_usize_env_var(&self.shell.env, "COLUMNS")?;
        let rows = parse_usize_env_var(&self.shell.env, "LINES")?;
        ensure!(cols > 0, "shell.env.COLUMNS must be greater than zero");
        ensure!(rows > 0, "shell.env.LINES must be greater than zero");
        Ok((cols, rows))
    }

    pub fn validate_llm(&self) -> Result<()> {
        ensure!(
            !self.llm.api_key.trim().is_empty(),
            "llm.api_key must not be empty (or set OPENAI_API_KEY)"
        );
        ensure!(
            !self.llm.initial_prompt.trim().is_empty(),
            "llm.initial_prompt must not be empty"
        );
        Ok(())
    }

    pub fn resolve_llm_api_key(&mut self, env_api_key: Option<String>) {
        if !self.llm.api_key.trim().is_empty() {
            return;
        }

        if let Some(api_key) = env_api_key
            && !api_key.trim().is_empty()
        {
            self.llm.api_key = api_key;
        }
    }
}

pub fn resolve_session_options(cli_path: Option<&Path>) -> Result<SessionConfig> {
    let path = match cli_path {
        Some(path) => path.to_path_buf(),
        None => {
            let path = default_config_path()?;
            ensure_default_config_file(&path)?;
            path
        }
    };

    let contents = fs::read_to_string(&path)
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    let mut config = toml::from_str::<SessionConfig>(&contents)
        .with_context(|| format!("failed to parse config file {}", path.display()))?;
    config.resolve_llm_api_key(std::env::var("OPENAI_API_KEY").ok());
    config
        .terminal_size()
        .with_context(|| format!("invalid terminal size in config file {}", path.display()))?;
    config
        .validate_llm()
        .with_context(|| format!("invalid llm settings in config file {}", path.display()))?;
    Ok(config)
}

fn default_config_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("failed to determine HOME directory for default config path")?;
    Ok(home.join(".config").join("gibberish").join("config.toml"))
}

fn ensure_default_config_file(path: &Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config directory for {}", path.display()))?;
    }

    fs::write(path, DEFAULT_CONFIG_CONTENTS)
        .with_context(|| format!("failed to write default config file {}", path.display()))?;
    Ok(())
}

fn parse_usize_env_var(env: &BTreeMap<String, String>, key: &str) -> Result<usize> {
    let value = env
        .get(key)
        .with_context(|| format!("missing shell.env.{key}"))?;
    value
        .parse::<usize>()
        .with_context(|| format!("shell.env.{key} must be a positive integer (got {value:?})"))
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_CONFIG_CONTENTS, LlmConfig, SessionConfig, ShellConfig, SkinMode,
        ensure_default_config_file,
    };
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    const TEST_INITIAL_PROMPT: &str = "Use raw_input tool.";

    fn base_config(api_key: &str) -> SessionConfig {
        let mut env = BTreeMap::new();
        env.insert("COLUMNS".to_string(), "80".to_string());
        env.insert("LINES".to_string(), "24".to_string());
        SessionConfig {
            wait_ms: 1000,
            yolo: false,
            shell: ShellConfig {
                program: "/bin/bash".to_string(),
                args: vec!["--noprofile".to_string()],
                env,
            },
            llm: LlmConfig {
                api_key: api_key.to_string(),
                skin: SkinMode::Default,
                initial_prompt: TEST_INITIAL_PROMPT.to_string(),
            },
        }
    }

    #[test]
    fn uses_env_api_key_when_config_api_key_is_missing() {
        let mut config = base_config("");
        config.resolve_llm_api_key(Some("env-key".to_string()));

        assert_eq!(config.llm.api_key, "env-key");
    }

    #[test]
    fn keeps_config_api_key_when_present() {
        let mut config = base_config("config-key");
        config.resolve_llm_api_key(Some("env-key".to_string()));

        assert_eq!(config.llm.api_key, "config-key");
    }

    #[test]
    fn ignores_blank_env_api_key() {
        let mut config = base_config("");
        config.resolve_llm_api_key(Some("   ".to_string()));

        assert!(config.validate_llm().is_err());
    }

    #[test]
    fn defaults_skin_mode_when_unspecified() {
        let parsed: SessionConfig = toml::from_str(
            r#"
wait_ms = 1000

[shell]
program = "/bin/bash"
args = ["--noprofile"]

[shell.env]
COLUMNS = "80"
LINES = "24"

[llm]
api_key = "config-key"
initial_prompt = "Use raw_input tool."
"#,
        )
        .expect("valid session config");

        assert!(!parsed.yolo);
        assert_eq!(parsed.llm.skin, SkinMode::Default);
    }

    #[test]
    fn parses_explicit_skin_mode() {
        let parsed: SessionConfig = toml::from_str(
            r#"
wait_ms = 1000

[shell]
program = "/bin/bash"
args = ["--noprofile"]

[shell.env]
COLUMNS = "80"
LINES = "24"

[llm]
api_key = "config-key"
skin = "dark"
initial_prompt = "Use raw_input tool."
"#,
        )
        .expect("valid session config");

        assert_eq!(parsed.llm.skin, SkinMode::Dark);
    }

    #[test]
    fn parses_explicit_yolo_mode() {
        let parsed: SessionConfig = toml::from_str(
            r#"
wait_ms = 1000
yolo = true

[shell]
program = "/bin/bash"
args = ["--noprofile"]

[shell.env]
COLUMNS = "80"
LINES = "24"

[llm]
api_key = "config-key"
initial_prompt = "Use raw_input tool."
"#,
        )
        .expect("valid session config");

        assert!(parsed.yolo);
    }

    #[test]
    fn rejects_missing_initial_prompt() {
        let parsed = toml::from_str::<SessionConfig>(
            r#"
wait_ms = 1000

[shell]
program = "/bin/bash"
args = ["--noprofile"]

[shell.env]
COLUMNS = "80"
LINES = "24"

[llm]
api_key = "config-key"
"#,
        );

        assert!(parsed.is_err());
    }

    #[test]
    fn writes_default_config_when_missing() {
        let temp_dir = unique_temp_dir("writes-default-config");
        let config_path = temp_dir.join("gibberish").join("config.toml");

        ensure_default_config_file(&config_path).expect("write default config");

        let contents = fs::read_to_string(&config_path).expect("read default config");
        assert_eq!(contents, DEFAULT_CONFIG_CONTENTS);

        fs::remove_dir_all(&temp_dir).expect("remove temp dir");
    }

    #[test]
    fn keeps_existing_config_file_contents() {
        let temp_dir = unique_temp_dir("keeps-existing-config");
        let config_path = temp_dir.join("gibberish").join("config.toml");

        fs::create_dir_all(config_path.parent().expect("parent path")).expect("create parent");
        fs::write(&config_path, "wait_ms = 42\n").expect("seed config");

        ensure_default_config_file(&config_path).expect("do not overwrite existing config");

        let contents = fs::read_to_string(&config_path).expect("read config");
        assert_eq!(contents, "wait_ms = 42\n");

        fs::remove_dir_all(&temp_dir).expect("remove temp dir");
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!("gibberish-{label}-{now}"))
    }
}
