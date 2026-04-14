//! Stable per-agent color assignment for CLI output.

use std::collections::HashMap;

use console::{Color, Style};

/// A palette of distinct terminal colors for agent differentiation.
const AGENT_COLORS: &[Color] = &[
    Color::Cyan,
    Color::Green,
    Color::Magenta,
    Color::Yellow,
    Color::Blue,
    Color::Red,
    Color::Color256(208), // orange
    Color::Color256(141), // light purple
    Color::Color256(39),  // deep sky blue
    Color::Color256(154), // lime
];

/// Assigns stable, distinct colors to agent names within a single output.
pub struct AgentPalette {
    assignments: HashMap<String, Style>,
    next: usize,
}

impl AgentPalette {
    pub fn new() -> Self {
        Self {
            assignments: HashMap::new(),
            next: 0,
        }
    }

    /// Return a `Style` for the given agent name. The same name always gets
    /// the same color within this palette instance.
    pub fn style_for(&mut self, agent: &str) -> &Style {
        if !self.assignments.contains_key(agent) {
            let color = AGENT_COLORS[self.next % AGENT_COLORS.len()];
            self.next += 1;
            self.assignments
                .insert(agent.to_string(), Style::new().fg(color).bold());
        }
        &self.assignments[agent]
    }
}
