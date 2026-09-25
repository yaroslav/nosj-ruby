//! Generation failure carriers and their mapping onto the gem's
//! exception classes (NOSJ::GeneratorError, NOSJ::NestingError, or a
//! re-raise of the user's own exception).

use magnus::error::ErrorType;
use magnus::rb_sys::AsRawValue;
use magnus::value::ReprValue;
use magnus::{Error, Ruby};

use super::ruby::{protected_to_s, rstring_bytes};
use crate::errors::nosj_exception;

pub(super) enum GenFail {
    /// Re-raise a Ruby exception captured by a protected call (user
    /// `to_json` or `to_s`).
    Reraise(Error),
    /// NOSJ::GeneratorError with this message.
    Generator(String),
    /// NOSJ::GeneratorError carrying the caught exception's message
    /// (encoding conversion failures; the gem wraps these the same way).
    GeneratorFrom(Error),
    /// NOSJ::NestingError naming the configured limit.
    Nesting(usize),
}

/// The exception's `to_s` (its message), matching what the gem embeds
/// when it wraps a secondary exception. Protected: an exception class
/// may override `to_s`, and a raise there propagates in place of the
/// GeneratorError (as it does from the gem).
fn error_message(err: &Error) -> Result<String, Error> {
    if let ErrorType::Exception(exc) = err.error_type() {
        let s = protected_to_s(exc.as_value().as_raw())?;
        // Safety: rb_obj_as_string returns a T_STRING; the bytes are
        // copied into an owned String before any further Ruby call.
        let bytes = unsafe { rstring_bytes(s) };
        return Ok(String::from_utf8_lossy(bytes).into_owned());
    }
    Ok(err.to_string())
}

pub(super) fn raise_fail(ruby: &Ruby, fail: GenFail) -> Error {
    match fail {
        GenFail::Reraise(err) => err,
        GenFail::Generator(msg) => Error::new(nosj_exception(ruby, "GeneratorError"), msg),
        GenFail::GeneratorFrom(err) => match error_message(&err) {
            Ok(msg) => Error::new(nosj_exception(ruby, "GeneratorError"), msg),
            Err(raised) => raised,
        },
        GenFail::Nesting(limit) => Error::new(
            nosj_exception(ruby, "NestingError"),
            format!(
                "nesting of {limit} is too deep. Did you try to serialize objects with circular references?"
            ),
        ),
    }
}
