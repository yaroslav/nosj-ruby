//! The nosj sinks: `RubyValueSink` builds Ruby VALUEs directly during
//! the parse (with interned-key caches and gem-compatible option
//! handling); `NullSink` powers `NOSJ.valid?` by discarding every
//! event; `DupKeys` gives hash-less sinks duplicate-key detection. Raw
//! VALUE construction helpers live here too.

use ahash::AHashMap;

use crate::state::VStackShadow;

/// Sanity ceiling for pending values (memory bomb guard), not a design limit.
const SINK_STACK_MAX: usize = 1 << 26;

/// The JSON gem's default nesting limit; matching it is part of drop-in
/// compatibility (gem raises NestingError past 100 levels).
pub(crate) const MAX_NESTING: usize = 100;

const KEY_CACHE_CAP: usize = 2048;

/// Why a sink stopped the drive; mapped onto the gem's exceptions in
/// [`crate::parse::finish_drive`]. Sinks see no offsets, so the two
/// document refusals get their position from a cold-path re-walk
/// (`crate::locate`).
pub(crate) enum SinkAbort {
    Overflow,
    BadBigint,
    TooDeep,
    /// An object repeats a key and `allow_duplicate_key` is off (json
    /// 3 semantics). From `DupKeys` it may be a fingerprint collision,
    /// which the exact cold-path check rules out.
    DuplicateKey,
    /// A string or key decodes to a lone UTF-16 surrogate (json 3
    /// rejects trailing ones too, not only leading ones).
    LoneSurrogate,
    /// The reformat pipe met a non-finite float without `allow_nan`.
    /// Parsing accepts huge-exponent literals like 1e999 as Infinity
    /// even in strict mode (gem parity), but generation refuses them;
    /// carries the JSON spelling for the gem-exact error message.
    NonFiniteFloat(&'static str),
}

/// Duplicate-key detection for sinks that build no Hash (validation and
/// the reformat pipe): each key is remembered as a 64-bit fingerprint,
/// `mark()` at a container's start is the fingerprint count, and an
/// object's close checks only its own keys. Integers, because comparing
/// them is what keeps this cheap: comparing the key bytes themselves
/// measured up to 93% slower (twitter's 40-key objects). A fingerprint
/// collision reads as a duplicate, so callers confirm a hit exactly on
/// the cold path; a collision can only cost time.
///
/// Measured split (twitter `valid?`): hashing and pushing each key is
/// ~0-2%; the close-time check is the cost, hence [`SeenTable`].
pub(crate) struct DupKeys<'a> {
    fingerprints: &'a mut Vec<u64>,
    table: &'a mut SeenTable,
    enabled: bool,
}

/// Fixed seeds: fingerprints only need to spread keys, and a collision
/// merely sends the document to the exact cold-path check. (A leaner
/// hand-rolled hash measured no faster: hashing is not the cost.)
const FINGERPRINT: ahash::RandomState = ahash::RandomState::with_seeds(
    0x243f_6a88_85a3_08d3,
    0x1319_8a2e_0370_7344,
    0xa409_3822_299f_31d0,
    0x082e_fa98_ec4e_6c89,
);

/// Objects up to this many keys compare pairwise (a few compares beat
/// any table traffic).
const PAIRWISE_MAX: usize = 8;
/// Table slots; a power of two, so a fingerprint's low bits index it.
const TABLE_SLOTS: usize = 256;
/// Objects up to this many keys use the table, which then stays at
/// least half empty; larger ones sort.
const TABLE_MAX_KEYS: usize = TABLE_SLOTS / 2;

/// Open-addressing set for one object's close: fingerprints are already
/// uniformly mixed, so their low bits index it directly, and one probe
/// per key usually decides. Each close claims a fresh epoch, and a slot
/// is empty unless it carries the current one, so nothing is cleared
/// between objects.
pub(crate) struct SeenTable {
    slots: Box<[(u64, u32)]>,
    epoch: u32,
}

impl Default for SeenTable {
    fn default() -> Self {
        SeenTable {
            slots: vec![(0, 0); TABLE_SLOTS].into_boxed_slice(),
            epoch: 0,
        }
    }
}

impl SeenTable {
    /// Whether `keys` repeats a fingerprint. Callers keep
    /// `keys.len() <= TABLE_MAX_KEYS`, so a free slot always exists.
    #[inline(always)]
    fn any_repeat(&mut self, keys: &[u64]) -> bool {
        const MASK: usize = TABLE_SLOTS - 1;
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            // Epoch 0 marks an empty slot; after a full wrap, really clear.
            self.slots.fill((0, 0));
            self.epoch = 1;
        }
        let epoch = self.epoch;
        for &fp in keys {
            let mut at = fp as usize & MASK;
            loop {
                let slot = &mut self.slots[at];
                if slot.1 != epoch {
                    *slot = (fp, epoch);
                    break;
                }
                if slot.0 == fp {
                    return true;
                }
                at = (at + 1) & MASK;
            }
        }
        false
    }
}

impl<'a> DupKeys<'a> {
    pub(crate) fn new(
        fingerprints: &'a mut Vec<u64>,
        table: &'a mut SeenTable,
        enabled: bool,
    ) -> Self {
        fingerprints.clear();
        DupKeys {
            fingerprints,
            table,
            enabled,
        }
    }

    #[inline(always)]
    pub(crate) fn mark(&self) -> usize {
        self.fingerprints.len()
    }

    #[inline(always)]
    pub(crate) fn key(&mut self, key: &[u8]) {
        if self.enabled {
            self.fingerprints.push(FINGERPRINT.hash_one(key));
        }
    }

    /// Close the object whose keys start at `mark`.
    #[inline(always)]
    pub(crate) fn close(&mut self, mark: usize) -> Result<(), SinkAbort> {
        if !self.enabled {
            return Ok(());
        }
        let keys = &mut self.fingerprints[mark..];
        let repeated = match keys.len() {
            n if n <= PAIRWISE_MAX => (1..n).any(|i| keys[..i].contains(&keys[i])),
            n if n <= TABLE_MAX_KEYS => self.table.any_repeat(keys),
            _ => {
                keys.sort_unstable();
                keys.windows(2).any(|w| w[0] == w[1])
            }
        };
        self.fingerprints.truncate(mark);
        if repeated {
            Err(SinkAbort::DuplicateKey)
        } else {
            Ok(())
        }
    }
}

/// Integer VALUE for `i` via rb-sys's inline `LONG2NUM` (the header
/// macro: a Fixnum tagged inline when it fits, else a Bignum), which
/// saves the FFI call per integer that `rb_ll2inum` costs. The fixable
/// range is defined by the C `long`, which is 32-bit on Windows (LLP64):
/// fixnums there hold only 31 bits, and tagging anything wider crashes
/// Ruby with "Unnormalized Fixnum value"; values beyond `long` take
/// `rb_ll2inum`.
// The conversion is an identity on LP64 hosts (clippy flags it there)
// but narrows on Windows, where c_long is 32-bit.
#[allow(clippy::useless_conversion, clippy::unnecessary_fallible_conversions)]
#[inline(always)]
fn int_to_raw(i: i64) -> rb_sys::VALUE {
    match std::os::raw::c_long::try_from(i) {
        Ok(l) => rb_sys::macros::LONG2NUM(l),
        Err(_) => unsafe { rb_sys::rb_ll2inum(i) },
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
    pub(crate) allow_duplicate_key: bool,
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

    /// Lone-surrogate content (the crate hands it over as WTF-8): json 3
    /// rejects it, trailing surrogates included.
    fn str_bytes(&mut self, _: &[u8]) -> Result<(), SinkAbort> {
        Err(SinkAbort::LoneSurrogate)
    }

    fn key_bytes(&mut self, _: &[u8]) -> Result<(), SinkAbort> {
        Err(SinkAbort::LoneSurrogate)
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
        // A repeated key collapses into one entry: the hash comes out
        // smaller than the pair count (one size read per object).
        if !self.allow_duplicate_key
            && (unsafe { rb_sys::macros::RHASH_SIZE(hash_raw) } as usize) < pairs
        {
            return Err(SinkAbort::DuplicateKey);
        }
        self.push_raw(hash_raw)
    }
}

/// Validation-only sink: every event is a no-op except nesting-depth
/// tracking and duplicate-key fingerprints, so `NOSJ.valid?` runs the
/// full parser (tokenizers, string decode, number validation) without
/// allocating a single VALUE.
pub(crate) struct NullSink<'a> {
    pub(crate) depth: usize,
    pub(crate) max_nesting: usize,
    pub(crate) dup_keys: DupKeys<'a>,
}

impl nosj::Sink for NullSink<'_> {
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
    fn key(&mut self, key: &str) -> Result<(), SinkAbort> {
        self.dup_keys.key(key.as_bytes());
        Ok(())
    }
    fn str_bytes(&mut self, _: &[u8]) -> Result<(), SinkAbort> {
        Err(SinkAbort::LoneSurrogate)
    }
    fn key_bytes(&mut self, _: &[u8]) -> Result<(), SinkAbort> {
        Err(SinkAbort::LoneSurrogate)
    }
    fn mark(&self) -> usize {
        self.dup_keys.mark()
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
    fn end_object(&mut self, mark: usize, _: usize) -> Result<(), SinkAbort> {
        self.depth -= 1;
        self.dup_keys.close(mark)
    }
}
