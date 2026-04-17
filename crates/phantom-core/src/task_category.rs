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
//! - [`TaskCategory::Custom`] — user-supplied label with no built-in rule body
//!
//! Wire format: built-ins serialize as their bare lowercase name
//! (`"corrective"`, `"adaptive"`, ...). A [`TaskCategory::Custom`] serializes as
//! `"custom:<name>"`. This keeps the on-disk plan format a single JSON string
//! per category so older plans deserialize unchanged.

use std::fmt;
use std::str::FromStr;

use serde::de::{Error as DeError, Unexpected};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Prefix used to distinguish custom category labels on the wire.
const CUSTOM_PREFIX: &str = "custom:";

/// Classical software-maintenance categorisation of a task.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TaskCategory {
    /// Bug fix. Requires reproduction test before implementation.
    Corrective,
    /// Refactor, cleanup, or performance work. Tests are read-only.
    Perfective,
    /// Test hardening. Source code is read-only.
    Preventive,
    /// New feature or extension. Must mirror an existing precedent.
    Adaptive,
    /// User-supplied label. No built-in rule body is rendered for `Custom`; the
    /// dispatcher falls through to "no category rules file". Reserved for
    /// escape-hatch use when none of the four built-ins fit.
    Custom(String),
}

impl TaskCategory {
    /// All four canonical built-in categories. [`TaskCategory::Custom`] is
    /// parametrised and therefore intentionally excluded.
    pub const ALL: [TaskCategory; 4] = [
        TaskCategory::Corrective,
        TaskCategory::Perfective,
        TaskCategory::Preventive,
        TaskCategory::Adaptive,
    ];

    /// Short string form — the canonical lowercase name for built-ins (also
    /// used as the on-disk filename stem), or the bare custom payload for
    /// [`TaskCategory::Custom`]. This is NOT the wire form; see
    /// [`TaskCategory::as_wire_string`] for serialisation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            TaskCategory::Corrective => "corrective",
            TaskCategory::Perfective => "perfective",
            TaskCategory::Preventive => "preventive",
            TaskCategory::Adaptive => "adaptive",
            TaskCategory::Custom(name) => name.as_str(),
        }
    }

    /// Wire form — the exact string used in JSON. Built-ins serialise as bare
    /// lowercase; [`TaskCategory::Custom`] serialises as `"custom:<name>"`.
    #[must_use]
    pub fn as_wire_string(&self) -> String {
        match self {
            TaskCategory::Corrective
            | TaskCategory::Perfective
            | TaskCategory::Preventive
            | TaskCategory::Adaptive => self.as_str().to_string(),
            TaskCategory::Custom(name) => format!("{CUSTOM_PREFIX}{name}"),
        }
    }

    /// Returns true for the four built-in categories that have canonical rule
    /// bodies on disk.
    #[must_use]
    pub fn is_builtin(&self) -> bool {
        !matches!(self, TaskCategory::Custom(_))
    }
}

impl fmt::Display for TaskCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_wire_string())
    }
}

/// Error returned when parsing an unknown or malformed category string.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "unknown task category: '{0}' (expected one of: corrective, perfective, preventive, adaptive, custom:<name>)"
)]
pub struct ParseTaskCategoryError(pub String);

impl FromStr for TaskCategory {
    type Err = ParseTaskCategoryError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        let lower = trimmed.to_ascii_lowercase();
        match lower.as_str() {
            "corrective" => Ok(TaskCategory::Corrective),
            "perfective" => Ok(TaskCategory::Perfective),
            "preventive" => Ok(TaskCategory::Preventive),
            "adaptive" => Ok(TaskCategory::Adaptive),
            other if other.starts_with(CUSTOM_PREFIX) => {
                // Preserve the user's casing in the custom payload.
                let name_start = trimmed.len() - (other.len() - CUSTOM_PREFIX.len());
                let name = trimmed[name_start..].trim();
                if name.is_empty() {
                    return Err(ParseTaskCategoryError(s.to_string()));
                }
                Ok(TaskCategory::Custom(name.to_string()))
            }
            _ => Err(ParseTaskCategoryError(s.to_string())),
        }
    }
}

impl Serialize for TaskCategory {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.as_wire_string())
    }
}

impl<'de> Deserialize<'de> for TaskCategory {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(|_| {
            D::Error::invalid_value(
                Unexpected::Str(&s),
                &"one of: corrective, perfective, preventive, adaptive, custom:<name>",
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_from_str_roundtrip_builtins() {
        for cat in &TaskCategory::ALL {
            let s = cat.to_string();
            let parsed: TaskCategory = s.parse().unwrap();
            assert_eq!(&parsed, cat);
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
    fn serde_roundtrip_builtin_is_bare_lowercase() {
        for cat in TaskCategory::ALL {
            let json = serde_json::to_string(&cat).unwrap();
            assert_eq!(json, format!("\"{}\"", cat.as_wire_string()));
            let back: TaskCategory = serde_json::from_str(&json).unwrap();
            assert_eq!(back, cat);
        }
    }

    #[test]
    fn custom_variant_roundtrip() {
        let cat = TaskCategory::Custom("migrate-to-v2".into());
        let json = serde_json::to_string(&cat).unwrap();
        assert_eq!(json, r#""custom:migrate-to-v2""#);
        let back: TaskCategory = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cat);
    }

    #[test]
    fn custom_preserves_payload_casing() {
        let parsed: TaskCategory = "custom:MigrateV2".parse().unwrap();
        assert_eq!(parsed, TaskCategory::Custom("MigrateV2".into()));
    }

    #[test]
    fn custom_rejects_empty_payload() {
        assert!("custom:".parse::<TaskCategory>().is_err());
        assert!("custom:   ".parse::<TaskCategory>().is_err());
    }

    #[test]
    fn custom_prefix_is_case_insensitive() {
        let parsed: TaskCategory = "CUSTOM:foo".parse().unwrap();
        assert_eq!(parsed, TaskCategory::Custom("foo".into()));
    }

    #[test]
    fn is_builtin_matches_all_array() {
        for cat in TaskCategory::ALL {
            assert!(cat.is_builtin(), "{cat} should be a built-in");
        }
        assert!(!TaskCategory::Custom("x".into()).is_builtin());
    }

    #[test]
    fn deserialize_rejects_unknown_string() {
        let err = serde_json::from_str::<TaskCategory>(r#""unknown""#).unwrap_err();
        assert!(err.to_string().contains("unknown"));
    }
}
