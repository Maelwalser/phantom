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

/// Reconstruct a merged file from base, ours, and theirs using symbol regions.
///
/// Strategy:
/// 1. Build a map of base symbol regions (byte ranges).
/// 2. Walk through base, replacing symbol regions with the appropriate version.
/// 3. Append symbols that were added by either side.
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

    for base_sym in &sorted_base {
        let key = entity_key(base_sym);
        let range = &base_sym.byte_range;

        // Copy interstitial bytes (between symbols) from base
        if range.start > cursor {
            result.extend_from_slice(&base[cursor..range.start]);
        }

        let in_ours = ours_map.get(&key);
        let in_theirs = theirs_map.get(&key);

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
                // Theirs deleted it, ours still has it (unchanged, since conflicts are already caught)
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

        cursor = range.end;
    }

    // Copy trailing bytes from base
    if cursor < base.len() {
        result.extend_from_slice(&base[cursor..]);
    }

    // Append symbols that were added by ours (not in base)
    for ours_sym in ours_symbols {
        let key = entity_key(ours_sym);
        if !base_map.contains_key(&key) {
            // Check if theirs also added the same symbol — if so, only add once
            if let Some(theirs_sym) = theirs_map.get(&key) {
                if theirs_sym.content_hash == ours_sym.content_hash {
                    // Identical — add from ours
                    ensure_newline(&mut result);
                    result.extend_from_slice(&ours[ours_sym.byte_range.clone()]);
                }
                // Different content is a conflict, already caught
            } else {
                ensure_newline(&mut result);
                result.extend_from_slice(&ours[ours_sym.byte_range.clone()]);
            }
        }
    }

    // Append symbols that were added only by theirs (not in base and not in ours)
    for theirs_sym in theirs_symbols {
        let key = entity_key(theirs_sym);
        if !base_map.contains_key(&key) && !ours_map.contains_key(&key) {
            ensure_newline(&mut result);
            result.extend_from_slice(&theirs[theirs_sym.byte_range.clone()]);
        }
    }

    result
}
