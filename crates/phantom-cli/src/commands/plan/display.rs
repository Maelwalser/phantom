//! Plan rendering: prints the plan grouped by execution wave for the user
//! to review before dispatch.

use phantom_core::plan::{Plan, PlanDomain};

use super::validate::compute_waves;

/// Display the plan to the user, grouped by execution wave.
pub(super) fn display_plan(plan: &Plan) {
    crate::ui::section_header(&format!("Plan: {}", plan.id));
    println!(
        "  {} domain(s) identified\n",
        console::style(plan.domains.len()).bold()
    );

    // Compute wave depth for each domain.
    let waves = compute_waves(&plan.domains);
    let max_wave = waves.values().copied().max().unwrap_or(0);

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
