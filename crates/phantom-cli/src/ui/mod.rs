//! Shared CLI presentation helpers: styling, formatting, and reusable widgets.

pub mod agent_color;
pub mod style;
pub mod text;
pub mod textbox;
pub mod time;

// Re-export the flat function API used across commands.
#[allow(unused_imports)]
pub use style::{
    action_hint, empty_state, key_value, run_state_indicator, run_state_text, section_header,
    status_label, style_bold, style_cyan, style_dim, style_error, style_success, style_warning,
    success_message, warning_message,
};
pub use text::{term_width, truncate_line};
#[allow(unused_imports)]
pub use time::{dim_timestamp, format_relative_time, relative_time};
