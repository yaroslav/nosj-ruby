//! JSON.generate-compatible option decoding: formatting strings, escape
//! mode, nesting limits, and the buffer size hint.

use magnus::value::ReprValue;
use magnus::{Error, RHash, RString, Ruby, Value};
use nosj::emit::EscapeMode;

pub(crate) struct GenConfig {
    pub(super) indent: Vec<u8>,
    pub(super) space: Vec<u8>,
    pub(super) space_before: Vec<u8>,
    pub(super) object_nl: Vec<u8>,
    pub(super) array_nl: Vec<u8>,
    /// 0 = unlimited.
    pub(super) max_nesting: usize,
    pub(super) start_depth: usize,
    pub(super) allow_nan: bool,
    pub(super) strict: bool,
    /// ActiveSupport walk semantics: non-native values recurse through
    /// as_json instead of splicing to_json, and non-finite floats emit
    /// null (Float#as_json parity). Set only by the Rails entry, never
    /// from user option hashes.
    pub(super) rails: bool,
    pub(super) mode: EscapeMode,
    /// Precomputed "any formatting string set": scanning the five
    /// vectors per call was measurable on tiny documents.
    pub(super) pretty: bool,
}

/// The nil-options configuration, shared instead of rebuilt: stamping
/// a fresh ~140-byte GenConfig onto the stack per call was measurable
/// on tiny documents (the json gem likewise reuses a cached State for
/// the default options). Safe as a static: `Vec::new` is const and
/// allocation-free, and generation only ever borrows the config.
pub(crate) static DEFAULT_CONFIG: GenConfig = GenConfig {
    indent: Vec::new(),
    space: Vec::new(),
    space_before: Vec::new(),
    object_nl: Vec::new(),
    array_nl: Vec::new(),
    max_nesting: 100,
    start_depth: 0,
    allow_nan: false,
    strict: false,
    rails: false,
    mode: EscapeMode::Standard,
    pretty: false,
};

/// The Rails-encoder configuration for ActiveSupport's default escape
/// flags (HTML entities and JS separators both on, the overwhelmingly
/// common case): escaping is fused into the crate's HtmlSafe kernels,
/// one pass, no post-scan.
pub(super) static RAILS_HTML_SAFE_CONFIG: GenConfig = GenConfig {
    indent: Vec::new(),
    space: Vec::new(),
    space_before: Vec::new(),
    object_nl: Vec::new(),
    array_nl: Vec::new(),
    max_nesting: 100,
    start_depth: 0,
    allow_nan: false,
    strict: false,
    rails: true,
    mode: EscapeMode::HtmlSafe,
    pretty: false,
};

/// Rails-encoder configuration with HTML entities on and JS separators
/// off.
pub(super) static RAILS_HTML_ENTITIES_CONFIG: GenConfig = GenConfig {
    indent: Vec::new(),
    space: Vec::new(),
    space_before: Vec::new(),
    object_nl: Vec::new(),
    array_nl: Vec::new(),
    max_nesting: 100,
    start_depth: 0,
    allow_nan: false,
    strict: false,
    rails: true,
    mode: EscapeMode::HtmlEntities,
    pretty: false,
};

/// Rails-encoder configuration with JS separators on and HTML entities
/// off.
pub(super) static RAILS_JS_SEPARATORS_CONFIG: GenConfig = GenConfig {
    indent: Vec::new(),
    space: Vec::new(),
    space_before: Vec::new(),
    object_nl: Vec::new(),
    array_nl: Vec::new(),
    max_nesting: 100,
    start_depth: 0,
    allow_nan: false,
    strict: false,
    rails: true,
    mode: EscapeMode::JsSeparators,
    pretty: false,
};

/// The Rails-encoder configuration with every escape flag off
/// (encode(escape: false)). Mirrors JSONGemEncoder#stringify, which
/// generates with the json gem's defaults.
pub(super) static RAILS_CONFIG: GenConfig = GenConfig {
    indent: Vec::new(),
    space: Vec::new(),
    space_before: Vec::new(),
    object_nl: Vec::new(),
    array_nl: Vec::new(),
    max_nesting: 100,
    start_depth: 0,
    allow_nan: false,
    strict: false,
    rails: true,
    mode: EscapeMode::Standard,
    pretty: false,
};

impl Default for GenConfig {
    fn default() -> Self {
        GenConfig {
            indent: Vec::new(),
            space: Vec::new(),
            space_before: Vec::new(),
            object_nl: Vec::new(),
            array_nl: Vec::new(),
            max_nesting: 100,
            start_depth: 0,
            allow_nan: false,
            strict: false,
            rails: false,
            mode: EscapeMode::Standard,
            pretty: false,
        }
    }
}

fn opt_bytes(ruby: &Ruby, opts: RHash, name: &str) -> Result<Option<Vec<u8>>, Error> {
    let v: Value = opts
        .get(ruby.to_symbol(name))
        .unwrap_or_else(|| ruby.qnil().as_value());
    if v.is_nil() {
        return Ok(None);
    }
    let s = RString::from_value(v).ok_or_else(|| {
        Error::new(
            ruby.exception_type_error(),
            format!("{name} must be a String"),
        )
    })?;
    Ok(Some(unsafe { s.as_slice() }.to_vec()))
}

fn opt_bool(ruby: &Ruby, opts: RHash, name: &str) -> Option<bool> {
    let v: Value = opts.get(ruby.to_symbol(name))?;
    if v.is_nil() {
        None
    } else {
        Some(v.to_bool())
    }
}

/// Decode a non-nil options hash (nil takes [`DEFAULT_CONFIG`] at the
/// call site without constructing anything).
pub(crate) fn parse_gen_opts(ruby: &Ruby, opts: Value) -> Result<(GenConfig, usize), Error> {
    let mut cfg = GenConfig::default();
    let mut cap_hint = 0usize;
    if opts.is_nil() {
        return Ok((cfg, cap_hint));
    }
    let opts = RHash::from_value(opts)
        .ok_or_else(|| Error::new(ruby.exception_type_error(), "options must be a Hash or nil"))?;

    if let Some(v) = opt_bytes(ruby, opts, "indent")? {
        cfg.indent = v;
    }
    if let Some(v) = opt_bytes(ruby, opts, "space")? {
        cfg.space = v;
    }
    if let Some(v) = opt_bytes(ruby, opts, "space_before")? {
        cfg.space_before = v;
    }
    if let Some(v) = opt_bytes(ruby, opts, "object_nl")? {
        cfg.object_nl = v;
    }
    if let Some(v) = opt_bytes(ruby, opts, "array_nl")? {
        cfg.array_nl = v;
    }
    if let Some(v) = opt_bool(ruby, opts, "allow_nan") {
        cfg.allow_nan = v;
    }
    if let Some(v) = opt_bool(ruby, opts, "strict") {
        cfg.strict = v;
    }
    let ascii = opt_bool(ruby, opts, "ascii_only").unwrap_or(false);
    let script = opt_bool(ruby, opts, "script_safe").unwrap_or(false)
        || opt_bool(ruby, opts, "escape_slash").unwrap_or(false);
    if ascii {
        cfg.mode = EscapeMode::AsciiOnly;
        if script {
            return Err(Error::new(
                ruby.exception_arg_error(),
                "NOSJ.generate: ascii_only and script_safe cannot be combined",
            ));
        }
    } else if script {
        cfg.mode = EscapeMode::ScriptSafe;
    }
    if let Some(v) = opts.get(ruby.to_symbol("max_nesting")) {
        let v: Value = v;
        // nil/false → unlimited; true → keep the default 100; Integer → limit.
        if !v.to_bool() {
            cfg.max_nesting = 0;
        } else if let Ok(n) = <i64 as magnus::TryConvert>::try_convert(v) {
            cfg.max_nesting = if n <= 0 { 0 } else { n as usize };
        }
    }
    if let Some(v) = opts.get(ruby.to_symbol("depth")) {
        let v: Value = v;
        if let Ok(n) = <i64 as magnus::TryConvert>::try_convert(v) {
            cfg.start_depth = if n <= 0 { 0 } else { n as usize };
        }
    }
    if let Some(v) = opts.get(ruby.to_symbol("buffer_initial_length")) {
        let v: Value = v;
        if let Ok(n) = <i64 as magnus::TryConvert>::try_convert(v) {
            if n > 0 {
                cap_hint = n as usize;
            }
        }
    }
    cfg.pretty = !(cfg.indent.is_empty()
        && cfg.space.is_empty()
        && cfg.space_before.is_empty()
        && cfg.object_nl.is_empty()
        && cfg.array_nl.is_empty());
    Ok((cfg, cap_hint))
}
