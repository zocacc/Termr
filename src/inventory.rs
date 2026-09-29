use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    IdentityFile,
    Agent,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Host {
    pub id: String,
    pub name: String,
    pub address: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub username: String,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub auth_method: AuthMethod,
    #[serde(default)]
    pub identity_file: Option<PathBuf>,
}

const fn default_ssh_port() -> u16 {
    22
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inventory {
    hosts: Vec<Host>,
}

impl Inventory {
    pub fn from_yaml(yaml: &str) -> Result<Self, InventoryError> {
        let file: InventoryFile = serde_yaml::from_str(yaml).map_err(InventoryError::Parse)?;
        let inventory = Self { hosts: file.hosts };
        inventory.validate()?;
        Ok(inventory)
    }

    pub fn load(path: &Path) -> Result<Self, InventoryError> {
        let yaml = fs::read_to_string(path).map_err(|source| InventoryError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_yaml(&yaml)
    }

    pub fn hosts(&self) -> &[Host] {
        &self.hosts
    }

    fn validate(&self) -> Result<(), InventoryError> {
        let mut errors = Vec::new();
        let mut ids = HashMap::new();
        let mut names = HashMap::new();

        for (index, host) in self.hosts.iter().enumerate() {
            let prefix = format!("hosts[{index}]");

            if !valid_host_id(&host.id) {
                errors.push(format!(
                    "{prefix}.id must be 1-64 ASCII letters, numbers, underscores, or hyphens and start with a letter or number"
                ));
            }
            if host.name.trim().is_empty() {
                errors.push(format!("{prefix}.name must not be empty"));
            }
            if host.address.trim().is_empty() {
                errors.push(format!("{prefix}.address must not be empty"));
            }
            if host.username.trim().is_empty() {
                errors.push(format!("{prefix}.username must not be empty"));
            }
            if host
                .group
                .as_ref()
                .is_some_and(|group| group.trim().is_empty())
            {
                errors.push(format!("{prefix}.group must not be empty when provided"));
            }
            for (tag_index, tag) in host.tags.iter().enumerate() {
                if tag.trim().is_empty() {
                    errors.push(format!("{prefix}.tags[{tag_index}] must not be empty"));
                }
            }

            match (&host.auth_method, &host.identity_file) {
                (AuthMethod::IdentityFile, None) => errors.push(format!(
                    "{prefix}.identity_file is required when auth_method is identity_file"
                )),
                (AuthMethod::Agent, Some(_)) => errors.push(format!(
                    "{prefix}.identity_file must be omitted when auth_method is agent"
                )),
                _ => {}
            }

            let normalized_id = host.id.to_lowercase();
            if let Some(first) = ids.insert(normalized_id, index) {
                errors.push(format!(
                    "{prefix}.id duplicates hosts[{first}].id (comparison is case-insensitive)"
                ));
            }

            let normalized_name = host.name.to_lowercase();
            if let Some(first) = names.insert(normalized_name, index) {
                errors.push(format!(
                    "{prefix}.name duplicates hosts[{first}].name (comparison is case-insensitive)"
                ));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(InventoryError::Invalid(errors.join("; ")))
        }
    }
}

fn valid_host_id(id: &str) -> bool {
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return false;
    };

    id.len() <= 64
        && first.is_ascii_alphanumeric()
        && chars.all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InventoryFile {
    hosts: Vec<Host>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryStore {
    current: Inventory,
}

impl InventoryStore {
    pub fn new(initial: Inventory) -> Self {
        Self { current: initial }
    }

    pub fn load(path: &Path) -> Result<Self, InventoryError> {
        Inventory::load(path).map(Self::new)
    }

    pub fn current(&self) -> &Inventory {
        &self.current
    }

    pub fn reload(&mut self, path: &Path) -> Result<(), InventoryError> {
        let candidate = Inventory::load(path)?;
        self.current = candidate;
        Ok(())
    }

    pub fn reload_from_yaml(&mut self, yaml: &str) -> Result<(), InventoryError> {
        let candidate = Inventory::from_yaml(yaml)?;
        self.current = candidate;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum InventoryError {
    #[error("failed to read inventory at {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid hosts.yaml: {0}")]
    Parse(#[source] serde_yaml::Error),
    #[error("invalid hosts.yaml: {0}")]
    Invalid(String),
}
