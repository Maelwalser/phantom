//! Relative time formatting for CLI output.

use chrono::{DateTime, Utc};
use console::Style;

/// Format a timestamp as a human-friendly relative string.
///
/// Accepts an explicit `now` for testability.
pub fn format_relative_time(ts: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let elapsed = now.signed_duration_since(ts);
    let secs = elapsed.num_seconds();

    if secs < 0 {
        return "just now".into();
    }
    if secs < 60 {
        return "just now".into();
    }
    if secs < 3600 {
        let mins = elapsed.num_minutes();
        return format!("{mins}m ago");
    }
    if secs < 86400 {
        let hours = elapsed.num_hours();
        return format!("{hours}h ago");
    }

    let days = elapsed.num_days();
    if days == 1 {
        return "yesterday".into();
    }
    if days < 7 {
        return format!("{days}d ago");
    }

    // Older than a week: abbreviated date
    ts.format("%b %d").to_string()
}

/// Format a timestamp relative to now.
pub fn relative_time(ts: DateTime<Utc>) -> String {
    format_relative_time(ts, Utc::now())
}

/// Return a dim-styled relative timestamp for use in listings.
pub fn dim_timestamp(ts: DateTime<Utc>) -> console::StyledObject<String> {
    Style::new().dim().apply_to(relative_time(ts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(year: i32, month: u32, day: u32, hour: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, min, 0)
            .unwrap()
    }

    #[test]
    fn just_now_under_one_minute() {
        let now = ts(2026, 4, 14, 12, 0);
        let t = now - chrono::Duration::seconds(30);
        assert_eq!(format_relative_time(t, now), "just now");
    }

    #[test]
    fn minutes_ago() {
        let now = ts(2026, 4, 14, 12, 0);
        let t = now - chrono::Duration::minutes(5);
        assert_eq!(format_relative_time(t, now), "5m ago");
    }

    #[test]
    fn hours_ago() {
        let now = ts(2026, 4, 14, 12, 0);
        let t = now - chrono::Duration::hours(3);
        assert_eq!(format_relative_time(t, now), "3h ago");
    }

    #[test]
    fn yesterday() {
        let now = ts(2026, 4, 14, 12, 0);
        let t = now - chrono::Duration::days(1);
        assert_eq!(format_relative_time(t, now), "yesterday");
    }

    #[test]
    fn days_ago() {
        let now = ts(2026, 4, 14, 12, 0);
        let t = now - chrono::Duration::days(4);
        assert_eq!(format_relative_time(t, now), "4d ago");
    }

    #[test]
    fn older_than_a_week_shows_date() {
        let now = ts(2026, 4, 14, 12, 0);
        let t = ts(2026, 3, 20, 10, 0);
        let result = format_relative_time(t, now);
        assert_eq!(result, "Mar 20");
    }

    #[test]
    fn future_timestamp_shows_just_now() {
        let now = ts(2026, 4, 14, 12, 0);
        let t = now + chrono::Duration::hours(1);
        assert_eq!(format_relative_time(t, now), "just now");
    }
}
