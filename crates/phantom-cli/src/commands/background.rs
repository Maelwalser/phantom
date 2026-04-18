//! `phantom background` — real-time watch view of all background agents.
//!
//! Refreshes on a configurable interval (default 1s) and groups agents into
//! three sections so the operator immediately sees the work pipeline around
//! any submit:
//!
//!   1. **Next up** — agents waiting on upstream materializations.
//!   2. **Running** — agents currently executing.
//!   3. **Recently completed** — finished or failed agents, most recent first.

use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use phantom_core::event::EventKind;
use phantom_core::id::AgentId;
use phantom_core::traits::EventStore;

use crate::context::PhantomContext;

use super::agent_monitor;
use super::status::{self, AgentRunState};
use crate::ui;

#[derive(clap::Args)]
pub struct BackgroundArgs {
    /// Refresh interval in seconds
    #[arg(short = 'n', long = "interval", default_value = "1")]
    pub interval: f64,
}

pub async fn run(args: BackgroundArgs) -> anyhow::Result<()> {
    let interval = Duration::from_secs_f64(args.interval.max(0.1));

    let mut stdout = io::stdout();

    // Hide cursor while rendering.
    write!(stdout, "\x1b[?25l")?;
    stdout.flush()?;

    let result = run_loop(&mut stdout, interval).await;

    // Show cursor on exit.
    write!(stdout, "\x1b[?25h")?;
    stdout.flush()?;

    result
}

/// Count the number of visual (screen) rows a line occupies, accounting for
/// terminal wrapping. ANSI escape sequences are stripped before measuring.
fn visual_line_count(line: &str, term_width: usize) -> usize {
    if term_width == 0 {
        return 1;
    }
    let visible_width = console::measure_text_width(line);
    if visible_width == 0 {
        1 // empty line still occupies one row
    } else {
        visible_width.div_ceil(term_width)
    }
}

async fn run_loop(stdout: &mut io::Stdout, interval: Duration) -> anyhow::Result<()> {
    let mut prev_visual_lines = 0usize;

    loop {
        let term_width = console::Term::stdout().size().1 as usize;

        // Move cursor up to overwrite the previous frame.
        if prev_visual_lines > 0 {
            write!(stdout, "\x1b[{prev_visual_lines}A\r")?;
        }

        let mut buf = Vec::new();
        if let Err(e) = render_frame(&mut buf).await {
            writeln!(buf, "\x1b[31mError: {e:#}\x1b[0m")?;
        }

        let output = String::from_utf8_lossy(&buf);

        // Count visual rows accounting for line wrapping.
        let visual_lines: usize = output
            .lines()
            .map(|line| visual_line_count(line, term_width))
            .sum();

        // Clear each line as we write to handle shrinking output.
        for line in output.lines() {
            writeln!(stdout, "\x1b[2K{line}")?;
        }

        // If previous frame had more visual lines, clear the leftover rows.
        if prev_visual_lines > visual_lines {
            for _ in 0..(prev_visual_lines - visual_lines) {
                writeln!(stdout, "\x1b[2K")?;
            }
            // Move cursor back up to end of current frame.
            let extra = prev_visual_lines - visual_lines;
            write!(stdout, "\x1b[{extra}A")?;
        }

        prev_visual_lines = visual_lines;
        stdout.flush()?;

        tokio::select! {
            () = tokio::time::sleep(interval) => {}
            _ = tokio::signal::ctrl_c() => {
                writeln!(stdout)?;
                return Ok(());
            }
        }
    }
}

/// Per-agent data assembled before bucketing.
struct AgentRow {
    agent: AgentId,
    run_state: AgentRunState,
    task: String,
    completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Three buckets used by the watch view, in display order.
#[derive(Default)]
struct Buckets {
    next_up: Vec<AgentRow>,
    running: Vec<AgentRow>,
    completed: Vec<AgentRow>,
}

/// Classify rows by run state. `Idle` rows are dropped — they only appear
/// transiently when marker files have been removed.
fn bucket_agents(rows: Vec<AgentRow>) -> Buckets {
    let mut b = Buckets::default();
    for row in rows {
        match &row.run_state {
            AgentRunState::WaitingForDependencies { .. } => b.next_up.push(row),
            AgentRunState::Running { .. } => b.running.push(row),
            AgentRunState::Finished | AgentRunState::Failed { .. } => b.completed.push(row),
            AgentRunState::Idle => {}
        }
    }
    b
}

/// Sort each bucket so the most actionable rows are at the top.
fn sort_buckets(buckets: &mut Buckets) {
    // Next up: alphabetical by agent name (stable across refreshes).
    buckets.next_up.sort_by(|a, b| a.agent.0.cmp(&b.agent.0));

    // Running: longest-running first — closer to submitting next.
    buckets.running.sort_by(|a, b| {
        let a_e = match &a.run_state {
            AgentRunState::Running { elapsed, .. } => *elapsed,
            _ => Duration::ZERO,
        };
        let b_e = match &b.run_state {
            AgentRunState::Running { elapsed, .. } => *elapsed,
            _ => Duration::ZERO,
        };
        b_e.cmp(&a_e)
    });

    // Recently completed: most recent first; rows without a timestamp last.
    buckets
        .completed
        .sort_by(|a, b| match (a.completed_at, b.completed_at) {
            (Some(a), Some(b)) => b.cmp(&a),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.agent.0.cmp(&b.agent.0),
        });
}

/// Read the completion timestamp from `agent.status`, if present and parseable.
fn read_completed_at(phantom_dir: &Path, agent: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let status_file = agent_monitor::status_path(phantom_dir, agent);
    let content = std::fs::read_to_string(&status_file).ok()?;
    let status: agent_monitor::AgentStatus = serde_json::from_str(&content).ok()?;
    Some(status.completed_at)
}

async fn render_frame(out: &mut impl Write) -> anyhow::Result<()> {
    // Force colors since we write to a buffer that gets flushed to a real terminal.
    console::set_colors_enabled(true);

    let ctx = PhantomContext::locate()?;
    let events_store = ctx.open_events().await?;
    let git = ctx.open_git()?;
    let all_events = events_store.query_all().await?;

    let head = git.head_oid()?;
    let head_short = head.to_hex();
    let head_short = &head_short[..12.min(head_short.len())];

    let now = chrono::Local::now().format("%H:%M:%S");

    // Header
    writeln!(
        out,
        "{} — watching agents  {}",
        console::style("ph background").bold(),
        console::style(format!("{now}  (Ctrl+C to exit)")).dim()
    )?;
    writeln!(out)?;
    writeln!(
        out,
        "{} {}",
        console::style("Trunk HEAD:").dim(),
        console::style(head_short).cyan()
    )?;
    writeln!(out)?;

    let term_width = ui::term_width();

    // Gather background agents — those with any process or wait marker file
    // in their overlay directory. Interactive-only agents are excluded.
    let mut all_agents: Vec<AgentId> = Vec::new();
    collect_background_agents(&ctx.phantom_dir, &mut all_agents);
    all_agents.sort_by(|a, b| a.0.cmp(&b.0));
    all_agents.dedup();

    if all_agents.is_empty() {
        writeln!(
            out,
            "  {}",
            console::style("No agents found. Use `phantom <agent> --background` to start one.")
                .dim()
        )?;
        return Ok(());
    }

    // Build per-agent rows.
    let rows: Vec<AgentRow> = all_agents
        .into_iter()
        .map(|agent| {
            let run_state = status::read_agent_run_state(&ctx.phantom_dir, &agent.0);
            let task = all_events
                .iter()
                .rev()
                .find(|e| e.agent_id == agent && matches!(e.kind, EventKind::TaskCreated { .. }))
                .and_then(|e| match &e.kind {
                    EventKind::TaskCreated { task, .. } if !task.is_empty() => Some(task.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            let completed_at = read_completed_at(&ctx.phantom_dir, &agent.0);
            AgentRow {
                agent,
                run_state,
                task,
                completed_at,
            }
        })
        .collect();

    let mut buckets = bucket_agents(rows);
    sort_buckets(&mut buckets);

    let next_up_count = buckets.next_up.len();
    let running_count = buckets.running.len();
    let (finished_count, failed_count) =
        buckets
            .completed
            .iter()
            .fold((0usize, 0usize), |(f, x), r| match r.run_state {
                AgentRunState::Finished => (f + 1, x),
                AgentRunState::Failed { .. } => (f, x + 1),
                _ => (f, x),
            });

    let mut printed_section = false;
    let now_utc = chrono::Utc::now();

    if !buckets.next_up.is_empty() {
        render_section_header(out, "Next up", next_up_count, "WAITING ON")?;
        for row in &buckets.next_up {
            render_next_up_row(out, row, term_width)?;
        }
        printed_section = true;
    }

    if !buckets.running.is_empty() {
        if printed_section {
            writeln!(out)?;
        }
        render_section_header(out, "Running", running_count, "ELAPSED")?;
        for row in &buckets.running {
            render_running_row(out, row, term_width)?;
        }
        printed_section = true;
    }

    if !buckets.completed.is_empty() {
        if printed_section {
            writeln!(out)?;
        }
        render_section_header(
            out,
            "Recently completed",
            buckets.completed.len(),
            "FINISHED",
        )?;
        for row in &buckets.completed {
            render_completed_row(out, row, term_width, now_utc)?;
        }
    }

    writeln!(out)?;
    write!(out, "  ")?;
    if next_up_count > 0 {
        write!(
            out,
            "{}  ",
            console::style(format!("◌ {next_up_count} next up")).cyan()
        )?;
    }
    if running_count > 0 {
        write!(
            out,
            "{}  ",
            console::style(format!("● {running_count} running")).yellow()
        )?;
    }
    if finished_count > 0 {
        write!(
            out,
            "{}  ",
            console::style(format!("✓ {finished_count} finished")).green()
        )?;
    }
    if failed_count > 0 {
        write!(
            out,
            "{}  ",
            console::style(format!("✗ {failed_count} failed")).red()
        )?;
    }
    writeln!(out)?;

    Ok(())
}

/// Print a section header (title with count) and its column header row.
fn render_section_header(
    out: &mut impl Write,
    title: &str,
    count: usize,
    third_col: &str,
) -> io::Result<()> {
    writeln!(
        out,
        "  {} {}",
        console::style("▼").dim(),
        console::style(format!("{title} ({count})")).bold()
    )?;
    writeln!(
        out,
        "  {}",
        console::style(format!(
            "{:<16} {:<14} {:<18} TASK",
            "AGENT", "STATUS", third_col
        ))
        .dim()
    )?;
    let rule_len = ui::term_width().min(80).saturating_sub(2);
    writeln!(out, "  {}", console::style("─".repeat(rule_len)).dim())?;
    Ok(())
}

/// Width of the row prefix preceding the TASK column (matches the section
/// header layout: 2-space indent + 16 + 1 + 2 + 1 + 12 + 1 + 18 + 1).
const TASK_PREFIX_WIDTH: usize = 2 + 16 + 1 + 2 + 1 + 12 + 1 + 18 + 1;

fn render_next_up_row(out: &mut impl Write, row: &AgentRow, term_width: usize) -> io::Result<()> {
    let upstream = match &row.run_state {
        AgentRunState::WaitingForDependencies { upstream } => upstream.join(", "),
        _ => String::new(),
    };
    let upstream_cell = ui::truncate_line(&upstream, 18);
    let truncated_task = ui::truncate_line(&row.task, term_width.saturating_sub(TASK_PREFIX_WIDTH));
    writeln!(
        out,
        "  {:<16} {} {:<12} {:<18} {}",
        row.agent.0,
        ui::run_state_indicator(&row.run_state),
        ui::run_state_text(&row.run_state),
        upstream_cell,
        console::style(truncated_task).dim()
    )
}

fn render_running_row(out: &mut impl Write, row: &AgentRow, term_width: usize) -> io::Result<()> {
    let elapsed = match &row.run_state {
        AgentRunState::Running { elapsed, .. } => status::format_duration(elapsed),
        _ => String::new(),
    };
    let truncated_task = ui::truncate_line(&row.task, term_width.saturating_sub(TASK_PREFIX_WIDTH));
    writeln!(
        out,
        "  {:<16} {} {:<12} {:<18} {}",
        row.agent.0,
        ui::run_state_indicator(&row.run_state),
        ui::run_state_text(&row.run_state),
        elapsed,
        console::style(truncated_task).dim()
    )
}

fn render_completed_row(
    out: &mut impl Write,
    row: &AgentRow,
    term_width: usize,
    now: chrono::DateTime<chrono::Utc>,
) -> io::Result<()> {
    let finished = row
        .completed_at
        .map(|ts| ui::format_relative_time(ts, now))
        .unwrap_or_default();
    let truncated_task = ui::truncate_line(&row.task, term_width.saturating_sub(TASK_PREFIX_WIDTH));
    writeln!(
        out,
        "  {:<16} {} {:<12} {:<18} {}",
        row.agent.0,
        ui::run_state_indicator(&row.run_state),
        ui::run_state_text(&row.run_state),
        finished,
        console::style(truncated_task).dim()
    )
}

/// Scan `.phantom/overlays/` for agent directories that were launched as
/// background agents. Includes agents that are still waiting on upstream
/// dependencies (`waiting.json` or `monitor.pid` only) — without this, the
/// "Next up" section would be invisible. Interactive-only agents are excluded.
fn collect_background_agents(phantom_dir: &Path, agents: &mut Vec<AgentId>) {
    let overlays_dir = phantom_dir.join("overlays");
    if let Ok(entries) = std::fs::read_dir(&overlays_dir) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir())
                && let Some(name) = entry.file_name().to_str()
            {
                let dir = entry.path();
                let is_background = dir.join("agent.pid").exists()
                    || dir.join("agent.status").exists()
                    || dir.join("waiting.json").exists()
                    || dir.join("monitor.pid").exists();
                if is_background {
                    let id = AgentId(name.to_string());
                    if !agents.contains(&id) {
                        agents.push(id);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_row(name: &str, state: AgentRunState) -> AgentRow {
        AgentRow {
            agent: AgentId(name.to_string()),
            run_state: state,
            task: String::new(),
            completed_at: None,
        }
    }

    #[test]
    fn collect_background_agents_includes_waiting_only_overlay() {
        let tmp = TempDir::new().unwrap();
        let overlay = tmp.path().join("overlays").join("bravo");
        fs::create_dir_all(&overlay).unwrap();
        // Only waiting.json + monitor.pid — no agent.pid, no agent.status.
        fs::write(overlay.join("waiting.json"), "[\"alpha\"]").unwrap();
        crate::pid_guard::write_pid_file(&overlay.join("monitor.pid"), i32::MAX).unwrap();

        let mut agents = Vec::new();
        collect_background_agents(tmp.path(), &mut agents);
        assert_eq!(agents, vec![AgentId("bravo".into())]);
    }

    #[test]
    fn bucket_classification_separates_states() {
        let rows = vec![
            make_row(
                "wait-1",
                AgentRunState::WaitingForDependencies {
                    upstream: vec!["alpha".into()],
                },
            ),
            make_row(
                "run-1",
                AgentRunState::Running {
                    pid: 1,
                    elapsed: Duration::from_secs(10),
                },
            ),
            make_row(
                "run-2",
                AgentRunState::Running {
                    pid: 2,
                    elapsed: Duration::from_secs(20),
                },
            ),
            make_row("done-1", AgentRunState::Finished),
            make_row("fail-1", AgentRunState::Failed { status: None }),
            make_row("idle-1", AgentRunState::Idle),
        ];

        let buckets = bucket_agents(rows);
        assert_eq!(buckets.next_up.len(), 1);
        assert_eq!(buckets.running.len(), 2);
        assert_eq!(buckets.completed.len(), 2); // Finished + Failed; Idle dropped
    }

    #[test]
    fn running_sorted_by_longest_elapsed_first() {
        let mut buckets = bucket_agents(vec![
            make_row(
                "short",
                AgentRunState::Running {
                    pid: 1,
                    elapsed: Duration::from_secs(5),
                },
            ),
            make_row(
                "long",
                AgentRunState::Running {
                    pid: 2,
                    elapsed: Duration::from_mins(1),
                },
            ),
            make_row(
                "mid",
                AgentRunState::Running {
                    pid: 3,
                    elapsed: Duration::from_secs(30),
                },
            ),
        ]);
        sort_buckets(&mut buckets);
        let order: Vec<&str> = buckets.running.iter().map(|r| r.agent.0.as_str()).collect();
        assert_eq!(order, vec!["long", "mid", "short"]);
    }

    #[test]
    fn completed_sorted_most_recent_first() {
        let now = chrono::Utc::now();
        let mut buckets = Buckets::default();
        buckets.completed.push(AgentRow {
            agent: AgentId("oldest".into()),
            run_state: AgentRunState::Finished,
            task: String::new(),
            completed_at: Some(now - chrono::Duration::hours(1)),
        });
        buckets.completed.push(AgentRow {
            agent: AgentId("newest".into()),
            run_state: AgentRunState::Finished,
            task: String::new(),
            completed_at: Some(now - chrono::Duration::seconds(5)),
        });
        buckets.completed.push(AgentRow {
            agent: AgentId("untimed".into()),
            run_state: AgentRunState::Finished,
            task: String::new(),
            completed_at: None,
        });

        sort_buckets(&mut buckets);
        let order: Vec<&str> = buckets
            .completed
            .iter()
            .map(|r| r.agent.0.as_str())
            .collect();
        assert_eq!(order, vec!["newest", "oldest", "untimed"]);
    }

    #[test]
    fn next_up_row_contains_upstream_names() {
        let row = make_row(
            "downstream",
            AgentRunState::WaitingForDependencies {
                upstream: vec!["alpha".into(), "bravo".into()],
            },
        );
        let mut buf: Vec<u8> = Vec::new();
        // Disable colors so the rendered text is easy to inspect.
        console::set_colors_enabled(false);
        render_next_up_row(&mut buf, &row, 120).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("downstream"), "missing agent name: {s:?}");
        assert!(s.contains("alpha, bravo"), "missing upstream list: {s:?}");
    }
}
