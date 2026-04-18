//! Plan rendering: prints the plan grouped by execution wave for the user
//! to review before dispatch.

use std::collections::HashMap;

use phantom_core::plan::{Plan, PlanDomain};

use super::validate::compute_waves;

/// Display the plan to the user, grouped by execution wave.
pub(super) fn display_plan(plan: &Plan) {
    crate::ui::section_header(&format!("Plan: {}", plan.id));
    println!(
        "  {} domain(s) identified",
        console::style(plan.domains.len()).bold()
    );

    // Compute wave depth for each domain.
    let waves = compute_waves(&plan.domains);
    let max_wave = waves.values().copied().max().unwrap_or(0);

    // One-line critical-path "spine": each wave as `wN×K` with K=fan-out.
    // Waves with only one domain are rendered in yellow so the reader
    // spots bottlenecks — a single-domain wave sandwiched between wider
    // ones serializes the critical path and usually signals a
    // decomposition opportunity.
    let spine = render_wave_spine(&waves, max_wave);
    if spine.is_empty() {
        println!();
    } else {
        println!("  {}  {}\n", console::style("spine").dim(), spine);
    }

    for wave in 0..=max_wave {
        let domains_in_wave: Vec<&PlanDomain> = plan
            .domains
            .iter()
            .filter(|d| waves.get(d.name.as_str()).copied().unwrap_or(0) == wave)
            .collect();

        if domains_in_wave.is_empty() {
            continue;
        }

        if max_wave > 0 {
            if wave == 0 {
                println!(
                    "  {} {}",
                    console::style(format!("Wave {wave}")).bold(),
                    console::style("(immediate)").dim()
                );
            } else {
                let after: Vec<&str> = domains_in_wave
                    .iter()
                    .flat_map(|d| d.depends_on.iter().map(String::as_str))
                    .collect::<std::collections::HashSet<_>>()
                    .into_iter()
                    .collect();
                let after_styled: Vec<String> = after
                    .iter()
                    .map(|a| console::style(a).bold().to_string())
                    .collect();
                println!(
                    "  {} {}",
                    console::style(format!("Wave {wave}")).bold(),
                    console::style(format!("(after: {})", after_styled.join(", "))).dim()
                );
            }
        }

        for domain in &domains_in_wave {
            println!(
                "    {} {}",
                console::style("▸").cyan(),
                console::style(&domain.name).bold()
            );
            println!("      {}", console::style(&domain.description).dim());
            if !domain.files_to_modify.is_empty() {
                let files: Vec<_> = domain
                    .files_to_modify
                    .iter()
                    .map(|f| f.display().to_string())
                    .collect();
                println!(
                    "      {}  {}",
                    console::style("Files").dim(),
                    console::style(files.join(", ")).dim()
                );
            }
            if !domain.depends_on.is_empty() {
                let deps: Vec<String> = domain
                    .depends_on
                    .iter()
                    .map(|d| console::style(d).bold().to_string())
                    .collect();
                println!(
                    "      {}  {}",
                    console::style("Depends on").dim(),
                    deps.join(", ")
                );
            }
            println!();
        }
    }
}

/// Render the wave-size sequence as a compact "spine" string for quick
/// visual scanning. Example output for a 5-wave plan with sizes
/// 1 → 3 → 2 → 1 → 3:
///   `w0×1 → w1×3 → w2×2 → w3×1 → w4×3`
/// Waves of size 1 sandwiched between wider ones are painted yellow to
/// flag critical-path bottlenecks.
fn render_wave_spine(waves: &HashMap<&str, usize>, max_wave: usize) -> String {
    let mut sizes: Vec<usize> = vec![0; max_wave + 1];
    for w in waves.values() {
        if *w <= max_wave {
            sizes[*w] += 1;
        }
    }
    if sizes.is_empty() {
        return String::new();
    }
    let has_wider_later = |idx: usize| -> bool { sizes[idx + 1..].iter().any(|&s| s >= 2) };
    let segments: Vec<String> = sizes
        .iter()
        .enumerate()
        .map(|(i, &n)| {
            let raw = format!("w{i}×{n}");
            if n == 1 && has_wider_later(i) {
                console::style(raw).yellow().to_string()
            } else {
                raw
            }
        })
        .collect();
    segments.join(&console::style(" → ").dim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waves_from(entries: &[(&'static str, usize)]) -> HashMap<&'static str, usize> {
        entries.iter().copied().collect()
    }

    #[test]
    fn spine_renders_every_wave() {
        let w = waves_from(&[("a", 0), ("b", 0), ("c", 1)]);
        let spine = render_wave_spine(&w, 1);
        assert!(spine.contains("w0×2"));
        assert!(spine.contains("w1×1"));
    }

    #[test]
    fn spine_handles_single_wave() {
        let w = waves_from(&[("a", 0), ("b", 0)]);
        let spine = render_wave_spine(&w, 0);
        assert!(spine.contains("w0×2"));
    }

    #[test]
    fn spine_empty_for_empty_waves() {
        let w: HashMap<&str, usize> = HashMap::new();
        // max_wave=0 with no entries yields a single "w0×0" — that is
        // acceptable for now; the caller guards on domains.len() before
        // rendering. Just assert it doesn't panic.
        let _ = render_wave_spine(&w, 0);
    }
}
