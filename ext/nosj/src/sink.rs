//! The nosj sinks: `RubyValueSink` builds Ruby VALUEs directly during
//! the parse (with interned-key caches and gem-compatible option
//! handling); `NullSink` powers `NOSJ.valid?` by discarding every
//! event. Raw VALUE construction helpers live here too.

use ahash::AHashMap;

use crate::state::VStackShadow;

/// Sanity ceiling for pending values (memory bomb guard), not a design limit.
const SINK_STACK_MAX: usize = 1 << 26;

/// The JSON gem's default nesting limit; matching it is part of drop-in
/// compatibility (gem raises NestingError past 100 levels).
pub(crate) const MAX_NESTING: usize = 100;

const KEY_CACHE_CAP: usize = 2048;

/// Why a sink stopped the drive; mapped onto the gem's exceptions in
/// [`crate::parse::finish_drive`].
pub(crate) enum SinkAbort {
    Overflow,
    BadBigint,
    TooDeep,
    /// The reformat pipe met a WTF-8 (lone-surrogate) object KEY,
    /// which the Writer has no pre-serialized escape hatch for; gem
    /// parity is the GeneratorError `generate` raises on the
    /// equivalent broken-coderange string. (String VALUES re-escape
    /// as \uXXXX instead.)
    BrokenUtf8Output,
}

/// `RB_INT2FIX` ported from Ruby's public inline headers: fixnums are
/// `(i << 1) + 1` for `|i| <= LONG_MAX / 2`, and the range is defined by
/// the C `long`, which is 32-bit on Windows (LLP64): fixnums there hold
/// only 31 bits, and tagging anything wider crashes Ruby with
/// "Unnormalized Fixnum value". C extensions get the check inlined by the
/// header; going through the extern `rb_ll2inum` costs an FFI call per
/// integer. Non-fixable values still take the call.
// The widening is an identity on LP64 hosts (clippy flags it there) but
// required on Windows, where c_long is 32-bit.
#[allow(clippy::unnecessary_cast)]
#[inline(always)]
fn int_to_raw(i: i64) -> rb_sys::VALUE {
    const FIXABLE_MIN: i64 = (std::os::raw::c_long::MIN / 2) as i64;
    const FIXABLE_MAX: i64 = (std::os::raw::c_long::MAX / 2) as i64;
    if (FIXABLE_MIN..=FIXABLE_MAX).contains(&i) {
        ((i as u64) << 1).wrapping_add(1) as rb_sys::VALUE
    } else {
        unsafe { rb_sys::rb_ll2inum(i) }
    }
}

#[inline(always)]
fn str_to_raw(s: &str) -> rb_sys::VALUE {
    unsafe {
        rb_sys::rb_utf8_str_new(
            s.as_ptr() as *const std::os::raw::c_char,
            s.len() as std::os::raw::c_long,
        )
    }
}

#[inline(always)]
fn interned_str_raw(s: &str) -> rb_sys::VALUE {
    unsafe {
        rb_sys::rb_enc_interned_str(
            s.as_ptr() as *const std::os::raw::c_char,
            s.len() as std::os::raw::c_long,
            rb_sys::rb_utf8_encoding(),
        )
    }
}

/// Interned-key cache with epoch eviction: when the cap is reached the whole
/// cache is cleared (hot keys repopulate immediately). Without this, one
/// document with many unique keys (citm's numeric id maps) permanently fills
/// the cache and every later document pays full interning per key, measured
/// as a 1.37x regression on twitter parsed after citm in the same process.
#[inline(always)]
fn intern_key_cached(
    key: &str,
    map: &mut AHashMap<Box<str>, rb_sys::VALUE>,
    shadow: &mut VStackShadow,
) -> rb_sys::VALUE {
    if key.is_empty() || key.len() > 64 {
        return interned_str_raw(key);
    }

    if let Some(&v) = map.get(key) {
        return v;
    }

    let raw = interned_str_raw(key);
    if map.len() >= KEY_CACHE_CAP {
        map.clear();
        shadow.values.clear();
    }
    map.insert(Box::from(key), raw);
    shadow.values.push(raw);
    raw
}

/// Symbol-mode key cache. Static symbols from `rb_intern3` are permanent, so
/// no GC shadow is needed; epoch-cleared at capacity like the string cache.
#[inline(always)]
fn intern_symbol_cached(key: &str, map: &mut AHashMap<Box<str>, rb_sys::VALUE>) -> rb_sys::VALUE {
    #[inline(always)]
    fn intern(key: &str) -> rb_sys::VALUE {
        unsafe {
            rb_sys::rb_id2sym(rb_sys::rb_intern3(
                key.as_ptr() as *const std::os::raw::c_char,
                key.len() as std::os::raw::c_long,
                rb_sys::rb_utf8_encoding(),
            ))
        }
    }

    if key.is_empty() || key.len() > 64 {
        return intern(key);
    }
    if let Some(&v) = map.get(key) {
        return v;
    }
    let raw = intern(key);
    if map.len() >= KEY_CACHE_CAP {
        map.clear();
    }
    map.insert(Box::from(key), raw);
    raw
}

/// nosj::Sink building Ruby VALUEs on the heap value stack. Pending
/// VALUEs are kept alive by the pinned TypedData wrapper's precise dmark.
pub(crate) struct RubyValueSink<'a> {
    pub(crate) stack: &'a mut Vec<rb_sys::VALUE>,
    pub(crate) keys: &'a mut AHashMap<Box<str>, rb_sys::VALUE>,
    pub(crate) key_shadow: &'a mut VStackShadow,
    pub(crate) depth: usize,
    /// JSON.parse-compatible options; defaults keep the fast paths.
    pub(crate) symbolize: bool,
    pub(crate) freeze: bool,
    pub(crate) max_nesting: usize,
}

// Tried and rejected (2026-07-10): a jiter-style cache of repeated VALUE
// strings (rb_str_dup of a canonical copy, gated to heap lengths 40..=200
// where the dup shares its buffer copy-on-write). Measured twitter parse
// 0.95x vs 0.90x without: Ruby must dup for mutability where Python only
// INCREFs, and the hash+insert overhead exceeds the CoW savings.

impl RubyValueSink<'_> {
    #[inline(always)]
    fn push_raw(&mut self, raw: rb_sys::VALUE) -> Result<(), SinkAbort> {
        if self.stack.len() >= SINK_STACK_MAX {
            return Err(SinkAbort::Overflow);
        }
        self.stack.push(raw);
        Ok(())
    }

    #[inline(always)]
    fn enter_container(&mut self) -> Result<(), SinkAbort> {
        self.depth += 1;
        if self.depth > self.max_nesting {
            return Err(SinkAbort::TooDeep);
        }
        Ok(())
    }
}

impl nosj::Sink for RubyValueSink<'_> {
    type Error = SinkAbort;

    #[inline(always)]
    fn null(&mut self) -> Result<(), SinkAbort> {
        self.push_raw(rb_sys::special_consts::Qnil as rb_sys::VALUE)
    }

    #[inline(always)]
    fn boolean(&mut self, value: bool) -> Result<(), SinkAbort> {
        self.push_raw(if value {
            rb_sys::special_consts::Qtrue as rb_sys::VALUE
        } else {
            rb_sys::special_consts::Qfalse as rb_sys::VALUE
        })
    }

    #[inline(always)]
    fn int(&mut self, value: i64) -> Result<(), SinkAbort> {
        self.push_raw(int_to_raw(value))
    }

    #[inline(always)]
    fn float(&mut self, value: f64) -> Result<(), SinkAbort> {
        self.push_raw(unsafe { rb_sys::rb_float_new(value) })
    }

    #[inline(always)]
    fn big_int(&mut self, digits: &str) -> Result<(), SinkAbort> {
        let c = std::ffi::CString::new(digits).map_err(|_| SinkAbort::BadBigint)?;
        self.push_raw(unsafe { rb_sys::rb_cstr2inum(c.as_ptr(), 10) })
    }

    #[inline(always)]
    fn str(&mut self, value: &str) -> Result<(), SinkAbort> {
        let raw = if self.freeze {
            // Gem parity: freeze mode dedupes strings via the fstring table.
            interned_str_raw(value)
        } else {
            str_to_raw(value)
        };
        self.push_raw(raw)
    }

    #[inline(always)]
    fn key(&mut self, key: &str) -> Result<(), SinkAbort> {
        let raw = if self.symbolize {
            intern_symbol_cached(key, self.keys)
        } else {
            intern_key_cached(key, self.keys, self.key_shadow)
        };
        self.push_raw(raw)
    }

    /// Lone-low-surrogate content: gem parity is a UTF-8-encoded Ruby string
    /// carrying the raw WTF-8 bytes (broken coderange, like the gem's).
    #[inline(always)]
    fn str_bytes(&mut self, value: &[u8]) -> Result<(), SinkAbort> {
        let raw = unsafe {
            let s = rb_sys::rb_utf8_str_new(
                value.as_ptr() as *const std::os::raw::c_char,
                value.len() as std::os::raw::c_long,
            );
            if self.freeze {
                rb_sys::rb_str_freeze(s)
            } else {
                s
            }
        };
        self.push_raw(raw)
    }

    #[inline(always)]
    fn key_bytes(&mut self, key: &[u8]) -> Result<(), SinkAbort> {
        // Interning is skipped: these keys are pathological, not hot.
        let raw = unsafe {
            rb_sys::rb_utf8_str_new(
                key.as_ptr() as *const std::os::raw::c_char,
                key.len() as std::os::raw::c_long,
            )
        };
        let frozen = unsafe { rb_sys::rb_str_freeze(raw) };
        self.push_raw(frozen)
    }

    #[inline(always)]
    fn begin_array(&mut self) -> Result<(), SinkAbort> {
        self.enter_container()
    }

    #[inline(always)]
    fn begin_object(&mut self) -> Result<(), SinkAbort> {
        self.enter_container()
    }

    #[inline(always)]
    fn mark(&self) -> usize {
        self.stack.len()
    }

    // Note: eager arrays (begin_array allocating + array_checkpoint spilling)
    // were measured SLOWER here; rb_ary_new + rb_ary_cat growth per small
    // array loses to one exact-size rb_ary_new_from_values, and the hoped-for
    // GC-marking savings didn't materialize (sweep/free dominates, not
    // pending-stack marking). The default no-op hooks stay available for
    // sinks where the trade differs.
    #[inline(always)]
    fn end_array(&mut self, mark: usize, _len: usize) -> Result<(), SinkAbort> {
        self.depth -= 1;
        let n = self.stack.len() - mark;
        let raw = unsafe {
            let a = rb_sys::rb_ary_new_from_values(
                n as std::os::raw::c_long,
                self.stack.as_ptr().add(mark),
            );
            if self.freeze {
                rb_sys::rb_obj_freeze(a);
            }
            a
        };
        self.stack.truncate(mark);
        self.push_raw(raw)
    }

    #[inline(always)]
    fn end_object(&mut self, mark: usize, pairs: usize) -> Result<(), SinkAbort> {
        self.depth -= 1;
        let n = self.stack.len() - mark;
        let hash_raw = unsafe { rb_sys::rb_hash_new_capa(pairs as std::os::raw::c_long) };
        unsafe {
            rb_sys::rb_hash_bulk_insert(
                n as std::os::raw::c_long,
                self.stack.as_ptr().add(mark),
                hash_raw,
            );
            if self.freeze {
                rb_sys::rb_obj_freeze(hash_raw);
            }
        }
        self.stack.truncate(mark);
        self.push_raw(hash_raw)
    }
}

/// Validation-only sink: every event is a no-op except nesting-depth
/// tracking, so `NOSJ.valid?` runs the full parser (tokenizers,
/// string decode, number validation) without allocating a single VALUE.
pub(crate) struct NullSink {
    pub(crate) depth: usize,
    pub(crate) max_nesting: usize,
}

impl nosj::Sink for NullSink {
    type Error = SinkAbort;

    fn null(&mut self) -> Result<(), SinkAbort> {
        Ok(())
    }
    fn boolean(&mut self, _: bool) -> Result<(), SinkAbort> {
        Ok(())
    }
    fn int(&mut self, _: i64) -> Result<(), SinkAbort> {
        Ok(())
    }
    fn float(&mut self, _: f64) -> Result<(), SinkAbort> {
        Ok(())
    }
    fn big_int(&mut self, _: &str) -> Result<(), SinkAbort> {
        Ok(())
    }
    fn str(&mut self, _: &str) -> Result<(), SinkAbort> {
        Ok(())
    }
    fn key(&mut self, _: &str) -> Result<(), SinkAbort> {
        Ok(())
    }
    fn str_bytes(&mut self, _: &[u8]) -> Result<(), SinkAbort> {
        Ok(())
    }
    fn mark(&self) -> usize {
        0
    }
    fn begin_array(&mut self) -> Result<(), SinkAbort> {
        self.depth += 1;
        if self.depth > self.max_nesting {
            return Err(SinkAbort::TooDeep);
        }
        Ok(())
    }
    fn begin_object(&mut self) -> Result<(), SinkAbort> {
        self.depth += 1;
        if self.depth > self.max_nesting {
            return Err(SinkAbort::TooDeep);
        }
        Ok(())
    }
    fn end_array(&mut self, _: usize, _: usize) -> Result<(), SinkAbort> {
        self.depth -= 1;
        Ok(())
    }
    fn end_object(&mut self, _: usize, _: usize) -> Result<(), SinkAbort> {
        self.depth -= 1;
        Ok(())
    }
}
