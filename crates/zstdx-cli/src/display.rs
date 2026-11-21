//! Terminal display with the reference zstd's line endings.
//!
//! zstd prints through the C runtime, whose Windows text mode expands every
//! `\n` to `\r\n` on both stdout and stderr. Rust's streams are binary, so
//! the expansion is done here to keep the output byte-identical.

use std::{borrow::Cow, fmt, io, io::Write as _};

/// Expand `\n` to `\r\n`; a `\n` already preceded by `\r` passes through.
#[cfg(windows)]
fn expand_newlines(buf: &[u8]) -> Cow<'_, [u8]> {
    if !buf.contains(&b'\n') {
        return Cow::Borrowed(buf);
    }
    let mut expanded = Vec::with_capacity(buf.len() * 2);
    let mut previous_cr = false;
    for &byte in buf {
        if byte == b'\n' && !previous_cr {
            expanded.push(b'\r');
        }
        previous_cr = byte == b'\r';
        expanded.push(byte);
    }
    Cow::Owned(expanded)
}

#[cfg(not(windows))]
fn expand_newlines(buf: &[u8]) -> Cow<'_, [u8]> {
    Cow::Borrowed(buf)
}

/// Write formatted text to a stream, panicking on failure like the std
/// printing macros do.
fn write_text(sink: &mut dyn io::Write, text: &str, stream: &str) {
    let expanded = expand_newlines(text.as_bytes());
    if let Err(err) = sink.write_all(&expanded) {
        panic!("failed printing to {stream}: {err}");
    }
}

/// `println!` routed through the Windows newline expansion.
pub(crate) fn out_line(args: fmt::Arguments<'_>) {
    write_text(&mut io::stdout().lock(), &format!("{args}\n"), "stdout");
}

/// `eprintln!` routed through the Windows newline expansion.
pub(crate) fn err_line(args: fmt::Arguments<'_>) {
    write_text(&mut io::stderr(), &format!("{args}\n"), "stderr");
}

/// `eprint!` counterpart of [`err_line`] for lines assembled piecewise.
pub(crate) fn err_piece(args: fmt::Arguments<'_>) {
    write_text(&mut io::stderr(), &format!("{args}"), "stderr");
}

/// Raw display text on stdout, flushed immediately: for payloads that carry
/// their own line endings (bench's `\r` progress updates). Write failures
/// are ignored, like the reference's `fprintf`.
pub(crate) fn out_flushed(text: &str) {
    let mut out = io::stdout().lock();
    let _ = out.write_all(&expand_newlines(text.as_bytes()));
    let _ = out.flush();
}

/// Macros over the functions above, keeping the std printing-macro call
/// shape at the (many) display sites.
macro_rules! outln {
    () => {
        $crate::display::out_line(format_args!(""))
    };
    ($($arg:tt)*) => {
        $crate::display::out_line(format_args!($($arg)*))
    };
}
pub(crate) use outln;

macro_rules! errln {
    ($($arg:tt)*) => {
        $crate::display::err_line(format_args!($($arg)*))
    };
}
pub(crate) use errln;

macro_rules! err {
    ($($arg:tt)*) => {
        $crate::display::err_piece(format_args!($($arg)*))
    };
}
pub(crate) use err;

/// `tracing_subscriber` adapter: event text reaches stderr as whole
/// formatted records, so the same expansion applies to summary lines.
pub(crate) struct LineEndings<M>(M);

impl<M> LineEndings<M> {
    pub(crate) fn new(inner: M) -> Self {
        Self(inner)
    }
}

impl<'a, M: tracing_subscriber::fmt::MakeWriter<'a>> tracing_subscriber::fmt::MakeWriter<'a>
    for LineEndings<M>
{
    type Writer = LineEndingsWriter<M::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        LineEndingsWriter(self.0.make_writer())
    }
}

pub(crate) struct LineEndingsWriter<W>(W);

impl<W: io::Write> io::Write for LineEndingsWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let expanded = expand_newlines(buf);
        self.0.write_all(&expanded)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}
