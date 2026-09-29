use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub config_file: PathBuf,
    pub hosts_file: PathBuf,
    pub known_hosts_file: PathBuf,
    pub database_file: PathBuf,
    pub runs_dir: PathBuf,
    pub log_file: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self, ConfigError> {
        let project = ProjectDirs::from("", "", "termr").ok_or(ConfigError::PathsUnavailable)?;
        let state_dir = project.state_dir().unwrap_or_else(|| project.data_dir());

        Ok(Self::from_roots(
            project.config_dir(),
            project.data_dir(),
            state_dir,
        ))
    }

    pub fn from_roots(config_dir: &Path, data_dir: &Path, state_dir: &Path) -> Self {
        Self {
            config_file: config_dir.join("config.yaml"),
            hosts_file: config_dir.join("hosts.yaml"),
            known_hosts_file: config_dir.join("known_hosts"),
            database_file: data_dir.join("termr.db"),
            runs_dir: data_dir.join("runs"),
            log_file: state_dir.join("termr.log"),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub max_concurrent_sessions: usize,
    pub connect_timeout_seconds: u64,
    pub command_timeout_seconds: u64,
    pub log_level: LogLevel,
    pub commands: BTreeMap<String, SavedCommandConfig>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            max_concurrent_sessions: 10,
            connect_timeout_seconds: 10,
            command_timeout_seconds: 30,
            log_level: LogLevel::Info,
            commands: BTreeMap::new(),
        }
    }
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match fs::read_to_string(path) {
            Ok(contents) => Self::from_yaml(&contents),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Read {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    pub fn from_yaml(yaml: &str) -> Result<Self, ConfigError> {
        let config: Self = serde_yaml::from_str(yaml).map_err(ConfigError::Parse)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let mut errors = Vec::new();

        if self.max_concurrent_sessions == 0 {
            errors.push("max_concurrent_sessions must be greater than zero".to_owned());
        }
        if self.connect_timeout_seconds == 0 {
            errors.push("connect_timeout_seconds must be greater than zero".to_owned());
        }
        if self.command_timeout_seconds == 0 {
            errors.push("command_timeout_seconds must be greater than zero".to_owned());
        }
        for (name, saved) in &self.commands {
            if name.trim().is_empty() {
                errors.push("commands contains an empty command name".to_owned());
            }
            if saved.command.trim().is_empty() {
                errors.push(format!("commands.{name}.command must not be empty"));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::Invalid(errors.join("; ")))
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SavedCommandConfig {
    pub command: String,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("platform configuration directories are unavailable")]
    PathsUnavailable,
    #[error("failed to read configuration at {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid config.yaml: {0}")]
    Parse(#[source] serde_yaml::Error),
    #[error("invalid config.yaml: {0}")]
    Invalid(String),
}
