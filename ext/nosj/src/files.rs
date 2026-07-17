//! File entry points. `load_file` reads natively into a reused Rust
//! buffer, so a document never round-trips through a throwaway Ruby
//! String (the string allocation and its GC churn are ~37% of
//! `parse(File.read(path))` on a 570 KB document). `write_file`
//! streams the generator's pooled buffer straight to disk. The mmap
//! trio (`load_lazy_file`, `at_pointer_file`, `dig_file`) maps the
//! file read-only, so partial access never reads unused pages off
//! disk.
//!
//! I/O failures raise the mapped `Errno::*` exception, like `File`
//! methods do.

use std::cell::RefCell;
use std::fs;
use std::io::Read;

use magnus::value::ReprValue;
use magnus::{Error, RArray, RString, Ruby, Value};

use crate::errors::{parser_error_at, runtime_error};
use crate::gen;
use crate::lazy::{self, DocBytes};
use crate::parse::{err, materialize, materialize_at, parse_native_opts, span_of, ParseNativeOpts};
use crate::pointer::path_to_pointer;
use crate::state::PULL_STATE;

const NOT_UTF8: &str = "input is not valid UTF-8";

thread_local! {
    /// Reused read buffer for `load_file`: capacity survives across
    /// calls, so a hot loop over files allocates nothing.
    static FILE_BUF: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// On Unix the raw OS error IS the errno `rb_syserr_new` expects.
#[cfg(unix)]
fn errno_of(e: &std::io::Error) -> Option<i32> {
    e.raw_os_error()
}

/// On Windows the raw OS error is a Win32 code, not a POSIX errno
/// (ERROR_PATH_NOT_FOUND = 3 read as errno 3 raised Errno::ESRCH on
/// CI), so map the portable ErrorKind onto the classic CRT errno
/// values Ruby's Errno classes use. Unmapped kinds fall back to a
/// plain RuntimeError with the message.
#[cfg(windows)]
fn errno_of(e: &std::io::Error) -> Option<i32> {
    const ENOENT: i32 = 2;
    const EACCES: i32 = 13;
    const EEXIST: i32 = 17;
    match e.kind() {
        std::io::ErrorKind::NotFound => Some(ENOENT),
        std::io::ErrorKind::PermissionDenied => Some(EACCES),
        std::io::ErrorKind::AlreadyExists => Some(EEXIST),
        _ => None,
    }
}

/// Map an I/O failure onto the matching `Errno::*` exception (class
/// parity with `File.read`/`File.write`; the message carries the path).
fn io_error(ruby: &Ruby, path: &str, e: &std::io::Error) -> Error {
    use magnus::rb_sys::FromRawValue;
    let Some(errno) = errno_of(e) else {
        return runtime_error(ruby, format!("{e} - {path}"));
    };
    let Ok(cpath) = std::ffi::CString::new(path) else {
        return runtime_error(ruby, format!("{e} - {path}"));
    };
    // SAFETY: rb_syserr_new returns a live Errno exception instance.
    let exc = unsafe { Value::from_raw(rb_sys::rb_syserr_new(errno, cpath.as_ptr())) };
    match magnus::Exception::from_value(exc) {
        Some(exc) => exc.into(),
        None => runtime_error(ruby, format!("{e} - {path}")),
    }
}

/// `NOSJ.load_file(path, opts)`: parse a file without ever creating a
/// file-sized Ruby String.
pub fn load_file_native(
    ruby: &Ruby,
    _rb_self: Value,
    path: RString,
    opts: Value,
) -> Result<Value, Error> {
    let o = parse_native_opts(ruby, opts)?;
    let p = path.to_string()?;
    FILE_BUF.with(|cell| {
        let mut buf = cell
            .try_borrow_mut()
            .map_or_else(|_| Vec::new(), |mut b| std::mem::take(&mut *b));
        buf.clear();
        let result = read_into(&mut buf, &p)
            .map_err(|e| io_error(ruby, &p, &e))
            .and_then(|()| {
                if std::str::from_utf8(&buf).is_err() {
                    return Err(err(ruby, NOT_UTF8.into()));
                }
                materialize(ruby, &buf, &o)
            });
        if let Ok(mut slot) = cell.try_borrow_mut() {
            *slot = buf;
        }
        result
    })
}

fn read_into(buf: &mut Vec<u8>, path: &str) -> std::io::Result<()> {
    let mut f = fs::File::open(path)?;
    let hint = f.metadata().map(|m| m.len() as usize).unwrap_or(0);
    buf.reserve(hint.saturating_add(1));
    f.read_to_end(buf)?;
    Ok(())
}

/// `NOSJ.write_file(path, obj, opts)`: generate under the usual
/// options and write the bytes straight from the generator's pooled
/// buffer. Returns the byte count, like `File.write`.
pub fn write_file_native(
    ruby: &Ruby,
    _rb_self: Value,
    path: RString,
    obj: Value,
    opts: Value,
) -> Result<usize, Error> {
    let p = path.to_string()?;
    gen::generate_bytes_into(ruby, obj, opts, |ruby, bytes| {
        fs::write(&p, bytes).map_err(|e| io_error(ruby, &p, &e))?;
        Ok(bytes.len())
    })
}

/// `NOSJ.write_lines(path, values, opts)`: generate NDJSON (one
/// document per element, newline-terminated) and write the bytes
/// straight from the generator's pooled buffer. Returns the byte
/// count, like `File.write`.
pub fn write_lines_native(
    ruby: &Ruby,
    _rb_self: Value,
    path: RString,
    values: magnus::RArray,
    opts: Value,
) -> Result<usize, Error> {
    let p = path.to_string()?;
    gen::generate_lines_bytes_into(ruby, values, opts, |ruby, bytes| {
        fs::write(&p, bytes).map_err(|e| io_error(ruby, &p, &e))?;
        Ok(bytes.len())
    })
}

/// Map `path` read-only and hand a UTF-8-checked view to `f`.
pub(crate) fn with_mapped_file<R>(
    ruby: &Ruby,
    path: &str,
    f: impl FnOnce(memmap2::Mmap) -> Result<R, Error>,
) -> Result<R, Error> {
    let file = fs::File::open(path).map_err(|e| io_error(ruby, path, &e))?;
    // SAFETY: the mapping is read-only; concurrent modification of the
    // file by another process is documented as unsupported (the
    // standard mmap caveat, shared with every mmap-based parser).
    let map = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| io_error(ruby, path, &e))?;
    if std::str::from_utf8(&map).is_err() {
        return Err(err(ruby, NOT_UTF8.into()));
    }
    f(map)
}

/// `NOSJ.load_lazy_file(path, opts)`: a lazy document over a read-only
/// file mapping. Creation costs one sequential UTF-8 scan; access is
/// the usual lazy resolution, and pages outside the touched paths are
/// never read.
pub fn load_lazy_file_native(
    ruby: &Ruby,
    _rb_self: Value,
    path: RString,
    opts: Value,
) -> Result<Value, Error> {
    let o = parse_native_opts(ruby, opts)?;
    let p = path.to_string()?;
    with_mapped_file(ruby, &p, |map| {
        lazy::wrap_root(ruby, DocBytes::Mmap(map), o)
    })
}

/// Resolve one pointer against a mapped file and materialize the
/// matched subtree; the mapping is dropped before returning.
fn resolve_file_pointer(
    ruby: &Ruby,
    path: &str,
    pointer: &str,
    o: &ParseNativeOpts,
) -> Result<Value, Error> {
    with_mapped_file(ruby, path, |map| {
        let resolved = PULL_STATE.with(|cell| {
            let mut state = cell.borrow_mut();
            // SAFETY: UTF-8 checked by with_mapped_file.
            unsafe { nosj::pointer_utf8_unchecked(&map, pointer, &mut state.bufs) }
        });
        match resolved {
            Ok(None) => Ok(ruby.qnil().as_value()),
            Ok(Some(slice)) => {
                let (start, end) = span_of(&map, slice.as_bytes());
                materialize_at(ruby, &map, start, end, o)
            }
            Err(e) if matches!(e.kind, nosj::ErrorKind::InvalidPointer) => {
                Err(Error::new(ruby.exception_arg_error(), e.to_string()))
            }
            Err(e) => Err(parser_error_at(ruby, &map, e.offset, e.to_string())),
        }
    })
}

/// `NOSJ.at_pointer_file(path, pointer, opts)`: `NOSJ.at_pointer`
/// against a file, without reading it into Ruby.
pub fn at_pointer_file_native(
    ruby: &Ruby,
    _rb_self: Value,
    path: RString,
    pointer: RString,
    opts: Value,
) -> Result<Value, Error> {
    let o = parse_native_opts(ruby, opts)?;
    let p = path.to_string()?;
    let ptr = pointer.to_string()?;
    resolve_file_pointer(ruby, &p, &ptr, &o)
}

/// `NOSJ.dig_file(path, *dig_path)`: `NOSJ.dig` against a file.
/// Negative indices resolve to nil, as everywhere else.
pub fn dig_file_native(
    ruby: &Ruby,
    _rb_self: Value,
    path: RString,
    dig_path: RArray,
) -> Result<Value, Error> {
    let p = path.to_string()?;
    match path_to_pointer(ruby, dig_path)? {
        Some(ptr) => resolve_file_pointer(ruby, &p, &ptr, &ParseNativeOpts::default()),
        None => Ok(ruby.qnil().as_value()),
    }
}
