//! The generation walker: recursive descent over a Ruby object graph,
//! emitting JSON bytes through nosj's escape and number kernels.
//! Compact and pretty modes are one const-generic body, so the compact
//! hot path carries no formatting branches.

use nosj::emit::{self, copy_short_raw, EscapeMode};
use rb_sys::macros::{
    FIX2LONG, FIXNUM_P, FLONUM_P, RARRAY_CONST_PTR, RARRAY_LEN, RB_BUILTIN_TYPE, RHASH_SIZE,
    STATIC_SYM_P,
};
use rb_sys::{ruby_value_type, VALUE};

use super::errors::GenFail;
use super::keys::GenKeyCache;
use super::opts::GenConfig;
use super::ruby::{
    is_json_fragment, is_special_const, protected_as_json, protected_encode_utf8,
    protected_to_json, protected_to_s, rstring_bytes, str_coderange, str_enc_index, to_json_id,
    utf8_encindexes, CR_7BIT, CR_VALID, QFALSE, QNIL, QTRUE,
};

/// Whether keys escaped under `mode` may be cached: the cached bytes
/// bake in the escape mode, so the scratch keeps one cache per
/// cacheable mode and hands `Gen` the matching one.
pub(super) fn mode_cacheable(mode: EscapeMode) -> bool {
    matches!(mode, EscapeMode::Standard | EscapeMode::HtmlSafe)
}

pub(super) struct Gen<'a> {
    /// The pooled per-thread output buffer, borrowed for the call.
    pub(super) out: &'a mut Vec<u8>,
    pub(super) cfg: &'a GenConfig,
    pub(super) fail: Option<GenFail>,
    /// Pre-escaped key cache, borrowed for the whole document (one
    /// thread-local borrow per generate call instead of one per key).
    /// Always the cache matching `cfg.mode` (see [`mode_cacheable`]).
    pub(super) keys: &'a mut GenKeyCache,
}

impl Gen<'_> {
    #[inline]
    fn push_indent(&mut self, n: usize) {
        for _ in 0..n {
            self.out.extend_from_slice(&self.cfg.indent);
        }
    }

    /// Quote and escape a Ruby String VALUE. Transcodes non-UTF-8 input and
    /// rejects broken UTF-8, both matching the gem.
    fn emit_rstring_quoted(&mut self, s: VALUE) -> Result<(), ()> {
        let cr = str_coderange(s);
        let valid = cr == CR_7BIT
            || (cr == CR_VALID && {
                let idx = str_enc_index(s);
                let (utf8, usascii) = utf8_encindexes();
                idx == utf8 || idx == usascii
            });
        let s = if valid {
            s
        } else {
            self.convert_invalid_encoding(s)?
        };
        // Bytes are copied into `out` before any further Ruby call, so the
        // temporary (if transcoded) cannot be collected mid-read.
        let bytes = unsafe { rstring_bytes(s) };
        self.out.reserve(bytes.len() + 2);
        self.out.push(b'"');
        emit::escape_into(&mut *self.out, bytes, self.cfg.mode);
        self.out.push(b'"');
        Ok(())
    }

    /// Cold path: string is not known-good UTF-8. Broken UTF-8-tagged input
    /// is malformed (encode to the same encoding is a no-op, so the gem ends
    /// up rejecting it too); anything else goes through the raising encode
    /// and either converts or surfaces as GeneratorError with the exception's
    /// message (the gem's exact behavior).
    #[cold]
    fn convert_invalid_encoding(&mut self, s: VALUE) -> Result<VALUE, ()> {
        let (utf8, _) = utf8_encindexes();
        if str_enc_index(s) == utf8 {
            self.fail = Some(GenFail::Generator(
                "source sequence is illegal/malformed utf-8".to_string(),
            ));
            return Err(());
        }
        match protected_encode_utf8(s) {
            Ok(v) => {
                if str_coderange(v) == CR_7BIT || str_coderange(v) == CR_VALID {
                    Ok(v)
                } else {
                    self.fail = Some(GenFail::Generator(
                        "source sequence is illegal/malformed utf-8".to_string(),
                    ));
                    Err(())
                }
            }
            Err(exc) => {
                self.fail = Some(GenFail::GeneratorFrom(exc));
                Err(())
            }
        }
    }

    /// Append a Ruby String VALUE's bytes verbatim (bignum digits, to_json
    /// results; already JSON).
    fn append_rstring_raw(&mut self, s: VALUE) {
        let bytes = unsafe { rstring_bytes(s) };
        self.out.extend_from_slice(bytes);
    }

    fn emit_float(&mut self, f: f64) -> Result<(), ()> {
        if f.is_finite() {
            emit::write_f64(&mut *self.out, f);
            return Ok(());
        }
        if self.cfg.rails {
            // ActiveSupport's Float#as_json: non-finite floats are null.
            self.out.extend_from_slice(b"null");
            return Ok(());
        }
        let name = if f.is_nan() {
            "NaN"
        } else if f > 0.0 {
            "Infinity"
        } else {
            "-Infinity"
        };
        if self.cfg.allow_nan {
            self.out.extend_from_slice(name.as_bytes());
            Ok(())
        } else {
            self.fail = Some(GenFail::Generator(format!("{name} not allowed in JSON")));
            Err(())
        }
    }

    /// Emit an object key from the pre-escaped cache. Only frozen string
    /// keys in a cacheable escape mode qualify: frozen guarantees the
    /// content behind the VALUE can't change, and the cached bytes bake
    /// in the escape mode, so each cacheable mode gets its own cache
    /// instance from the scratch (see [`mode_cacheable`]).
    fn emit_key_cached(&mut self, k: VALUE) -> Result<(), ()> {
        const FL_FREEZE: u64 = rb_sys::ruby_fl_type::RUBY_FL_FREEZE as u64;
        let frozen = unsafe { (*(k as *const rb_sys::RBasic)).flags } & FL_FREEZE != 0;
        if !frozen || !mode_cacheable(self.cfg.mode) {
            return self.emit_rstring_quoted(k);
        }
        if let Some(bytes) = self.keys.get(k) {
            if bytes.len() <= 16 {
                emit::push_short(&mut *self.out, bytes);
            } else {
                self.out.extend_from_slice(bytes);
            }
            return Ok(());
        }
        let start = self.out.len();
        self.emit_rstring_quoted(k)?;
        self.keys.store(k, self.out[start..].into());
        Ok(())
    }

    /// The pre-escaped bytes for `k` when the cache may serve it:
    /// frozen string key, cacheable escape mode (see
    /// [`Gen::emit_key_cached`]). An associated fn over the split-out
    /// fields so callers keep `self.out` free.
    #[inline(always)]
    fn cached_key_bytes<'k>(cfg: &GenConfig, keys: &'k GenKeyCache, k: VALUE) -> Option<&'k [u8]> {
        const FL_FREEZE: u64 = rb_sys::ruby_fl_type::RUBY_FL_FREEZE as u64;
        if mode_cacheable(cfg.mode)
            && !is_special_const(k)
            && unsafe { RB_BUILTIN_TYPE(k) } == ruby_value_type::RUBY_T_STRING
            && unsafe { (*(k as *const rb_sys::RBasic)).flags } & FL_FREEZE != 0
        {
            keys.get(k)
        } else {
            None
        }
    }

    /// Fused `,"key":` prefix for compact-mode object pairs: one
    /// reservation, raw stores for the separator, the cached
    /// pre-escaped key, and the colon, instead of three separate
    /// buffer operations (comma push, key copy, colon push) per pair.
    /// Twitter-class documents emit tens of thousands of pairs, almost
    /// all through the cache-hit path here. Misses take the plain path
    /// below, which also populates the cache.
    #[inline(always)]
    fn emit_pair_prefix_compact(&mut self, k: VALUE, comma: bool) -> Result<(), ()> {
        if let Some(bytes) = Self::cached_key_bytes(self.cfg, self.keys, k) {
            let n = bytes.len();
            self.out.reserve(n + 2);
            // SAFETY: `n + 2` bytes reserved above; `bytes` borrows
            // the key cache, disjoint from the output.
            unsafe {
                let len = self.out.len();
                let base = self.out.as_mut_ptr().add(len);
                let mut w = 0usize;
                if comma {
                    *base = b',';
                    w = 1;
                }
                copy_short_raw(bytes.as_ptr(), base.add(w), n);
                w += n;
                *base.add(w) = b':';
                self.out.set_len(len + w + 1);
            }
            return Ok(());
        }
        if comma {
            self.out.push(b',');
        }
        self.emit_key(k)?;
        self.out.push(b':');
        Ok(())
    }

    /// Fused `,"key":123` for compact int-valued pairs (citm-class
    /// documents are walls of these: prices, ids, seat numbers): the
    /// separator, cached key, colon, and digits in one reservation and
    /// one raw cursor.
    #[inline(always)]
    fn emit_pair_int_compact(&mut self, k: VALUE, comma: bool, value: i64) -> Result<(), ()> {
        if let Some(bytes) = Self::cached_key_bytes(self.cfg, self.keys, k) {
            let n = bytes.len();
            self.out.reserve(n + 2 + emit::I64_MAX_LEN);
            // SAFETY: the reservation covers separator + key + colon +
            // I64_MAX_LEN digits; `bytes` borrows the key cache,
            // disjoint from the output.
            unsafe {
                let len = self.out.len();
                let base = self.out.as_mut_ptr().add(len);
                let mut w = 0usize;
                if comma {
                    *base = b',';
                    w = 1;
                }
                copy_short_raw(bytes.as_ptr(), base.add(w), n);
                w += n;
                *base.add(w) = b':';
                w += 1;
                w += emit::write_i64_raw(base.add(w), value);
                self.out.set_len(len + w);
            }
            return Ok(());
        }
        self.emit_pair_prefix_compact(k, comma)?;
        emit::write_i64(&mut *self.out, value);
        Ok(())
    }

    /// Object/hash key: String and Symbol direct, anything else via to_s
    /// (the gem's key coercion).
    fn emit_key(&mut self, k: VALUE) -> Result<(), ()> {
        if !is_special_const(k) {
            match unsafe { RB_BUILTIN_TYPE(k) } {
                ruby_value_type::RUBY_T_STRING => return self.emit_key_cached(k),
                ruby_value_type::RUBY_T_SYMBOL => {
                    let s = unsafe { rb_sys::rb_sym2str(k) };
                    return self.emit_rstring_quoted(s);
                }
                _ => {}
            }
        } else if STATIC_SYM_P(k) {
            let s = unsafe { rb_sys::rb_sym2str(k) };
            return self.emit_rstring_quoted(s);
        }
        match protected_to_s(k) {
            Ok(s) => self.emit_rstring_quoted(s),
            Err(exc) => {
                self.fail = Some(GenFail::Reraise(exc));
                Err(())
            }
        }
    }

    /// Non-native type: strict raises (except `JSON::Fragment`, which
    /// the gem splices even under strict); otherwise `to_json` if the
    /// object responds (result appended verbatim), else `to_s` as a
    /// JSON string, which is exactly what the gem's `Object#to_json`
    /// does. Rails mode recurses through `as_json` instead
    /// (JSONGemEncoder#jsonify).
    fn emit_fallback<const PRETTY: bool>(&mut self, raw: VALUE, depth: usize) -> Result<(), ()> {
        if self.cfg.rails {
            return self.emit_rails_fallback::<PRETTY>(raw, depth);
        }
        if self.cfg.strict {
            if is_json_fragment(raw) {
                return self.splice_to_json(raw);
            }
            let name = unsafe {
                std::ffi::CStr::from_ptr(rb_sys::rb_obj_classname(raw))
                    .to_string_lossy()
                    .into_owned()
            };
            self.fail = Some(GenFail::Generator(format!("{name} not allowed in JSON")));
            return Err(());
        }
        if unsafe { rb_sys::rb_respond_to(raw, to_json_id()) } != 0 {
            match protected_to_json(raw) {
                Ok(json) => {
                    if !is_special_const(json)
                        && unsafe { RB_BUILTIN_TYPE(json) } == ruby_value_type::RUBY_T_STRING
                    {
                        self.append_rstring_raw(json);
                        return Ok(());
                    }
                }
                Err(exc) => {
                    self.fail = Some(GenFail::Reraise(exc));
                    return Err(());
                }
            }
        }
        match protected_to_s(raw) {
            Ok(s) => self.emit_rstring_quoted(s),
            Err(exc) => {
                self.fail = Some(GenFail::Reraise(exc));
                Err(())
            }
        }
    }

    /// Splice `raw`'s `to_json` result verbatim: the JSON::Fragment
    /// path (pre-rendered JSON, trusted like the gem trusts it).
    fn splice_to_json(&mut self, raw: VALUE) -> Result<(), ()> {
        match protected_to_json(raw) {
            Ok(json)
                if !is_special_const(json)
                    && unsafe { RB_BUILTIN_TYPE(json) } == ruby_value_type::RUBY_T_STRING =>
            {
                self.append_rstring_raw(json);
                Ok(())
            }
            Ok(_) => {
                self.fail = Some(GenFail::Generator(
                    "JSON::Fragment#to_json did not return a String".to_string(),
                ));
                Err(())
            }
            Err(exc) => {
                self.fail = Some(GenFail::Reraise(exc));
                Err(())
            }
        }
    }

    /// Rails-mode fallback, mirroring JSONGemEncoder#jsonify:
    /// fragments splice through (like ActiveSupport passes them to the
    /// gem); everything else is asked for its as_json representation
    /// (no arguments; only the top-level value receives the encoder
    /// options), which is emitted in its place. An as_json returning
    /// the receiver would recurse forever, so it raises instead.
    fn emit_rails_fallback<const PRETTY: bool>(
        &mut self,
        raw: VALUE,
        depth: usize,
    ) -> Result<(), ()> {
        if is_json_fragment(raw) {
            return self.splice_to_json(raw);
        }
        match protected_as_json(raw) {
            Ok(json) if json == raw => {
                let name = unsafe {
                    std::ffi::CStr::from_ptr(rb_sys::rb_obj_classname(raw))
                        .to_string_lossy()
                        .into_owned()
                };
                self.fail = Some(GenFail::Generator(format!(
                    "{name}#as_json returned the receiver"
                )));
                Err(())
            }
            Ok(json) => self.emit_value::<PRETTY>(json, depth),
            Err(exc) => {
                self.fail = Some(GenFail::Reraise(exc));
                Err(())
            }
        }
    }

    fn nesting_check(&mut self, inner: usize) -> Result<(), ()> {
        if self.cfg.max_nesting > 0 && inner > self.cfg.max_nesting {
            self.fail = Some(GenFail::Nesting(self.cfg.max_nesting));
            return Err(());
        }
        Ok(())
    }

    fn emit_array<const PRETTY: bool>(&mut self, ary: VALUE, depth: usize) -> Result<(), ()> {
        let inner = depth + 1;
        self.nesting_check(inner)?;
        let len = unsafe { RARRAY_LEN(ary) } as usize;
        self.out.push(b'[');
        if len == 0 {
            self.out.push(b']');
            return Ok(());
        }
        let mut i = 0usize;
        while i < len {
            if i > 0 {
                self.out.push(b',');
            }
            if PRETTY {
                self.out.extend_from_slice(&self.cfg.array_nl);
                self.push_indent(inner);
            }
            // Re-read the pointer every element: an allocation inside the
            // recursion may trigger GC compaction and move the array.
            let elem = unsafe { *RARRAY_CONST_PTR(ary).add(i) };
            // Numeric runs (compact mode) emit through a raw local
            // cursor under one chunked reservation: per-element Vec
            // operations round-trip length and pointer through memory
            // and pay a capacity branch each, where the run keeps the
            // cursor in a register and writes commas as plain stores
            // (the C generator's shape). Numbers emit no Ruby calls,
            // so neither GC nor compaction can run inside a run: the
            // array pointer is cached for its duration. Non-finite
            // floats break out pre-comma to the allow_nan-aware slow
            // arm below.
            if !PRETTY && (FLONUM_P(elem) || FIXNUM_P(elem)) {
                /// Reservation ceiling per run (elements), so a huge
                /// numeric array reserves incrementally instead of
                /// worst-case-times-length at once.
                const RUN_CHUNK: usize = 4096;
                let run_start = i;
                let chunk = (len - i).min(RUN_CHUNK);
                self.out.reserve(chunk * (emit::F64_MAX_LEN + 1));
                // SAFETY: the reservation above covers `chunk`
                // elements at F64_MAX_LEN (>= I64_MAX_LEN) plus one
                // comma each; `w` tracks exactly the bytes written and
                // is published once on exit. No Ruby call happens
                // inside the run, so `ptr` (and the array) are stable.
                unsafe {
                    let base = self.out.as_mut_ptr();
                    let mut w = self.out.len();
                    let ptr = RARRAY_CONST_PTR(ary);
                    let chunk_end = i + chunk;
                    // The comma precedes every element but the run's
                    // first, and is only written once the element is
                    // known to belong to the run: every break path
                    // then leaves the output separator-clean, and the
                    // outer loop's `i > 0` arm owns the next comma.
                    let mut first = true;
                    loop {
                        let e = *ptr.add(i);
                        let mut float_val = 0.0f64;
                        let is_float = FLONUM_P(e);
                        if is_float {
                            float_val = rb_sys::macros::NUM2DBL(e);
                            if !float_val.is_finite() {
                                break;
                            }
                        } else if !FIXNUM_P(e) {
                            break;
                        }
                        if !first {
                            *base.add(w) = b',';
                            w += 1;
                        }
                        first = false;
                        w += if is_float {
                            emit::write_f64_raw(base.add(w), float_val)
                        } else {
                            emit::write_i64_raw(base.add(w), FIX2LONG(e) as i64)
                        };
                        i += 1;
                        if i == chunk_end {
                            break;
                        }
                    }
                    self.out.set_len(w);
                }
                // Anything the run consumed re-enters the outer loop,
                // whose `i > 0` arm supplies the pending comma. A run
                // that consumed nothing (first element is a non-finite
                // flonum) falls through to the slow arms instead: its
                // comma is already written.
                if i > run_start {
                    continue;
                }
            }
            // Scalar fast arms for the remaining shapes, hoisted out
            // of the recursive emit_value (recursion blocks inlining,
            // so every element otherwise pays a full call). FLONUM
            // decodes inline via rb-sys's stable API; heap Floats
            // (rare) still avoid the call boundary.
            if FLONUM_P(elem) {
                self.emit_float(unsafe { rb_sys::macros::NUM2DBL(elem) })?;
                i += 1;
                continue;
            }
            if !is_special_const(elem)
                && unsafe { RB_BUILTIN_TYPE(elem) } == ruby_value_type::RUBY_T_FLOAT
            {
                self.emit_float(unsafe { rb_sys::macros::NUM2DBL(elem) })?;
                i += 1;
                continue;
            }
            self.emit_value::<PRETTY>(elem, inner)?;
            i += 1;
        }
        // The gem writes the closing newline+indent only when array_nl is
        // set; per-element indent above is unconditional.
        if PRETTY && !self.cfg.array_nl.is_empty() {
            self.out.extend_from_slice(&self.cfg.array_nl);
            self.push_indent(depth);
        }
        self.out.push(b']');
        Ok(())
    }

    /// Object pairs iterate through [`hash_iter::foreach_raw`] (the
    /// extension's one C-ABI exception; see that module's docs).
    /// Failures ride the `fail` flag + [`Step::Stop`], never a raise
    /// across these frames.
    fn emit_object<const PRETTY: bool>(&mut self, hash: VALUE, depth: usize) -> Result<(), ()> {
        use super::hash_iter::{foreach_raw, Step};
        let inner = depth + 1;
        self.nesting_check(inner)?;
        self.out.push(b'{');
        if unsafe { RHASH_SIZE(hash) } == 0 {
            self.out.push(b'}');
            return Ok(());
        }
        // SAFETY: emit_object is only reached for T_HASH values.
        if PRETTY {
            let mut first = true;
            unsafe {
                foreach_raw(hash, |k, v| {
                    if !first {
                        self.out.push(b',');
                    }
                    first = false;
                    self.out.extend_from_slice(&self.cfg.object_nl);
                    self.push_indent(inner);
                    if self.emit_key(k).is_err() {
                        return Step::Stop;
                    }
                    self.out.extend_from_slice(&self.cfg.space_before);
                    self.out.push(b':');
                    self.out.extend_from_slice(&self.cfg.space);
                    if self.emit_value::<true>(v, inner).is_err() {
                        return Step::Stop;
                    }
                    Step::Continue
                });
            }
        } else {
            unsafe {
                foreach_raw(hash, |k, v| {
                    // `out` always ends with '{' (just pushed) or the
                    // previous pair.
                    let comma = *self.out.last().unwrap_unchecked() != b'{';
                    // Int values fuse with their key into one write.
                    if FIXNUM_P(v) {
                        if self
                            .emit_pair_int_compact(k, comma, FIX2LONG(v) as i64)
                            .is_err()
                        {
                            return Step::Stop;
                        }
                        return Step::Continue;
                    }
                    if self.emit_pair_prefix_compact(k, comma).is_err() {
                        return Step::Stop;
                    }
                    // Value fast arms, mirroring emit_value's: one
                    // special-const branch splits heap values (strings
                    // dominate) from immediates (twitter-class objects
                    // carry a dozen-plus nulls and booleans per
                    // record). Every arm skips the non-inlinable
                    // recursive call.
                    if !is_special_const(v) {
                        if RB_BUILTIN_TYPE(v) == ruby_value_type::RUBY_T_STRING {
                            if self.emit_rstring_quoted(v).is_err() {
                                return Step::Stop;
                            }
                            return Step::Continue;
                        }
                    } else {
                        if v == QNIL {
                            self.out.extend_from_slice(b"null");
                            return Step::Continue;
                        }
                        if v == QTRUE {
                            self.out.extend_from_slice(b"true");
                            return Step::Continue;
                        }
                        if v == QFALSE {
                            self.out.extend_from_slice(b"false");
                            return Step::Continue;
                        }
                        if FLONUM_P(v) {
                            if self.emit_float(rb_sys::macros::NUM2DBL(v)).is_err() {
                                return Step::Stop;
                            }
                            return Step::Continue;
                        }
                    }
                    if self.emit_value::<false>(v, inner).is_err() {
                        return Step::Stop;
                    }
                    Step::Continue
                });
            }
        }
        if self.fail.is_some() {
            return Err(());
        }
        if PRETTY && !self.cfg.object_nl.is_empty() {
            self.out.extend_from_slice(&self.cfg.object_nl);
            self.push_indent(depth);
        }
        self.out.push(b'}');
        Ok(())
    }

    pub(super) fn emit_value<const PRETTY: bool>(
        &mut self,
        raw: VALUE,
        depth: usize,
    ) -> Result<(), ()> {
        // Heap objects first: strings/hashes/arrays dominate real documents.
        if !is_special_const(raw) {
            return match unsafe { RB_BUILTIN_TYPE(raw) } {
                ruby_value_type::RUBY_T_STRING => self.emit_rstring_quoted(raw),
                ruby_value_type::RUBY_T_HASH => self.emit_object::<PRETTY>(raw, depth),
                ruby_value_type::RUBY_T_ARRAY => self.emit_array::<PRETTY>(raw, depth),
                ruby_value_type::RUBY_T_FLOAT => {
                    self.emit_float(unsafe { rb_sys::macros::NUM2DBL(raw) })
                }
                ruby_value_type::RUBY_T_BIGNUM => {
                    let s = unsafe { rb_sys::rb_big2str(raw, 10) };
                    self.append_rstring_raw(s);
                    Ok(())
                }
                ruby_value_type::RUBY_T_SYMBOL => {
                    let s = unsafe { rb_sys::rb_sym2str(raw) };
                    self.emit_rstring_quoted(s)
                }
                _ => self.emit_fallback::<PRETTY>(raw, depth),
            };
        }
        if FIXNUM_P(raw) {
            emit::write_i64(&mut *self.out, unsafe { FIX2LONG(raw) } as i64);
            return Ok(());
        }
        // Flonums before the nil/true/false compares: floats dominate
        // real numeric documents while literals are rare, and a flonum
        // here decodes inline (rb-sys stable API), no FFI call.
        if FLONUM_P(raw) {
            return self.emit_float(unsafe { rb_sys::macros::NUM2DBL(raw) });
        }
        match raw {
            QNIL => {
                self.out.extend_from_slice(b"null");
                return Ok(());
            }
            QTRUE => {
                self.out.extend_from_slice(b"true");
                return Ok(());
            }
            QFALSE => {
                self.out.extend_from_slice(b"false");
                return Ok(());
            }
            _ => {}
        }
        if STATIC_SYM_P(raw) {
            let s = unsafe { rb_sys::rb_sym2str(raw) };
            return self.emit_rstring_quoted(s);
        }
        self.emit_fallback::<PRETTY>(raw, depth)
    }
}
