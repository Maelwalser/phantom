//! Static per-category discipline rules injected into agent system prompts.
//!
//! Mirrors the shape of [`crate::context_file::resolve::write_resolve_rules_file`]:
//! each rule file is 100% static content so it becomes part of the cached
//! system-prompt prefix across every session of the same category. The CLI
//! picks the right file based on the task's [`TaskCategory`] and passes it to
//! `claude --append-system-prompt-file`.
//!
//! Rules encode adversarial clauses that LLMs cannot rationalise away: explicit
//! ordering requirements, read-only constraints, escalation markers, and
//! pattern-mirroring requirements. Each rule body ends with a rejection-at-
//! merge-time footer; see the crate README for the roadmap that makes that
//! footer literally enforceable.

use std::path::{Path, PathBuf};

use anyhow::Context;
use phantom_core::TaskCategory;

/// Directory (relative to `.phantom/`) where per-category rule files live.
pub const RULES_DIR: &str = "rules";

/// Resolve the absolute path of the rules file for a given category inside a
/// phantom directory. Does NOT check that the file exists.
pub fn rules_path(phantom_dir: &Path, category: TaskCategory) -> PathBuf {
    phantom_dir
        .join(RULES_DIR)
        .join(format!("{}.md", category.as_str()))
}

/// Write the static rules markdown body for `category` to `path`. Idempotent:
/// overwrites byte-identically each call so prompt caches stay warm.
pub fn write_category_rules_file(path: &Path, category: TaskCategory) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }
    std::fs::write(path, rules_body(category))
        .with_context(|| format!("failed to write {} rules to {}", category, path.display()))?;
    Ok(())
}

/// Ensure `.phantom/rules/` exists and contains the four static rule files.
/// Returns the directory path. Safe to call on every task creation.
pub fn ensure_category_rules_dir(phantom_dir: &Path) -> anyhow::Result<PathBuf> {
    let dir = phantom_dir.join(RULES_DIR);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create rules directory {}", dir.display()))?;
    for cat in TaskCategory::ALL {
        write_category_rules_file(&rules_path(phantom_dir, cat), cat)?;
    }
    Ok(dir)
}

/// Return the static markdown body for a category. Public so callers that need
/// to compose category rules into a larger instruction file (e.g. the plan
/// dispatcher) can prepend the body without going through the filesystem.
pub fn rules_body(category: TaskCategory) -> &'static str {
    match category {
        TaskCategory::Corrective => CORRECTIVE,
        TaskCategory::Perfective => PERFECTIVE,
        TaskCategory::Preventive => PREVENTIVE,
        TaskCategory::Adaptive => ADAPTIVE,
    }
}

const CORRECTIVE: &str = "\
# Phantom Task Rules: Corrective (bug fix)

This task is a bug fix. You MUST follow the ordering below — not in spirit, in
literal sequence.

## Required ordering

1. Read the bug description and form a hypothesis about the faulty code path.
2. Write a test that exercises that path and asserts the correct behaviour.
3. Run the test. It MUST FAIL against the unmodified code. Capture the exact
   failure output.
4. Only after step 3 succeeds: modify application code to fix the bug.
5. Re-run the test. It MUST PASS. Also run the project's full test suite.
6. Submit.

## Unreproducibility clause

If you cannot produce a test that fails against the unmodified code, STOP.
Do not guess at a fix. Emit a `PHANTOM_UNREPRODUCIBLE:` note at the top of
`.phantom-task.md` explaining what you tried, and exit without modifying
application code. A submission without a reproduction is not a bug fix; it is
a speculative patch and will be rejected.

## Band-aid clause

If the root cause lies outside the file or module where the symptom surfaces,
you MUST fix it at the root cause. Do not patch at the symptom site. If the
root cause crosses module boundaries and the fix feels architecturally large,
surface a `PHANTOM_ESCALATION:` note describing (a) the root cause, (b) the
minimal fix, and (c) the surface area of the change, then make the minimal
change at the root cause.

## Test integrity

You MUST NOT weaken existing assertions, delete existing tests, or relax
existing test input ranges to make the fix appear to work. If an existing
test legitimately encoded the buggy behaviour, update it AND add a comment
explaining what the buggy behaviour was and why the new behaviour is correct.

Failure to follow these rules will cause your submission to be rejected at
semantic merge time.
";

const PERFECTIVE: &str = "\
# Phantom Task Rules: Perfective (refactor / performance / cleanup)

This task is a refactor, performance change, or cleanup. The code under test
must behave identically before and after — tests are your contract.

## Read-only tests (HARD CONSTRAINT)

You MUST NOT modify any file whose primary purpose is testing. That includes,
but is not limited to:

- Any path matching `tests/`, `**/tests.rs`, `**/*_test.rs`, `**/*_tests.rs`
- Any path matching `**/*.test.ts`, `**/*.test.tsx`, `**/*.test.js`
- Any `__tests__/` directory
- Any file whose body is dominated by `#[test]`, `#[tokio::test]`, `it(...)`,
  `describe(...)`, `def test_...`, or `func Test...`

If your refactor would require a test change, the refactor is changing
observable behaviour. STOP and emit a `PHANTOM_TEST_CONTRACT_CHANGE:` note
describing the observable change and why it is intentional. Do not modify the
test to make the refactor pass.

## Measurement clause

If this task is motivated by performance, you MUST have a benchmark. If one
does not exist, add it in a separate first commit (Criterion for Rust; the
project's established harness otherwise). Record before-and-after numbers in
your submit description. A `performance` refactor without numbers is rejected.

## Characterization clause

Before refactoring a module with thin test coverage (< 60% line coverage, or
fewer than three meaningful test cases if no coverage tool is configured),
you MUST write characterization tests in a separate first commit that pin
down the module's current behaviour — including weird behaviour you do not
intend to fix. Without this step, the refactor will erase edge cases that
were implicit in the old implementation. Characterization tests go in the
test directory; they do not violate the read-only-tests rule because they
are new additions, not modifications of existing tests.

## Scope discipline

Only touch files that are load-bearing for the stated refactor. If you notice
unrelated issues, surface them as `PHANTOM_FOLLOWUP:` notes instead of fixing
them opportunistically.

Failure to follow these rules will cause your submission to be rejected at
semantic merge time.
";

const PREVENTIVE: &str = "\
# Phantom Task Rules: Preventive (test hardening)

This task is about adding or strengthening tests. Source code is read-only.

## Source-test isolation (HARD CONSTRAINT)

You MUST NOT modify any file outside of test directories and test modules.
Specifically forbidden even under a 'testability' justification:

- Adding `pub` (or `pub(crate)`) to any private item to allow tests to call it
- Inserting a `#[cfg(test)]` hook, accessor, or constructor into source code
- Extracting a private function to module level, crate level, or across files
- Introducing a new trait purely to mock an internal dependency
- Adding a `#[derive(...)]` purely to satisfy a test's need to compare values
- Splitting a file to expose previously-private items

If the code under test is genuinely untestable from its current shape, STOP
and emit a `PHANTOM_REFACTOR_REQUIRED:` note describing the minimum source
change that would make the code testable, then exit without modifying source.
A separate perfective task will make the source change; this task will not.

## Negative-space requirement

Happy-path-only test sets are rejected. At least 60% of your new test cases
(by count) must target:

- Boundary conditions (empty, zero, one, max, overflow, off-by-one)
- Error paths (every `Err(_)` return, every `panic!`, every `Option::None`)
- Invariants under stress (concurrent access, interrupted operations,
  partial writes, poisoned locks, reentrancy, unicode / non-ASCII input)
- Regression cases (any bug fixed in the last N commits whose regression
  is not already covered)

Include at least one negative test that asserts a specific error TYPE or
message, not just `assert!(result.is_err())`.

## Test quality

- Tests must be deterministic. No wall-clock, no unseeded randomness, no
  flaky network / filesystem dependencies.
- Each test asserts one concrete behaviour. Split tests that assert multiple
  unrelated properties.
- Test names describe the scenario: `rejects_invalid_email`, not `test_email`.

Failure to follow these rules will cause your submission to be rejected at
semantic merge time.
";

const ADAPTIVE: &str = "\
# Phantom Task Rules: Adaptive (new feature / extension)

This task adds or extends a feature. The biggest risk is inventing a new
pattern when an established one already exists.

## Pattern-mirroring clause (READ BEFORE CODING)

Before writing any code:

1. Locate the most similar existing feature in this codebase. 'Similar' means:
   same subsystem / similar I/O shape / similar persistence model / similar
   test harness. Not 'shares a word in the name.'
2. Open it. Identify its:
   - File layout (where does the public API live, where do internals live,
     where do tests live)
   - Error type and propagation style (`thiserror` enum, `anyhow`, custom)
   - Logging conventions (tracing spans? log levels? structured fields?)
   - Test structure (unit vs integration, fixtures, mocks, testkit helpers)
   - Public vs private visibility conventions
3. Mirror them. Use the same module layout, the same error shape, the same
   logging fields, the same test harness.
4. Cite the reference feature by file path in your submit description:
   'Mirrored from `crates/phantom-foo/src/bar.rs`.'

If no sufficiently similar feature exists (you looked and the codebase
genuinely has no precedent), STOP. Emit a `PHANTOM_ARCHITECTURE_REQUIRED:`
note describing the shape you would propose and exit. A human will establish
the precedent; you will not invent one unilaterally.

## No drive-bys

You MUST NOT modify any file that is not either (a) listed in the task's
file scope or (b) strictly required to make the feature compile and its
tests pass. Unrelated cleanup, renames, doc fixes, 'while I was here'
improvements, and formatter runs on untouched files are forbidden. If you
spot something worth doing, surface it as a `PHANTOM_FOLLOWUP:` note.

## Dependency restraint

Adding a new top-level dependency (a new entry in `Cargo.toml`,
`package.json`, `pyproject.toml`, `go.mod`, etc.) requires a
`PHANTOM_NEW_DEPENDENCY:` note justifying (a) why the stdlib or existing
deps cannot do it, (b) why this specific crate, (c) the maintenance and
licence posture.

## Public surface

Do not make items `pub` unless they are genuinely part of the crate's
public API. Prefer `pub(crate)` for internal sharing. Match the precedent
feature's visibility style.

Failure to follow these rules will cause your submission to be rejected at
semantic merge time.
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_body_is_nonempty_for_all_categories() {
        for cat in TaskCategory::ALL {
            let body = rules_body(cat);
            assert!(!body.is_empty(), "category {cat} has empty body");
            assert!(
                body.starts_with("# Phantom Task Rules:"),
                "category {cat} must start with a level-1 header"
            );
            assert!(
                body.contains("Failure to follow these rules"),
                "category {cat} must contain the rejection footer"
            );
        }
    }

    #[test]
    fn rules_body_category_matches_content() {
        assert!(rules_body(TaskCategory::Corrective).contains("Corrective (bug fix)"));
        assert!(rules_body(TaskCategory::Perfective).contains("Perfective (refactor"));
        assert!(rules_body(TaskCategory::Preventive).contains("Preventive (test hardening)"));
        assert!(rules_body(TaskCategory::Adaptive).contains("Adaptive (new feature"));
    }

    #[test]
    fn rules_body_contains_key_adversarial_clauses() {
        assert!(rules_body(TaskCategory::Corrective).contains("PHANTOM_UNREPRODUCIBLE:"));
        assert!(rules_body(TaskCategory::Corrective).contains("PHANTOM_ESCALATION:"));
        assert!(rules_body(TaskCategory::Perfective).contains("PHANTOM_TEST_CONTRACT_CHANGE:"));
        assert!(rules_body(TaskCategory::Preventive).contains("PHANTOM_REFACTOR_REQUIRED:"));
        assert!(rules_body(TaskCategory::Adaptive).contains("PHANTOM_ARCHITECTURE_REQUIRED:"));
    }

    #[test]
    fn write_category_rules_file_is_byte_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrective.md");

        write_category_rules_file(&path, TaskCategory::Corrective).unwrap();
        let first = std::fs::read(&path).unwrap();

        write_category_rules_file(&path, TaskCategory::Corrective).unwrap();
        let second = std::fs::read(&path).unwrap();

        assert_eq!(
            first, second,
            "rules file must be byte-identical across writes"
        );
    }

    #[test]
    fn ensure_category_rules_dir_writes_all_four() {
        let dir = tempfile::tempdir().unwrap();
        let phantom_dir = dir.path();

        let rules_dir = ensure_category_rules_dir(phantom_dir).unwrap();
        assert_eq!(rules_dir, phantom_dir.join(RULES_DIR));

        for cat in TaskCategory::ALL {
            let path = rules_path(phantom_dir, cat);
            assert!(path.exists(), "missing rules file for {cat}");
            let body = std::fs::read_to_string(&path).unwrap();
            assert!(body.contains(&format!("{cat}").to_string()) || !body.is_empty());
        }
    }

    #[test]
    fn ensure_category_rules_dir_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let phantom_dir = dir.path();

        ensure_category_rules_dir(phantom_dir).unwrap();
        let before: Vec<Vec<u8>> = TaskCategory::ALL
            .iter()
            .map(|c| std::fs::read(rules_path(phantom_dir, *c)).unwrap())
            .collect();

        ensure_category_rules_dir(phantom_dir).unwrap();
        let after: Vec<Vec<u8>> = TaskCategory::ALL
            .iter()
            .map(|c| std::fs::read(rules_path(phantom_dir, *c)).unwrap())
            .collect();

        assert_eq!(before, after);
    }

    #[test]
    fn rules_path_uses_lowercase_filename() {
        let dir = std::path::Path::new("/tmp/phantom");
        assert_eq!(
            rules_path(dir, TaskCategory::Corrective),
            std::path::PathBuf::from("/tmp/phantom/rules/corrective.md")
        );
        assert_eq!(
            rules_path(dir, TaskCategory::Adaptive),
            std::path::PathBuf::from("/tmp/phantom/rules/adaptive.md")
        );
    }
}
