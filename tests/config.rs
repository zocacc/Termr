use std::path::Path;

use termr::config::{AppConfig, AppPaths, LogLevel};

#[test]
fn empty_yaml_uses_documented_defaults() {
    let config = AppConfig::from_yaml("{}").unwrap();

    assert_eq!(config.max_concurrent_sessions, 10);
    assert_eq!(config.connect_timeout_seconds, 10);
    assert_eq!(config.command_timeout_seconds, 30);
    assert_eq!(config.log_level, LogLevel::Info);
    assert!(config.commands.is_empty());
}

#[test]
fn explicit_values_are_loaded() {
    let config = AppConfig::from_yaml(
        r#"
max_concurrent_sessions: 4
connect_timeout_seconds: 5
command_timeout_seconds: 90
log_level: debug
commands:
  version:
    command: show version
"#,
    )
    .unwrap();

    assert_eq!(config.max_concurrent_sessions, 4);
    assert_eq!(config.connect_timeout_seconds, 5);
    assert_eq!(config.command_timeout_seconds, 90);
    assert_eq!(config.log_level, LogLevel::Debug);
    assert_eq!(config.commands["version"].command, "show version");
}

#[test]
fn invalid_values_name_every_bad_field() {
    let error = AppConfig::from_yaml(
        r#"
max_concurrent_sessions: 0
connect_timeout_seconds: 0
command_timeout_seconds: 0
"#,
    )
    .unwrap_err();
    let message = error.to_string();

    assert!(message.contains("max_concurrent_sessions"));
    assert!(message.contains("connect_timeout_seconds"));
    assert!(message.contains("command_timeout_seconds"));
}

#[test]
fn malformed_fields_are_identified() {
    let error = AppConfig::from_yaml("max_concurrent_sessions: many").unwrap_err();

    assert!(error.to_string().contains("max_concurrent_sessions"));
}

#[test]
fn missing_config_file_returns_defaults_without_creating_files() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.yaml");

    let config = AppConfig::load(&path).unwrap();

    assert_eq!(config, AppConfig::default());
    assert!(!path.exists());
}

#[test]
fn paths_follow_the_platform_directory_contract() {
    let paths = AppPaths::from_roots(
        Path::new("/config/termr"),
        Path::new("/data/termr"),
        Path::new("/state/termr"),
    );

    assert_eq!(paths.config_file, Path::new("/config/termr/config.yaml"));
    assert_eq!(paths.hosts_file, Path::new("/config/termr/hosts.yaml"));
    assert_eq!(
        paths.known_hosts_file,
        Path::new("/config/termr/known_hosts")
    );
    assert_eq!(paths.database_file, Path::new("/data/termr/termr.db"));
    assert_eq!(paths.runs_dir, Path::new("/data/termr/runs"));
    assert_eq!(paths.log_file, Path::new("/state/termr/termr.log"));
}
