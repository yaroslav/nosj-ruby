//! Cold-path positions for the refusals a sink detects without offsets.
//!
//! Sinks see events, not byte positions, so a duplicate key (found by
//! hash size or key fingerprints) or a lone surrogate (delivered as
//! WTF-8) aborts the drive with no location. These walks re-read the
//! same bytes with the crate's pull `Reader`, under the same grammar
//! options, to find where. Each is linear and iterative: one `Reader`
//! over the whole document, an explicit stack instead of recursion, so
//! no nesting depth is out of reach.

use std::collections::HashSet;

use nosj::{Buffers, Node, ParseError, ParseOptions, Reader};

use crate::parse::span_of;

/// What the exact check found behind a sink's duplicate-key refusal.
pub(crate) enum Repeat {
    /// The first object, in the order objects close (as the drive meets
    /// them), that repeats a key: the offset of its `{` and the key.
    Found { at: usize, key: String },
    /// No object repeats a key: the refusal was a fingerprint collision.
    Absent,
    /// The walk stopped on a Reader error before deciding. Callers
    /// treat it as a refusal.
    Undecided,
}

pub(crate) fn duplicate_key(doc: &[u8], popts: ParseOptions) -> Repeat {
    match scan(doc, popts, true) {
        Ok(None) => Repeat::Absent,
        Ok(Some((path, key))) => match container_offset(doc, &path, popts) {
            Ok(at) => Repeat::Found { at, key },
            Err(_) => Repeat::Undecided,
        },
        Err(_) => Repeat::Undecided,
    }
}

/// The first error a full walk of `doc` meets, decoding every string
/// and key: where a lone surrogate the sink refused sits (the Reader
/// rejects them with a position).
pub(crate) fn first_walk_error(doc: &[u8], popts: ParseOptions) -> Option<ParseError> {
    scan(doc, popts, false).err()
}

/// One open container of [`scan`]'s walk.
struct Frame {
    object: bool,
    /// The member or element being walked.
    index: usize,
    seen: HashSet<String>,
    repeated: Option<String>,
}

impl Frame {
    fn open(object: bool) -> Self {
        Frame {
            object,
            index: 0,
            seen: HashSet::new(),
            repeated: None,
        }
    }

    fn note_key(&mut self, key: &str) {
        if self.repeated.is_none() && !self.seen.insert(key.to_owned()) {
            self.repeated = Some(key.to_owned());
        }
    }
}

/// Walk all of `doc`, depth first. With `find_repeats`, stop at the
/// first object to close with a repeated key: the member/element
/// indices from the root down to it, and the key.
fn scan(
    doc: &[u8],
    popts: ParseOptions,
    find_repeats: bool,
) -> Result<Option<(Vec<usize>, String)>, ParseError> {
    let mut bufs = Buffers::new();
    // SAFETY: `doc` is validated UTF-8.
    let mut r = unsafe { Reader::from_utf8_unchecked_with(doc, &mut bufs, popts) };
    let mut stack: Vec<Frame> = Vec::new();
    loop {
        // The cursor is at a value: open it, or step past it.
        let opened = match r.next_node()? {
            Node::ObjectStart => match r.object_first_key()? {
                Some(key) => {
                    let mut frame = Frame::open(true);
                    if find_repeats {
                        frame.note_key(key);
                    }
                    Some(frame)
                }
                None => None,
            },
            Node::ArrayStart => r.array_first()?.then(|| Frame::open(false)),
            _ => None,
        };
        if let Some(frame) = opened {
            stack.push(frame);
            continue;
        }
        // The value is complete: advance its container, closing every
        // container it completes on the way up.
        loop {
            let Some(top) = stack.last_mut() else {
                return Ok(None);
            };
            let more = if top.object {
                match r.object_next_key()? {
                    Some(key) => {
                        if find_repeats {
                            top.note_key(key);
                        }
                        true
                    }
                    None => false,
                }
            } else {
                r.array_next()?
            };
            if more {
                top.index += 1;
                break;
            }
            if let Some(key) = stack.pop().and_then(|closed| closed.repeated) {
                return Ok(Some((stack.iter().map(|f| f.index).collect(), key)));
            }
        }
    }
}

/// The offset of the container at `path`, member/element indices from
/// the root: walk down skipping earlier siblings, then skip the
/// container itself, whose slice starts at its bracket.
fn container_offset(doc: &[u8], path: &[usize], popts: ParseOptions) -> Result<usize, ParseError> {
    let mut bufs = Buffers::new();
    // SAFETY: `doc` is validated UTF-8.
    let mut r = unsafe { Reader::from_utf8_unchecked_with(doc, &mut bufs, popts) };
    // Every index on the path was walked by `scan`, so each step lands
    // on an existing member.
    for &index in path {
        let object = matches!(r.next_node()?, Node::ObjectStart);
        if object {
            r.object_first_key()?;
        } else {
            r.array_first()?;
        }
        for _ in 0..index {
            r.skip_value()?;
            if object {
                r.object_next_key()?;
            } else {
                r.array_next()?;
            }
        }
    }
    let container = r.skip_value()?;
    Ok(span_of(doc, container.as_bytes()).0)
}
