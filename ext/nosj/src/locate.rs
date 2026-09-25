//! Cold-path positions for the refusals a sink detects without offsets.
//!
//! Sinks see events, not byte positions, so a duplicate key (found by
//! hash size or key fingerprints) or a lone surrogate (delivered as
//! WTF-8) aborts the drive with no location. These walks re-read the
//! same bytes with the crate's pull `Reader`, under the same grammar
//! options, to find where. They run only after a refusal, so they
//! favor simplicity over speed (a nested container is re-indexed as
//! its own slice).

use nosj::{Buffers, Node, ParseError, ParseOptions, Reader};

const WS: [u8; 4] = *b" \t\n\r";

/// Deeper nesting than this gives up on a position instead of risking
/// the native stack (only reachable with `max_nesting: false`).
const WALK_DEPTH_MAX: usize = 4096;

/// The first object, in the order objects close (as the drive meets
/// them), that repeats a key: the offset of its `{` within `doc`, and
/// the repeated key. `None` when there is none (a fingerprint
/// collision) or the walk cannot finish.
pub(crate) fn duplicate_key(doc: &[u8], popts: ParseOptions) -> Option<(usize, String)> {
    let start = doc.iter().position(|b| !WS.contains(b))?;
    first_duplicate(&doc[start..], start, popts, 0)
}

fn first_duplicate(
    value: &[u8],
    at: usize,
    popts: ParseOptions,
    depth: usize,
) -> Option<(usize, String)> {
    if depth > WALK_DEPTH_MAX {
        return None;
    }
    let mut bufs = Buffers::new();
    // SAFETY: `value` is validated UTF-8 cut on token edges.
    let mut r = unsafe { Reader::from_utf8_unchecked_with(value, &mut bufs, popts) };
    // A container child is walked as its own slice, before its later
    // siblings and before its parent's own keys: close order.
    let child = |v: &str| -> Option<(usize, String)> {
        let offset = v.as_ptr() as usize - value.as_ptr() as usize;
        matches!(v.as_bytes().first(), Some(b'{' | b'['))
            .then(|| first_duplicate(v.as_bytes(), at + offset, popts, depth + 1))
            .flatten()
    };
    match r.next_node().ok()? {
        Node::ObjectStart => {
            let mut seen = std::collections::HashSet::new();
            let mut repeated = None;
            let mut key = r.object_first_key().ok()?.map(str::to_owned);
            while let Some(k) = key {
                if let Some(hit) = child(r.skip_value().ok()?) {
                    return Some(hit);
                }
                if !seen.insert(k.clone()) && repeated.is_none() {
                    repeated = Some(k);
                }
                key = r.object_next_key().ok()?.map(str::to_owned);
            }
            repeated.map(|k| (at, k))
        }
        Node::ArrayStart => {
            let mut more = r.array_first().ok()?;
            while more {
                if let Some(hit) = child(r.skip_value().ok()?) {
                    return Some(hit);
                }
                more = r.array_next().ok()?;
            }
            None
        }
        _ => None,
    }
}

/// The first error a full walk of `doc` meets, decoding every string
/// and key: where a lone surrogate the sink refused sits (the Reader
/// rejects them with a position).
pub(crate) fn first_walk_error(doc: &[u8], popts: ParseOptions) -> Option<ParseError> {
    let mut bufs = Buffers::new();
    // SAFETY: `doc` is validated UTF-8.
    let mut r = unsafe { Reader::from_utf8_unchecked_with(doc, &mut bufs, popts) };
    walk(&mut r, 0).err().flatten()
}

/// `Err(None)` gives up (too deep); `Err(Some(e))` is the error found.
fn walk(r: &mut Reader<'_, '_>, depth: usize) -> Result<(), Option<ParseError>> {
    if depth > WALK_DEPTH_MAX {
        return Err(None);
    }
    match r.next_node()? {
        Node::ObjectStart => {
            let mut more = r.object_first_key()?.is_some();
            while more {
                walk(r, depth + 1)?;
                more = r.object_next_key()?.is_some();
            }
        }
        Node::ArrayStart => {
            let mut more = r.array_first()?;
            while more {
                walk(r, depth + 1)?;
                more = r.array_next()?;
            }
        }
        _ => {}
    }
    Ok(())
}
