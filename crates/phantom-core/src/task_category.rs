//! Task category tags used to inject per-category discipline rules into
//! agent sessions.
//!
//! A category is optional metadata on a task or plan domain. When present, the
//! CLI picks a matching static rule file (`.phantom/rules/<category>.md`) and
//! injects it into the agent's system prompt. Categories follow the classical
//! software-maintenance taxonomy:
//!
//! - [`TaskCategory::Corrective`] — bug fixes
//! - [`TaskCategory::Perfective`] — refactors and performance work
//! - [`TaskCategory::Preventive`] — test hardening and coverage work
//! - [`TaskCategory::Adaptive`] — new features and extensions

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Classical software-maintenance categorisation of a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskCategory {
    /// Bug fix. Requires reproduction test before implementation.
    Corrective,
    /// Refactor, cleanup, or performance work. Tests are read-only.
    Perfective,
    /// Test hardening. Source code is read-only.
    Preventive,
    /// New feature or extension. Must mirror an existing precedent.
    Adaptive,
}

impl TaskCategory {
    /// All four categories in canonical order.
    pub const ALL: [TaskCategory; 4] = [
        TaskCategory::Corrective,
        TaskCategory::Perfective,
        TaskCategory::Preventive,
        TaskCategory::Adaptive,
    ];

    /// Lowercase string form — used as the `.md` filename stem and in JSON.
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskCategory::Corrective => "corrective",
            TaskCategory::Perfective => "perfective",
            TaskCategory::Preventive => "preventive",
            TaskCategory::Adaptive => "adaptive",
        }
    }
}

impl fmt::Display for TaskCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when parsing an unknown category string.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "unknown task category: '{0}' (expected one of: corrective, perfective, preventive, adaptive)"
)]
pub struct ParseTaskCategoryError(pub String);

impl FromStr for TaskCategory {
    type Err = ParseTaskCategoryError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "corrective" => Ok(TaskCategory::Corrective),
            "perfective" => Ok(TaskCategory::Perfective),
            "preventive" => Ok(TaskCategory::Preventive),
            "adaptive" => Ok(TaskCategory::Adaptive),
            _ => Err(ParseTaskCategoryError(s.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_from_str_roundtrip() {
        for cat in TaskCategory::ALL {
            let s = cat.to_string();
            let parsed: TaskCategory = s.parse().unwrap();
            assert_eq!(parsed, cat);
        }
    }

    #[test]
    fn from_str_accepts_mixed_case_and_whitespace() {
        assert_eq!(
            "  Corrective ".parse::<TaskCategory>().unwrap(),
            TaskCategory::Corrective
        );
        assert_eq!(
            "ADAPTIVE".parse::<TaskCategory>().unwrap(),
            TaskCategory::Adaptive
        );
    }

    #[test]
    fn from_str_rejects_unknown() {
        let err = "cleanup".parse::<TaskCategory>().unwrap_err();
        assert!(err.to_string().contains("cleanup"));
    }

    #[test]
    fn serde_roundtrip_lowercase() {
        for cat in TaskCategory::ALL {
            let json = serde_json::to_string(&cat).unwrap();
            assert_eq!(json, format!("\"{}\"", cat.as_str()));
            let back: TaskCategory = serde_json::from_str(&json).unwrap();
            assert_eq!(back, cat);
        }
    }
}
