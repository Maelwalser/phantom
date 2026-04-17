//! Merged file reconstruction from symbol regions.

use std::collections::HashMap;

use phantom_core::symbol::SymbolEntry;

use crate::diff::{EntityKey, entity_key};

/// Ensure `buf` ends with exactly one newline before appending a new symbol.
fn ensure_newline(buf: &mut Vec<u8>) {
    if buf.last() != Some(&b'\n') {
        buf.push(b'\n');
    }
}

/// A pending insertion of a new symbol into the output buffer.
struct Insertion {
    /// Byte position in the output buffer after which to insert.
    /// `0` means insert at the very beginning (before all content).
    after_output_pos: usize,
    /// Whether this insertion targets the beginning of the file (before any
    /// base content).  Used to distinguish "insert at pos 0 = prepend" from
    /// "insert after the byte at pos 0".
    prepend: bool,
    /// The raw bytes to insert (from the source file).
    content: Vec<u8>,
    /// Insertion order index used to break ties during sorting so that
    /// multiple insertions at the same position preserve their source order.
    original_index: usize,
}

/// Search direction relative to `new_sym` when looking for an anchor symbol in base.
#[derive(Clone, Copy)]
enum Direction {
    Preceding,
    Following,
}

/// Find the nearest sibling of `new_sym` in the given direction that also exists
/// in `base_map`, returning its entity key. Symbols are scanned in `source_symbols`
/// (an unsorted slice); the closest one by byte offset wins.
fn find_base_sibling(
    new_sym: &SymbolEntry,
    source_symbols: &[SymbolEntry],
    base_map: &HashMap<EntityKey, &SymbolEntry>,
    dir: Direction,
) -> Option<EntityKey> {
    let anchor = new_sym.byte_range.start;
    let mut best: Option<(&SymbolEntry, EntityKey)> = None;
    for sym in source_symbols {
        let pos = sym.byte_range.start;
        let on_right_side = match dir {
            Direction::Preceding => pos < anchor,
            Direction::Following => pos > anchor,
        };
        if !on_right_side {
            continue;
        }
        let key = entity_key(sym);
        if !base_map.contains_key(&key) {
            continue;
        }
        let is_closer = match (&best, dir) {
            (None, _) => true,
            (Some((prev, _)), Direction::Preceding) => pos > prev.byte_range.start,
            (Some((next, _)), Direction::Following) => pos < next.byte_range.start,
        };
        if is_closer {
            best = Some((sym, key));
        }
    }
    best.map(|(_, key)| key)
}

/// Reconstruct a merged file from base, ours, and theirs using symbol regions.
///
/// Strategy:
/// 1. Build a map of base symbol regions (byte ranges).
/// 2. Walk through base, replacing symbol regions with the appropriate version.
/// 3. Insert symbols added by either side at their approximate original position
///    relative to neighboring symbols that exist in base.
pub(super) fn reconstruct_merged_file(
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
    base_symbols: &[SymbolEntry],
    ours_symbols: &[SymbolEntry],
    theirs_symbols: &[SymbolEntry],
) -> Vec<u8> {
    let base_map: HashMap<EntityKey, &SymbolEntry> =
        base_symbols.iter().map(|e| (entity_key(e), e)).collect();
    let ours_map: HashMap<EntityKey, &SymbolEntry> =
        ours_symbols.iter().map(|e| (entity_key(e), e)).collect();
    let theirs_map: HashMap<EntityKey, &SymbolEntry> =
        theirs_symbols.iter().map(|e| (entity_key(e), e)).collect();

    // Sort base symbols by byte position
    let mut sorted_base: Vec<&SymbolEntry> = base_symbols.iter().collect();
    sorted_base.sort_by_key(|s| s.byte_range.start);

    let mut result = Vec::new();
    let mut cursor = 0;

    // Track output positions for base symbols so we can place new symbols nearby.
    // Maps entity key → byte offset in `result` immediately after the symbol was emitted.
    let mut base_output_end: HashMap<EntityKey, usize> = HashMap::new();
    // Maps entity key → byte offset in `result` immediately before the symbol was emitted.
    let mut base_output_start: HashMap<EntityKey, usize> = HashMap::new();

    for base_sym in &sorted_base {
        let key = entity_key(base_sym);
        let range = &base_sym.byte_range;

        // Copy interstitial bytes (between symbols) from base
        if range.start > cursor {
            result.extend_from_slice(&base[cursor..range.start]);
        }

        let in_ours = ours_map.get(&key);
        let in_theirs = theirs_map.get(&key);

        let start_pos = result.len();

        match (in_ours, in_theirs) {
            (Some(o), Some(t)) => {
                let ours_changed = o.content_hash != base_sym.content_hash;
                let theirs_changed = t.content_hash != base_sym.content_hash;
                if ours_changed && !theirs_changed {
                    result.extend_from_slice(&ours[o.byte_range.clone()]);
                } else if !ours_changed && theirs_changed {
                    result.extend_from_slice(&theirs[t.byte_range.clone()]);
                } else {
                    // Both changed to same thing, or neither changed — use ours
                    result.extend_from_slice(&ours[o.byte_range.clone()]);
                }
            }
            (Some(o), None) => {
                if o.content_hash == base_sym.content_hash {
                    // Ours unchanged, theirs deleted → honor deletion (skip)
                } else {
                    // Should not reach here — conflict was caught
                    result.extend_from_slice(&ours[o.byte_range.clone()]);
                }
            }
            (None, Some(t)) => {
                if t.content_hash == base_sym.content_hash {
                    // Theirs unchanged, ours deleted → honor deletion (skip)
                } else {
                    result.extend_from_slice(&theirs[t.byte_range.clone()]);
                }
            }
            (None, None) => {
                // Both deleted — skip
            }
        }

        base_output_start.insert(key.clone(), start_pos);
        base_output_end.insert(key, result.len());

        cursor = range.end;
    }

    // Copy trailing bytes from base
    if cursor < base.len() {
        result.extend_from_slice(&base[cursor..]);
    }

    // Collect new symbols from ours and theirs with position hints.
    let mut insertions: Vec<Insertion> = Vec::new();

    // New symbols added by ours (not in base)
    for ours_sym in ours_symbols {
        let key = entity_key(ours_sym);
        if base_map.contains_key(&key) {
            continue;
        }

        // Decide whether to include this symbol
        let should_include = if let Some(theirs_sym) = theirs_map.get(&key) {
            // Both sides added — include only if identical (dedup)
            theirs_sym.content_hash == ours_sym.content_hash
        } else {
            true
        };

        if !should_include {
            continue;
        }

        let mut content = Vec::new();
        ensure_newline(&mut content);
        content.extend_from_slice(&ours[ours_sym.byte_range.clone()]);

        // Find position hint via neighboring base symbols
        if let Some(prev_key) =
            find_base_sibling(ours_sym, ours_symbols, &base_map, Direction::Preceding)
            && let Some(&pos) = base_output_end.get(&prev_key)
        {
            insertions.push(Insertion {
                after_output_pos: pos,
                prepend: false,
                content,
                original_index: insertions.len(),
            });
            continue;
        }

        if let Some(next_key) =
            find_base_sibling(ours_sym, ours_symbols, &base_map, Direction::Following)
            && let Some(&pos) = base_output_start.get(&next_key)
        {
            insertions.push(Insertion {
                after_output_pos: pos,
                prepend: true,
                content,
                original_index: insertions.len(),
            });
            continue;
        }

        // Fallback: append to EOF
        insertions.push(Insertion {
            after_output_pos: result.len(),
            prepend: false,
            content,
            original_index: insertions.len(),
        });
    }

    // New symbols added only by theirs (not in base and not in ours)
    for theirs_sym in theirs_symbols {
        let key = entity_key(theirs_sym);
        if base_map.contains_key(&key) || ours_map.contains_key(&key) {
            continue;
        }

        let mut content = Vec::new();
        ensure_newline(&mut content);
        content.extend_from_slice(&theirs[theirs_sym.byte_range.clone()]);

        if let Some(prev_key) =
            find_base_sibling(theirs_sym, theirs_symbols, &base_map, Direction::Preceding)
            && let Some(&pos) = base_output_end.get(&prev_key)
        {
            insertions.push(Insertion {
                after_output_pos: pos,
                prepend: false,
                content,
                original_index: insertions.len(),
            });
            continue;
        }

        if let Some(next_key) =
            find_base_sibling(theirs_sym, theirs_symbols, &base_map, Direction::Following)
            && let Some(&pos) = base_output_start.get(&next_key)
        {
            insertions.push(Insertion {
                after_output_pos: pos,
                prepend: true,
                content,
                original_index: insertions.len(),
            });
            continue;
        }

        // Fallback: append to EOF
        insertions.push(Insertion {
            after_output_pos: result.len(),
            prepend: false,
            content,
            original_index: insertions.len(),
        });
    }

    // Apply insertions in reverse position order so earlier offsets stay valid.
    // Tie-breaking ensures correct output order when multiple insertions
    // target the same position: since each splice at pos X pushes prior
    // content right, items spliced later end up earlier in the output.
    insertions.sort_by(|a, b| {
        b.after_output_pos
            .cmp(&a.after_output_pos)
            // At same position: prepend (anchored to next sibling start)
            // spliced first so it gets pushed right by the non-prepend
            // splice, yielding [non-prepend][prepend] in output order.
            .then(b.prepend.cmp(&a.prepend))
            // Within same position and prepend: higher original_index
            // spliced first → pushed right by later splices → source order
            // is preserved in the output.
            .then(b.original_index.cmp(&a.original_index))
    });

    for ins in insertions {
        result.splice(ins.after_output_pos..ins.after_output_pos, ins.content);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;
    use std::path::Path;

    fn reconstruct(base: &str, ours: &str, theirs: &str) -> String {
        let parser = Parser::new();
        let path = Path::new("test.rs");
        let base_syms = parser.parse_file(path, base.as_bytes()).unwrap();
        let ours_syms = parser.parse_file(path, ours.as_bytes()).unwrap();
        let theirs_syms = parser.parse_file(path, theirs.as_bytes()).unwrap();
        let merged = reconstruct_merged_file(
            base.as_bytes(),
            ours.as_bytes(),
            theirs.as_bytes(),
            &base_syms,
            &ours_syms,
            &theirs_syms,
        );
        String::from_utf8(merged).unwrap()
    }

    #[test]
    fn appends_new_symbol_to_eof_when_no_anchor_exists() {
        // Empty base means no sibling anchor — should append at end.
        let base = "";
        let ours = "fn new_fn() {}\n";
        let theirs = "";
        let merged = reconstruct(base, ours, theirs);
        assert!(merged.contains("fn new_fn"));
    }

    #[test]
    fn prepends_via_following_sibling_when_no_preceding_anchor() {
        // New symbol in `ours` has no preceding base sibling, but the following
        // base sibling (`anchor`) is present → insertion is anchored to its start.
        let base = "fn anchor() {}\n";
        let ours = "fn before() {}\nfn anchor() {}\n";
        let theirs = "fn anchor() {}\n";
        let merged = reconstruct(base, ours, theirs);
        let before_pos = merged.find("fn before").expect("fn before present");
        let anchor_pos = merged.find("fn anchor").expect("fn anchor present");
        assert!(
            before_pos < anchor_pos,
            "new symbol must appear before the anchor: {merged:?}"
        );
    }

    #[test]
    fn multiple_insertions_after_same_anchor_preserve_source_order() {
        // Both sides add new symbols after `a`: ours adds `b`, theirs adds `c`.
        // Both land at the same output position (right after `a`); the sort
        // invariant must preserve the order they were collected (ours first,
        // then theirs) so the output is a → b → c.
        let base = "fn a() {}\n";
        let ours = "fn a() {}\nfn b() {}\n";
        let theirs = "fn a() {}\nfn c() {}\n";
        let merged = reconstruct(base, ours, theirs);
        let a = merged.find("fn a").unwrap();
        let b = merged.find("fn b").unwrap();
        let c = merged.find("fn c").unwrap();
        assert!(a < b && b < c, "expected a < b < c, got {merged:?}");
    }
}
