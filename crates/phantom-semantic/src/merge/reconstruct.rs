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

/// Find the nearest preceding sibling of `new_sym` that also exists in `base_map`,
/// returning its entity key.  Symbols are ordered by byte offset in `source_symbols`.
fn find_preceding_base_sibling<'a>(
    new_sym: &SymbolEntry,
    source_symbols: &[SymbolEntry],
    base_map: &HashMap<EntityKey, &'a SymbolEntry>,
) -> Option<EntityKey> {
    // Symbols in the same source file, sorted by byte offset, that appear
    // before `new_sym` and also exist in base.
    let mut best: Option<(&SymbolEntry, EntityKey)> = None;
    for sym in source_symbols {
        if sym.byte_range.start >= new_sym.byte_range.start {
            continue;
        }
        let key = entity_key(sym);
        if base_map.contains_key(&key) {
            match &best {
                Some((prev, _)) if sym.byte_range.start > prev.byte_range.start => {
                    best = Some((sym, key));
                }
                None => {
                    best = Some((sym, key));
                }
                _ => {}
            }
        }
    }
    best.map(|(_, key)| key)
}

/// Find the nearest following sibling of `new_sym` that also exists in `base_map`,
/// returning its entity key.
fn find_following_base_sibling<'a>(
    new_sym: &SymbolEntry,
    source_symbols: &[SymbolEntry],
    base_map: &HashMap<EntityKey, &'a SymbolEntry>,
) -> Option<EntityKey> {
    let mut best: Option<(&SymbolEntry, EntityKey)> = None;
    for sym in source_symbols {
        if sym.byte_range.start <= new_sym.byte_range.start {
            continue;
        }
        let key = entity_key(sym);
        if base_map.contains_key(&key) {
            match &best {
                Some((next, _)) if sym.byte_range.start < next.byte_range.start => {
                    best = Some((sym, key));
                }
                None => {
                    best = Some((sym, key));
                }
                _ => {}
            }
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
            find_preceding_base_sibling(ours_sym, ours_symbols, &base_map)
        {
            if let Some(&pos) = base_output_end.get(&prev_key) {
                insertions.push(Insertion {
                    after_output_pos: pos,
                    prepend: false,
                    content,
                    original_index: insertions.len(),
                });
                continue;
            }
        }

        if let Some(next_key) =
            find_following_base_sibling(ours_sym, ours_symbols, &base_map)
        {
            if let Some(&pos) = base_output_start.get(&next_key) {
                insertions.push(Insertion {
                    after_output_pos: pos,
                    prepend: true,
                    content,
                    original_index: insertions.len(),
                });
                continue;
            }
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
            find_preceding_base_sibling(theirs_sym, theirs_symbols, &base_map)
        {
            if let Some(&pos) = base_output_end.get(&prev_key) {
                insertions.push(Insertion {
                    after_output_pos: pos,
                    prepend: false,
                    content,
                    original_index: insertions.len(),
                });
                continue;
            }
        }

        if let Some(next_key) =
            find_following_base_sibling(theirs_sym, theirs_symbols, &base_map)
        {
            if let Some(&pos) = base_output_start.get(&next_key) {
                insertions.push(Insertion {
                    after_output_pos: pos,
                    prepend: true,
                    content,
                    original_index: insertions.len(),
                });
                continue;
            }
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
