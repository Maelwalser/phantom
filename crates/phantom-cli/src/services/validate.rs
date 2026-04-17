//! Shared agent-name validation used by all commands that accept an `<agent>`
//! positional argument.

use phantom_core::id::AgentId;

/// Validate a user-provided agent name and wrap it in a typed [`AgentId`].
///
/// Returns an `anyhow::Error` with a message suitable for direct display:
///
/// ```text
/// invalid agent name 'Agent With Spaces': must match [a-z0-9-]+
/// ```
pub fn agent_id(name: &str) -> anyhow::Result<AgentId> {
    AgentId::validate(name).map_err(|e| anyhow::anyhow!("invalid agent name '{name}': {e}"))
}
