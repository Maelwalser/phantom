//! `ph verify` — run the project's build / lint / test commands as a
//! post-plan gate.
//!
//! The typical flow is: an operator runs `ph plan`, waits for
//! `ph background` to report all agents done, then runs `ph verify` to
//! confirm the workspace still compiles and tests pass. Without this
//! gate a domain can silently submit incomplete work — the observed
//! lessdb run had a CLI agent declare a `hook_install` subcommand in
//! `plan.json` and never implement it; `cargo test -p lessdb-cli` (its
//! own declared verification command) would have caught the missing
//! file.
//!
//! Commands come from [`phantom_toolchain::Toolchain`], which
//! auto-detects Rust / Node / Python / Go / JVM / Ruby / Elixir from
//! sentinel files. The command runs in sequence — build, lint, test —
//! and stops on the first failure so the operator can act without
//! waiting for the rest. Exit code mirrors Unix convention: 0 when
//! every command passes, non-zero on the first failure.

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::Context;
use phantom_toolchain::{Toolchain, ToolchainDetector, VerificationVerb};

use crate::context::PhantomContext;
use crate::ui;

#[derive(clap::Args)]
#[allow(clippy::struct_excessive_bools)] // Each flag is independent; a
// state-machine refactor would obscure the clap-generated --help.
pub struct VerifyArgs {
    /// Skip the build step.
    #[arg(long)]
    pub skip_build: bool,
    /// Skip the linter step.
    #[arg(long)]
    pub skip_lint: bool,
    /// Skip the test step.
    #[arg(long)]
    pub skip_tests: bool,
    /// Run every step even after the first failure.
    #[arg(long)]
    pub no_fail_fast: bool,
}

// async signature is kept for CLI uniformity — every other subcommand's
// `run` is async, so making this one sync would require a special case
// in main.rs's match arm.
#[allow(clippy::unused_async)]
pub async fn run(args: VerifyArgs) -> anyhow::Result<()> {
    let ctx = PhantomContext::locate()
        .context("`ph verify` must be run inside an initialised phantom repo")?;

    let detector = ToolchainDetector::new();
    let toolchain = detector.detect_repo_root(&ctx.repo_root);

    if toolchain.is_empty() {
        ui::warning_message(
            "no recognised toolchain sentinel found (Cargo.toml, package.json, go.mod, …) — \
             nothing to verify",
        );
        return Ok(());
    }

    let plan = build_plan(&toolchain, &args);
    if plan.is_empty() {
        ui::warning_message("every verification step was skipped by flag — nothing to run");
        return Ok(());
    }

    let mut failures: Vec<String> = Vec::new();
    for step in &plan {
        let outcome = run_step(&ctx.repo_root, step);
        match outcome {
            StepOutcome::Passed => {
                println!(
                    "  {} {} {}",
                    console::style("✓").green().bold(),
                    console::style(step.verb.human_label()).bold(),
                    console::style(&step.command).dim(),
                );
            }
            StepOutcome::Failed { exit_code } => {
                println!(
                    "  {} {} {} {}",
                    console::style("✗").red().bold(),
                    console::style(step.verb.human_label()).bold(),
                    console::style(&step.command).dim(),
                    console::style(format!("(exit {exit_code})")).red(),
                );
                failures.push(step.command.clone());
                if !args.no_fail_fast {
                    break;
                }
            }
        }
    }

    if failures.is_empty() {
        println!(
            "\n{}",
            console::style(format!("{} verification step(s) passed", plan.len()))
                .green()
                .bold()
        );
        Ok(())
    } else {
        anyhow::bail!(
            "verification gate failed: {} step(s) failed",
            failures.len()
        )
    }
}

struct VerificationStep {
    verb: VerificationVerb,
    command: String,
}

fn build_plan(toolchain: &Toolchain, args: &VerifyArgs) -> Vec<VerificationStep> {
    let mut plan = Vec::new();
    let push_if = |plan: &mut Vec<VerificationStep>, verb: VerificationVerb, skip: bool| {
        if skip {
            return;
        }
        if let Some(cmd) = toolchain.command_for(verb) {
            plan.push(VerificationStep {
                verb,
                command: cmd.to_string(),
            });
        }
    };

    // Build first (fastest signal), then lint, then tests. Typecheck and
    // format are deliberately omitted — they're mostly subsumed by the
    // lint/build steps and tend to surface style noise rather than real
    // regressions.
    push_if(&mut plan, VerificationVerb::VerifyBuild, args.skip_build);
    push_if(&mut plan, VerificationVerb::RunLinter, args.skip_lint);
    push_if(&mut plan, VerificationVerb::RunTests, args.skip_tests);
    plan
}

enum StepOutcome {
    Passed,
    Failed { exit_code: i32 },
}

/// Spawn a verification command as a child process. Output streams directly
/// through to the user's TTY so they see cargo's or npm's native progress
/// instead of a buffered dump at the end.
fn run_step(cwd: &Path, step: &VerificationStep) -> StepOutcome {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(&step.command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    match cmd.status() {
        Ok(status) if status.success() => StepOutcome::Passed,
        Ok(status) => StepOutcome::Failed {
            exit_code: status.code().unwrap_or(-1),
        },
        Err(e) => {
            eprintln!(
                "{} failed to spawn `{}`: {e}",
                console::style("✗").red().bold(),
                step.command,
            );
            StepOutcome::Failed { exit_code: -1 }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phantom_toolchain::DetectedLanguage;

    fn rust_toolchain() -> Toolchain {
        Toolchain {
            language: Some(DetectedLanguage::Rust),
            test_cmd: Some("cargo test".into()),
            build_cmd: Some("cargo build".into()),
            lint_cmd: Some("cargo clippy -- -D warnings".into()),
            typecheck_cmd: None,
            format_check_cmd: Some("cargo fmt --check".into()),
        }
    }

    fn all_args() -> VerifyArgs {
        VerifyArgs {
            skip_build: false,
            skip_lint: false,
            skip_tests: false,
            no_fail_fast: false,
        }
    }

    #[test]
    fn plan_orders_build_lint_test() {
        let plan = build_plan(&rust_toolchain(), &all_args());
        assert_eq!(plan.len(), 3);
        assert_eq!(plan[0].verb, VerificationVerb::VerifyBuild);
        assert_eq!(plan[1].verb, VerificationVerb::RunLinter);
        assert_eq!(plan[2].verb, VerificationVerb::RunTests);
    }

    #[test]
    fn plan_respects_skip_flags() {
        let args = VerifyArgs {
            skip_build: true,
            skip_lint: false,
            skip_tests: true,
            no_fail_fast: false,
        };
        let plan = build_plan(&rust_toolchain(), &args);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].verb, VerificationVerb::RunLinter);
    }

    #[test]
    fn plan_skips_commands_the_toolchain_does_not_provide() {
        let toolchain = Toolchain {
            language: Some(DetectedLanguage::Rust),
            test_cmd: None,
            build_cmd: Some("cargo build".into()),
            lint_cmd: None,
            typecheck_cmd: None,
            format_check_cmd: None,
        };
        let plan = build_plan(&toolchain, &all_args());
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].command, "cargo build");
    }

    #[test]
    fn plan_empty_when_toolchain_is_empty() {
        assert!(build_plan(&Toolchain::empty(), &all_args()).is_empty());
    }
}
