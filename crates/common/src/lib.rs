use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, time::Duration};
use thiserror::Error;

pub const MAX_COMMAND_LENGTH: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct WorkflowEnvironment {
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinition {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub requires: Map<String, Value>,
    #[serde(default)]
    pub environment: WorkflowEnvironment,
    #[serde(default, alias = "setup")]
    pub prepare: Vec<WorkflowStep>,
    #[serde(default)]
    pub deploy: Vec<WorkflowStep>,
    #[serde(default)]
    pub configure: Vec<WorkflowStep>,
    #[serde(default)]
    pub test: Vec<WorkflowStep>,
    #[serde(default)]
    pub collect: Vec<WorkflowStep>,
    #[serde(default)]
    pub cleanup: Vec<WorkflowStep>,
    #[serde(default)]
    pub artifacts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowStep {
    pub name: String,
    pub run: String,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub continue_on_error: bool,
    #[serde(default)]
    pub working_directory: Option<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
}

fn default_timeout() -> u64 {
    300
}

#[derive(Debug, Error)]
pub enum WorkflowValidationError {
    #[error("invalid workflow YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("workflow name must not be empty or exceed 200 characters")]
    InvalidName,
    #[error("workflow must contain at least one step")]
    NoSteps,
    #[error("step name and command must not be empty")]
    EmptyStep,
    #[error("step command exceeds the 64 KiB limit")]
    CommandTooLong,
    #[error("step timeout must be between 1 second and 24 hours")]
    InvalidTimeout,
    #[error("working_directory must be relative and remain inside the run workspace")]
    InvalidWorkingDirectory,
    #[error("artifact pattern must be relative and remain inside the run workspace")]
    InvalidArtifactPattern,
}

impl WorkflowDefinition {
    pub fn parse(yaml: &str) -> Result<Self, WorkflowValidationError> {
        let value: Self = serde_yaml::from_str(yaml)?;
        value.validate()?;
        Ok(value)
    }
    pub fn phases(&self) -> [(&'static str, &[WorkflowStep]); 6] {
        [
            ("prepare", &self.prepare),
            ("deploy", &self.deploy),
            ("configure", &self.configure),
            ("test", &self.test),
            ("collect", &self.collect),
            ("cleanup", &self.cleanup),
        ]
    }
    pub fn validate(&self) -> Result<(), WorkflowValidationError> {
        if self.name.trim().is_empty() || self.name.len() > 200 {
            return Err(WorkflowValidationError::InvalidName);
        }
        if self.phases().iter().all(|(_, steps)| steps.is_empty()) {
            return Err(WorkflowValidationError::NoSteps);
        }
        for (_, steps) in self.phases() {
            for step in steps {
                if step.name.trim().is_empty() || step.run.trim().is_empty() {
                    return Err(WorkflowValidationError::EmptyStep);
                }
                if step.run.len() > MAX_COMMAND_LENGTH {
                    return Err(WorkflowValidationError::CommandTooLong);
                }
                if !(1..=Duration::from_secs(86400).as_secs()).contains(&step.timeout_seconds) {
                    return Err(WorkflowValidationError::InvalidTimeout);
                }
                if step
                    .working_directory
                    .as_deref()
                    .is_some_and(unsafe_relative_path)
                {
                    return Err(WorkflowValidationError::InvalidWorkingDirectory);
                }
            }
        }
        if self.artifacts.iter().any(|path| unsafe_relative_path(path)) {
            return Err(WorkflowValidationError::InvalidArtifactPattern);
        }
        Ok(())
    }
}

fn unsafe_relative_path(path: &str) -> bool {
    path.is_empty() || path.starts_with('/') || path.split('/').any(|part| part == "..")
}
pub fn capabilities_match(capabilities: &Value, required: &Map<String, Value>) -> bool {
    required
        .iter()
        .all(|(key, expected)| capabilities.get(key) == Some(expected))
}
pub fn hash_token(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_legacy_setup_as_prepare() {
        let workflow =
            WorkflowDefinition::parse("name: smoke\nsetup:\n  - name: ok\n    run: echo ok\n")
                .unwrap();
        assert_eq!(workflow.prepare[0].timeout_seconds, 300);
    }
    #[test]
    fn rejects_traversal_and_long_commands() {
        let yaml = "name: bad\ntest:\n- name: bad\n  run: echo bad\n  working_directory: ../etc\n";
        assert!(matches!(
            WorkflowDefinition::parse(yaml),
            Err(WorkflowValidationError::InvalidWorkingDirectory)
        ));
        let mut workflow =
            WorkflowDefinition::parse("name: ok\ntest:\n- name: ok\n  run: echo ok\n").unwrap();
        workflow.test[0].run = "x".repeat(MAX_COMMAND_LENGTH + 1);
        assert!(matches!(
            workflow.validate(),
            Err(WorkflowValidationError::CommandTooLong)
        ));
    }
    #[test]
    fn matches_generic_capabilities() {
        let capabilities = serde_json::json!({"compute":"jetson-orin","can":true,"nix":false});
        let required =
            serde_json::from_value(serde_json::json!({"compute":"jetson-orin","can":true}))
                .unwrap();
        assert!(capabilities_match(&capabilities, &required));
    }
    #[test]
    fn hashes_tokens() {
        assert_eq!(hash_token("secret"), hash_token("secret"));
        assert_ne!(hash_token("secret"), "secret");
    }
}
