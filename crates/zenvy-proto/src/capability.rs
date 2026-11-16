use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Explicit state of a capability (PLAN.md §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapState {
    Available,
    Unavailable,
    PermissionRequired,
    Unsupported,
    Error,
}

impl CapState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Unavailable => "unavailable",
            Self::PermissionRequired => "permission_required",
            Self::Unsupported => "unsupported",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub backend: String,
    pub state: CapState,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemediationCommand {
    pub run_as: String,
    pub argv: Vec<String>,
    pub needs_relogin: bool,
}

/// How to fix a non-available capability. Zenvy only *shows* this; running it needs user approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remediation {
    pub summary: String,
    pub commands: Vec<RemediationCommand>,
    pub requires_user_approval: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityRecord {
    pub state: CapState,
    pub backend: Option<String>,
    pub candidates: Vec<Candidate>,
    pub reason: Option<String>,
    pub remediation: Option<Remediation>,
}

impl CapabilityRecord {
    pub fn available(backend: impl Into<String>) -> Self {
        let backend = backend.into();
        Self {
            state: CapState::Available,
            candidates: vec![Candidate {
                backend: backend.clone(),
                state: CapState::Available,
                reason: None,
            }],
            backend: Some(backend),
            reason: None,
            remediation: None,
        }
    }

    pub fn not(state: CapState, backend: impl Into<String>, reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            state,
            backend: None,
            candidates: vec![Candidate {
                backend: backend.into(),
                state,
                reason: Some(reason.clone()),
            }],
            reason: Some(reason),
            remediation: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistrySnapshot {
    pub schema_version: u32,
    pub capabilities: BTreeMap<String, CapabilityRecord>,
}
