//! Pre-built [`EventQuery`] constructors used by multiple commands.

use phantom_events::query::EventQuery;

/// Query all `PlanCreated` events — used by `status` and `tasks` to group
/// overlays by their plan.
pub fn plans_only() -> EventQuery {
    EventQuery {
        kind_prefixes: vec!["PlanCreated".into()],
        ..Default::default()
    }
}
