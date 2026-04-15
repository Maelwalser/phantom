//! Dockerfile symbol extraction via line-based parsing.
//!
//! No tree-sitter grammar is used because the `tree-sitter-dockerfile` crate
//! depends on an incompatible tree-sitter version (0.20 vs our 0.25).
//!
//! Instead, we parse Dockerfile instructions directly from text. `FROM`
//! instructions become `Section` symbols (one per build stage) and other
//! instructions become `Directive` symbols scoped to their stage.

use std::path::Path;

use phantom_core::symbol::{SymbolEntry, SymbolKind};

use super::LanguageExtractor;

/// Extracts symbols from Dockerfile files via line-based parsing.
///
/// This extractor does **not** use a tree-sitter grammar. It implements
/// [`LanguageExtractor`] by constructing a trivial single-node tree that is
/// ignored in `extract_symbols`; the real work is done by scanning raw text.
pub struct DockerfileExtractor;

impl LanguageExtractor for DockerfileExtractor {
    fn language(&self) -> tree_sitter::Language {
        // We need *some* grammar for the Parser to call set_language. Use bash
        // as a stand-in — it doesn't matter because extract_symbols ignores the
        // tree entirely and parses the raw source.
        tree_sitter_bash::LANGUAGE.into()
    }

    fn extensions(&self) -> &[&str] {
        &["dockerfile"]
    }

    fn filenames(&self) -> &[&str] {
        &["Dockerfile"]
    }

    fn extract_symbols(
        &self,
        _tree: &tree_sitter::Tree,
        source: &[u8],
        file_path: &Path,
    ) -> Vec<SymbolEntry> {
        let text = match std::str::from_utf8(source) {
            Ok(t) => t,
            Err(_) => return Vec::new(),
        };
        extract_dockerfile_symbols(text, source, file_path)
    }
}

/// Parse Dockerfile text and extract symbols from instruction lines.
fn extract_dockerfile_symbols(
    text: &str,
    source: &[u8],
    file_path: &Path,
) -> Vec<SymbolEntry> {
    let mut symbols = Vec::new();
    let mut current_stage = "global".to_string();
    let mut directive_idx: usize = 0;

    // Track instruction spans: each instruction starts at its keyword and
    // extends to the byte before the next instruction or EOF.
    let instructions = collect_instructions(text);

    for instr in &instructions {
        let keyword = instr.keyword.to_uppercase();
        let start = instr.start_byte;
        let end = instr.end_byte;

        // Build a fake node-like span by slicing source directly.
        // We use push_symbol_raw to avoid needing a tree-sitter Node.
        match keyword.as_str() {
            "FROM" => {
                let stage_name = parse_from_stage(&instr.full_line);
                current_stage = stage_name.clone();
                directive_idx = 0;
                push_symbol_raw(
                    &mut symbols,
                    "dockerfile",
                    &stage_name,
                    SymbolKind::Section,
                    start,
                    end,
                    source,
                    file_path,
                );
            }
            _ => {
                let name = format!("{}_{}", keyword, directive_idx);
                directive_idx += 1;
                push_symbol_raw(
                    &mut symbols,
                    &current_stage,
                    &name,
                    SymbolKind::Directive,
                    start,
                    end,
                    source,
                    file_path,
                );
            }
        }
    }
    symbols
}

struct Instruction {
    keyword: String,
    full_line: String,
    start_byte: usize,
    end_byte: usize,
}

/// Collect instruction spans from Dockerfile text, handling line continuations.
fn collect_instructions(text: &str) -> Vec<Instruction> {
    let mut instructions = Vec::new();
    let mut current: Option<(String, String, usize)> = None; // (keyword, full_text, start_byte)

    let dockerfile_keywords = [
        "FROM", "RUN", "CMD", "LABEL", "MAINTAINER", "EXPOSE", "ENV", "ADD",
        "COPY", "ENTRYPOINT", "VOLUME", "USER", "WORKDIR", "ARG", "ONBUILD",
        "STOPSIGNAL", "HEALTHCHECK", "SHELL",
    ];

    for (line_start, line) in line_byte_offsets(text) {
        let trimmed = line.trim();

        // Skip empty lines and comments.
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Check if this line starts a new instruction.
        let first_word = trimmed.split_whitespace().next().unwrap_or("");
        let is_new_instruction = dockerfile_keywords
            .iter()
            .any(|kw| first_word.eq_ignore_ascii_case(kw));

        if is_new_instruction {
            // Flush previous instruction.
            if let Some((kw, full, start)) = current.take() {
                instructions.push(Instruction {
                    keyword: kw,
                    full_line: full,
                    start_byte: start,
                    end_byte: line_start,
                });
            }
            current = Some((
                first_word.to_uppercase(),
                line.to_string(),
                line_start,
            ));
        } else if let Some((_, ref mut full, _)) = current {
            // Continuation line (after `\` or multi-line RUN).
            full.push('\n');
            full.push_str(line);
        }
    }

    // Flush last instruction.
    if let Some((kw, full, start)) = current {
        instructions.push(Instruction {
            keyword: kw,
            full_line: full,
            start_byte: start,
            end_byte: text.len(),
        });
    }

    instructions
}

/// Iterate over lines with their byte offsets.
fn line_byte_offsets(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut offset = 0;
    text.lines().map(move |line| {
        let start = offset;
        offset += line.len() + 1; // +1 for '\n' (approximation)
        (start, line)
    })
}

/// Parse "FROM image:tag AS stagename" → "stagename", or fall back to image.
fn parse_from_stage(from_text: &str) -> String {
    let parts: Vec<&str> = from_text.split_whitespace().collect();
    for (i, part) in parts.iter().enumerate() {
        if part.eq_ignore_ascii_case("AS")
            && let Some(name) = parts.get(i + 1)
        {
            return name.to_string();
        }
    }
    parts.get(1).unwrap_or(&"unknown").to_string()
}

/// Like `push_symbol` but takes byte offsets instead of a tree-sitter Node.
#[allow(clippy::too_many_arguments)]
fn push_symbol_raw(
    symbols: &mut Vec<SymbolEntry>,
    scope: &str,
    name: &str,
    kind: SymbolKind,
    start_byte: usize,
    end_byte: usize,
    source: &[u8],
    file_path: &Path,
) {
    use phantom_core::id::{ContentHash, SymbolId};

    let kind_str = format!("{kind:?}").to_lowercase();
    let id = SymbolId(format!("{scope}::{name}::{kind_str}"));
    let end = end_byte.min(source.len());
    let start = start_byte.min(end);
    let content = &source[start..end];
    let content_hash = ContentHash::from_bytes(content);

    symbols.push(SymbolEntry {
        id,
        kind,
        name: name.to_string(),
        scope: scope.to_string(),
        file: file_path.to_path_buf(),
        byte_range: start..end,
        content_hash,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn parse_dockerfile(source: &str) -> Vec<SymbolEntry> {
        // Dockerfile extractor ignores the tree-sitter tree and parses raw text.
        let mut parser = tree_sitter::Parser::new();
        let extractor = DockerfileExtractor;
        parser.set_language(&extractor.language()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        extractor.extract_symbols(&tree, source.as_bytes(), Path::new("Dockerfile"))
    }

    #[test]
    fn extracts_single_stage() {
        let src = "FROM rust:1.85\nRUN cargo build\nCOPY . /app\n";
        let symbols = parse_dockerfile(src);
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section && s.name == "rust:1.85"));
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Directive && s.name.starts_with("RUN")));
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Directive && s.name.starts_with("COPY")));
    }

    #[test]
    fn extracts_multi_stage_build() {
        let src = r#"FROM rust:1.85 AS builder
    RUN cargo build --release

    FROM debian:bookworm-slim AS runtime
    COPY --from=builder /app/target/release/app /usr/local/bin/
    CMD ["/usr/local/bin/app"]
    "#;
        let symbols = parse_dockerfile(src);
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section && s.name == "builder"));
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section && s.name == "runtime"));
        // Each stage should have its own directives.
        let builder_directives: Vec<_> = symbols.iter()
            .filter(|s| s.scope == "builder" && s.kind == SymbolKind::Directive)
            .collect();
        assert!(!builder_directives.is_empty());
    }

    #[test]
    fn skips_comments() {
        let src = "# This is a comment\nFROM alpine\nRUN echo hello\n";
        let symbols = parse_dockerfile(src);
        // Should not create symbols for comments.
        assert!(!symbols.iter().any(|s| s.name.contains("comment")));
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section));
    }
}
