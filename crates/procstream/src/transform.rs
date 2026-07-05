//! Streaming byte transforms applied to a single captured stream.
//!
//! A [`Transform<T>`] is a factory: it records an ordered set of byte pre-stages
//! and a terminal *framer*, and mints a fresh [`Pipeline<T>`] of stateful filters
//! for each stream it is attached to, so one `Transform` can be reused across
//! many spawns.
//!
//! A pipeline has two halves. The pre-stages ([`ByteFilter`]s — ANSI stripping,
//! `\r` overwrite collapse, UTF-8 sanitizing) are byte-to-byte and run in a fixed
//! order regardless of builder call order: `ansi`, then `overwrite`, then `utf8`.
//! The terminal stage is a [`Framer`], which turns the byte stream into items of
//! its chosen `Item` type — that type is the transform's output type. `lines()`
//! produces [`Line`]s; the default produces `Vec<u8>` byte runs; `frame()` lets
//! you plug in any framer of your own.

use std::sync::Arc;

use vt_push_parser::VTPushParser;
use vt_push_parser::event::VTEvent;

const CR: u8 = b'\r';
const LF: u8 = b'\n';

/// The default cap on a single un-terminated line, used as a safety valve so a
/// stream that never emits a newline cannot grow the buffer without bound.
const DEFAULT_MAX_LINE: usize = 1 << 20;

/// The Unicode replacement character, `U+FFFD`, as UTF-8 bytes.
const REPLACEMENT: &[u8] = "\u{FFFD}".as_bytes();

/// How a framed line was terminated.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LineEnding {
    /// Terminated by `\n`.
    Lf,
    /// Terminated by `\r\n`.
    CrLf,
    /// Force-emitted at the max-line cap, with no terminator seen.
    Overlong,
    /// The final line of the stream, with no terminator (emitted at flush).
    Eof,
}

/// A single framed line — the output type of the [`TransformBuilder::lines`]
/// framer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// The line's bytes, without the terminator.
    pub bytes: Vec<u8>,
    /// How the line was terminated.
    pub ending: LineEnding,
}

impl Line {
    /// The line's bytes as a lossily-decoded string.
    pub fn as_str_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.bytes)
    }

    /// The line's bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// A byte-to-byte pre-stage of a pipeline.
pub trait ByteFilter: Send {
    /// Feed `bytes` through the filter, invoking `out` with any complete output.
    fn push(&mut self, bytes: &[u8], out: &mut dyn FnMut(&[u8]));

    /// Flush any buffered state at end-of-stream.
    fn flush(&mut self, out: &mut dyn FnMut(&[u8]));
}

/// The terminal stage of a pipeline: turns the byte stream into items of type
/// [`Framer::Item`]. This is where a transform's output type is set.
pub trait Framer: Send {
    /// The output type this framer produces.
    type Item;

    /// Feed `bytes` through the framer, invoking `out` with each complete item.
    fn push(&mut self, bytes: &[u8], out: &mut dyn FnMut(Self::Item));

    /// Flush any buffered state at end-of-stream.
    fn flush(&mut self, out: &mut dyn FnMut(Self::Item));
}

/// How to handle ANSI escape sequences in the stream.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Ansi {
    /// Leave every escape sequence in place.
    Keep,
    /// Strip every escape sequence, leaving only printable text.
    StripAll,
    /// Strip motion, erase, and OSC sequences but keep SGR (colour/attribute)
    /// sequences verbatim.
    StripNonAttribute,
}

/// How to handle in-place line rewrites (carriage returns, spinners, progress
/// bars).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Overwrite {
    /// Pass carriage returns through untouched.
    Passthrough,
    /// Resolve `\r` rewrites within a physical line, keeping the final render.
    CollapseLine,
    /// Resolve rewrites that span a few lines (cursor-up + erase). Not yet
    /// implemented; currently behaves like [`Overwrite::CollapseLine`].
    CollapseBlock,
}

/// How to handle invalid UTF-8 in the stream.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Utf8 {
    /// Deliver bytes exactly as produced, valid UTF-8 or not.
    Preserve,
    /// Replace invalid UTF-8 sequences with `U+FFFD`, buffering an incomplete
    /// sequence that straddles a read boundary rather than mangling it.
    Lossy,
}

#[derive(Clone)]
enum ByteStage {
    Ansi(Ansi),
    Overwrite(Overwrite),
    Utf8(Utf8),
}

impl ByteStage {
    fn build(&self) -> Option<Box<dyn ByteFilter>> {
        match self {
            ByteStage::Ansi(Ansi::Keep) => None,
            ByteStage::Ansi(mode) => Some(Box::new(AnsiFilter::new(*mode))),
            ByteStage::Overwrite(Overwrite::Passthrough) => None,
            // CollapseBlock is not implemented yet; fall back to per-line collapse.
            ByteStage::Overwrite(_) => Some(Box::new(CollapseLine::default())),
            ByteStage::Utf8(Utf8::Preserve) => None,
            ByteStage::Utf8(Utf8::Lossy) => Some(Box::new(Utf8Filter::default())),
        }
    }
}

type FramerFactory<T> = Arc<dyn Fn() -> Box<dyn Framer<Item = T>> + Send + Sync>;

/// An ordered, reusable recipe for transforming a captured stream into items of
/// type `T`.
///
/// Build one with [`Transform::raw`] for raw byte runs or [`Transform::builder`]
/// for the staged form.
pub struct Transform<T = Vec<u8>> {
    stages: Vec<ByteStage>,
    framer: FramerFactory<T>,
}

impl<T> Clone for Transform<T> {
    fn clone(&self) -> Self {
        Transform {
            stages: self.stages.clone(),
            framer: Arc::clone(&self.framer),
        }
    }
}

impl Transform {
    /// Start building a staged transform.
    pub fn builder() -> TransformBuilder {
        TransformBuilder::default()
    }

    /// A passthrough transform that delivers raw byte runs unchanged.
    pub fn raw() -> Transform<Vec<u8>> {
        TransformBuilder::default().raw()
    }
}

impl<T> Transform<T> {
    /// Mint a fresh pipeline with its own per-stream state.
    pub(crate) fn build(&self) -> Pipeline<T> {
        Pipeline {
            byte_stages: self.stages.iter().filter_map(|s| s.build()).collect(),
            framer: (self.framer)(),
        }
    }
}

/// Builder for the byte pre-stages of a [`Transform`]. Terminal methods
/// ([`lines`](TransformBuilder::lines), [`raw`](TransformBuilder::raw),
/// [`frame`](TransformBuilder::frame)) choose the framer and fix the output type.
#[derive(Default)]
pub struct TransformBuilder {
    ansi: Option<Ansi>,
    overwrite: Option<Overwrite>,
    utf8: Option<Utf8>,
    max_line: Option<usize>,
}

impl TransformBuilder {
    pub fn ansi(mut self, mode: Ansi) -> Self {
        self.ansi = Some(mode);
        self
    }

    pub fn overwrite(mut self, mode: Overwrite) -> Self {
        self.overwrite = Some(mode);
        self
    }

    pub fn utf8(mut self, mode: Utf8) -> Self {
        self.utf8 = Some(mode);
        self
    }

    /// Cap a single framed line at `max` bytes; anything longer is delivered
    /// in [`LineEnding::Overlong`] pieces. Defaults to 1 MiB.
    pub fn max_line(mut self, max: usize) -> Self {
        self.max_line = Some(max);
        self
    }

    fn stages(self) -> Vec<ByteStage> {
        let mut stages = Vec::new();
        if let Some(mode) = self.ansi {
            stages.push(ByteStage::Ansi(mode));
        }
        if let Some(mode) = self.overwrite {
            stages.push(ByteStage::Overwrite(mode));
        }
        if let Some(mode) = self.utf8 {
            stages.push(ByteStage::Utf8(mode));
        }
        stages
    }

    /// Frame the stream into [`Line`]s on `\n`, stripping a trailing `\r`.
    pub fn lines(self) -> Transform<Line> {
        let max = self.max_line.unwrap_or(DEFAULT_MAX_LINE);
        Transform {
            stages: self.stages(),
            framer: Arc::new(move || Box::new(LineFramer::new(max)) as Box<dyn Framer<Item = Line>>),
        }
    }

    /// Deliver raw byte runs with no framing.
    pub fn raw(self) -> Transform<Vec<u8>> {
        Transform {
            stages: self.stages(),
            framer: Arc::new(|| Box::new(RawFramer) as Box<dyn Framer<Item = Vec<u8>>>),
        }
    }

    /// Terminate with a framer of your own, setting the output type to its
    /// [`Framer::Item`]. `make` constructs a fresh framer for each stream.
    pub fn frame<F>(self, make: impl Fn() -> F + Send + Sync + 'static) -> Transform<F::Item>
    where
        F: Framer + 'static,
    {
        Transform {
            stages: self.stages(),
            framer: Arc::new(move || Box::new(make()) as Box<dyn Framer<Item = F::Item>>),
        }
    }
}

/// A built chain of per-stream byte filters plus a terminal framer.
pub(crate) struct Pipeline<T> {
    byte_stages: Vec<Box<dyn ByteFilter>>,
    framer: Box<dyn Framer<Item = T>>,
}

impl<T> Pipeline<T> {
    /// Push bytes through the whole chain, invoking `out` once per final item.
    pub(crate) fn push(&mut self, bytes: &[u8], out: &mut dyn FnMut(T)) {
        let Self {
            byte_stages,
            framer,
        } = self;
        run_bytes(byte_stages, bytes, &mut |b| framer.push(b, out));
    }

    /// Flush every stage in order at end-of-stream.
    pub(crate) fn flush(&mut self, out: &mut dyn FnMut(T)) {
        let Self {
            byte_stages,
            framer,
        } = self;
        flush_bytes(byte_stages, &mut |b| framer.push(b, out));
        framer.flush(out);
    }
}

// Feed `bytes` into the first byte filter, chaining each of its outputs into the
// rest of the chain. An empty chain passes bytes straight to `sink` (the framer).
fn run_bytes(stages: &mut [Box<dyn ByteFilter>], bytes: &[u8], sink: &mut dyn FnMut(&[u8])) {
    match stages.split_first_mut() {
        None => sink(bytes),
        Some((first, rest)) => first.push(bytes, &mut |b| run_bytes(rest, b, sink)),
    }
}

// Flush the first byte filter (routing its residue through the rest), then flush
// the rest.
fn flush_bytes(stages: &mut [Box<dyn ByteFilter>], sink: &mut dyn FnMut(&[u8])) {
    if let Some((first, rest)) = stages.split_first_mut() {
        first.flush(&mut |b| run_bytes(rest, b, sink));
        flush_bytes(rest, sink);
    }
}

/// The framer for raw output: emits each byte run as an owned `Vec<u8>`.
struct RawFramer;

impl Framer for RawFramer {
    type Item = Vec<u8>;

    fn push(&mut self, bytes: &[u8], out: &mut dyn FnMut(Vec<u8>)) {
        if !bytes.is_empty() {
            out(bytes.to_vec());
        }
    }

    fn flush(&mut self, _out: &mut dyn FnMut(Vec<u8>)) {}
}

/// Frames the stream into [`Line`]s on `\n`, stripping a trailing `\r`, and
/// tagging each with the [`LineEnding`] it saw.
pub struct LineFramer {
    buf: Vec<u8>,
    max: usize,
}

impl LineFramer {
    pub fn new(max: usize) -> Self {
        LineFramer {
            buf: Vec::new(),
            max,
        }
    }
}

impl Framer for LineFramer {
    type Item = Line;

    fn push(&mut self, bytes: &[u8], out: &mut dyn FnMut(Line)) {
        for &b in bytes {
            if b == LF {
                // Strip a trailing CR so CRLF collapses to a bare line, and
                // record which ending we saw.
                let ending = if self.buf.last() == Some(&CR) {
                    self.buf.pop();
                    LineEnding::CrLf
                } else {
                    LineEnding::Lf
                };
                out(Line {
                    bytes: std::mem::take(&mut self.buf),
                    ending,
                });
            } else {
                // Force-emit an over-long line so a stream with no newline
                // cannot grow the buffer without bound.
                if self.buf.len() == self.max {
                    out(Line {
                        bytes: std::mem::take(&mut self.buf),
                        ending: LineEnding::Overlong,
                    });
                }
                self.buf.push(b);
            }
        }
    }

    fn flush(&mut self, out: &mut dyn FnMut(Line)) {
        if !self.buf.is_empty() {
            out(Line {
                bytes: std::mem::take(&mut self.buf),
                ending: LineEnding::Eof,
            });
        }
    }
}

/// Resolves `\r` rewrites within a physical line, keeping the final render.
///
/// `\r` resets the write cursor to column zero; subsequent bytes overwrite in
/// place and extend the line if they run past the previous end (longest write
/// wins on the tail). `\n` commits the rendered line, terminator included, so a
/// following framer can still see the newline.
///
/// The cursor is a byte index, which is fine for ASCII progress bars; a
/// multi-byte overwrite that lands mid-character can tear.
#[derive(Default)]
pub struct CollapseLine {
    line: Vec<u8>,
    col: usize,
}

impl ByteFilter for CollapseLine {
    fn push(&mut self, bytes: &[u8], out: &mut dyn FnMut(&[u8])) {
        for &b in bytes {
            match b {
                LF => {
                    self.line.push(LF);
                    out(&self.line);
                    self.line.clear();
                    self.col = 0;
                }
                CR => self.col = 0,
                _ => {
                    if self.col == self.line.len() {
                        self.line.push(b);
                    } else {
                        self.line[self.col] = b;
                    }
                    self.col += 1;
                }
            }
        }
    }

    fn flush(&mut self, out: &mut dyn FnMut(&[u8])) {
        if !self.line.is_empty() {
            out(&self.line);
            self.line.clear();
            self.col = 0;
        }
    }
}

/// Replaces invalid UTF-8 with `U+FFFD`, buffering an incomplete sequence that
/// straddles a read boundary so a multi-byte character split across two reads is
/// reassembled rather than mangled.
#[derive(Default)]
pub struct Utf8Filter {
    pending: Vec<u8>,
}

impl ByteFilter for Utf8Filter {
    fn push(&mut self, bytes: &[u8], out: &mut dyn FnMut(&[u8])) {
        self.pending.extend_from_slice(bytes);

        let mut sanitized: Vec<u8> = Vec::with_capacity(self.pending.len());
        let mut start = 0;
        loop {
            match std::str::from_utf8(&self.pending[start..]) {
                Ok(valid) => {
                    sanitized.extend_from_slice(valid.as_bytes());
                    start = self.pending.len();
                    break;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    sanitized.extend_from_slice(&self.pending[start..start + valid]);
                    match e.error_len() {
                        Some(len) => {
                            // A complete invalid sequence: replace and continue.
                            sanitized.extend_from_slice(REPLACEMENT);
                            start += valid + len;
                        }
                        None => {
                            // An incomplete tail: keep it for the next read.
                            start += valid;
                            break;
                        }
                    }
                }
            }
        }
        self.pending.drain(..start);

        if !sanitized.is_empty() {
            out(&sanitized);
        }
    }

    fn flush(&mut self, out: &mut dyn FnMut(&[u8])) {
        // Anything left is an incomplete sequence at end-of-stream.
        if !self.pending.is_empty() {
            out(REPLACEMENT);
            self.pending.clear();
        }
    }
}

/// Strips ANSI escape sequences from the stream, driven by [`vt_push_parser`].
///
/// Printable text and C0 controls (newlines, tabs, ...) pass through so later
/// stages still see line structure. Escape sequences are dropped, except that
/// [`Ansi::StripNonAttribute`] re-emits SGR (the `m` sequences that carry colour
/// and attributes) verbatim. The parser is stateful across [`push`](ByteFilter::push)
/// calls, so a sequence split over a read boundary is still recognised.
pub struct AnsiFilter {
    parser: VTPushParser,
    keep_sgr: bool,
}

impl AnsiFilter {
    pub fn new(mode: Ansi) -> Self {
        AnsiFilter {
            parser: VTPushParser::new(),
            keep_sgr: mode == Ansi::StripNonAttribute,
        }
    }
}

// Forward the bytes of one event to `out`, keeping only text, C0 controls, and
// (when asked) SGR sequences.
fn strip_event(event: VTEvent, keep_sgr: bool, out: &mut dyn FnMut(&[u8])) {
    match event {
        VTEvent::Raw(text) => out(text),
        VTEvent::C0(b) => out(&[b]),
        VTEvent::Csi(csi) if keep_sgr && csi.final_byte == b'm' => {
            let event = VTEvent::Csi(csi);
            // Re-encode into a stack buffer, falling back to the heap for an
            // unusually long parameter list.
            let mut buf = [0u8; 64];
            match event.encode(&mut buf) {
                Ok(n) => out(&buf[..n]),
                Err(n) => {
                    let mut big = vec![0u8; n];
                    if let Ok(n) = event.encode(&mut big) {
                        out(&big[..n]);
                    }
                }
            }
        }
        _ => {}
    }
}

impl ByteFilter for AnsiFilter {
    fn push(&mut self, bytes: &[u8], out: &mut dyn FnMut(&[u8])) {
        let keep_sgr = self.keep_sgr;
        self.parser
            .feed_with(bytes, |event: VTEvent| strip_event(event, keep_sgr, out));
    }

    fn flush(&mut self, out: &mut dyn FnMut(&[u8])) {
        let keep_sgr = self.keep_sgr;
        self.parser
            .finish(&mut |event: VTEvent| strip_event(event, keep_sgr, out));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Collect a byte filter's emitted chunks as strings.
    fn byte_out(mut filter: impl ByteFilter, input: &str) -> Vec<String> {
        let mut out = Vec::new();
        filter.push(input.as_bytes(), &mut |b| {
            out.push(String::from_utf8_lossy(b).into_owned())
        });
        filter.flush(&mut |b| out.push(String::from_utf8_lossy(b).into_owned()));
        out
    }

    // Run a whole transform and collect its typed output items.
    fn run_transform<T>(transform: &Transform<T>, input: &str) -> Vec<T> {
        let mut pipeline = transform.build();
        let mut out = Vec::new();
        pipeline.push(input.as_bytes(), &mut |item| out.push(item));
        pipeline.flush(&mut |item| out.push(item));
        out
    }

    #[test]
    fn line_framer_tags_endings() {
        use LineEnding::*;
        let mut framer = LineFramer::new(40);
        let mut out = Vec::new();
        let feed = |framer: &mut LineFramer, s: &str, out: &mut Vec<(String, LineEnding)>| {
            framer.push(s.as_bytes(), &mut |l| {
                out.push((l.as_str_lossy().into_owned(), l.ending))
            });
        };
        feed(&mut framer, "a\nb\r\nc", &mut out);
        framer.flush(&mut |l| out.push((l.as_str_lossy().into_owned(), l.ending)));
        assert_eq!(
            out,
            vec![
                ("a".into(), Lf),
                ("b".into(), CrLf),
                ("c".into(), Eof),
            ]
        );
    }

    #[test]
    fn line_framer_caps_long_lines_as_overlong() {
        let long = "0123456789".repeat(6); // 60 chars, cap is 40
        let t = Transform::builder().lines();
        let out = run_transform(&t, &long);
        // DEFAULT_MAX_LINE is huge, so use a direct framer for the cap test.
        let mut framer = LineFramer::new(40);
        let mut lines = Vec::new();
        framer.push(long.as_bytes(), &mut |l| lines.push(l));
        framer.flush(&mut |l| lines.push(l));
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].ending, LineEnding::Overlong);
        assert_eq!(lines[0].bytes.len(), 40);
        assert_eq!(lines[1].ending, LineEnding::Eof);
        assert_eq!(lines[1].bytes.len(), 20);
        // The transform (with the default 1 MiB cap) sees one Eof line.
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].ending, LineEnding::Eof);
    }

    #[test]
    fn collapse_line_keeps_final_render() {
        assert_eq!(byte_out(CollapseLine::default(), "10%\r20%\r100%\n"), vec!["100%\n"]);
        assert_eq!(byte_out(CollapseLine::default(), "a\rbc\n"), vec!["bc\n"]);
        // Longest write wins on the tail.
        assert_eq!(byte_out(CollapseLine::default(), "abc\rX\n"), vec!["Xbc\n"]);
    }

    #[test]
    fn utf8_lossy_replaces_and_reassembles() {
        // A character split across two pushes is reassembled, not mangled.
        let mut f = Utf8Filter::default();
        let mut out = String::new();
        let smiley = "😀".as_bytes(); // F0 9F 98 80
        f.push(&smiley[..2], &mut |b| out.push_str(&String::from_utf8_lossy(b)));
        f.push(&smiley[2..], &mut |b| out.push_str(&String::from_utf8_lossy(b)));
        f.flush(&mut |b| out.push_str(&String::from_utf8_lossy(b)));
        assert_eq!(out, "😀");
    }

    // Reassemble raw output into one string.
    fn joined(transform: &Transform<Vec<u8>>, input: &str) -> String {
        run_transform(transform, input)
            .into_iter()
            .map(|v| String::from_utf8_lossy(&v).into_owned())
            .collect()
    }

    #[test]
    fn ansi_strip_all() {
        let t = Transform::builder().ansi(Ansi::StripAll).raw();
        assert_eq!(joined(&t, "a\x1b[31mb\x1b[0mc"), "abc");
        assert_eq!(joined(&t, "x\x1b[2Ky"), "xy");
    }

    #[test]
    fn ansi_keeps_attributes() {
        let t = Transform::builder().ansi(Ansi::StripNonAttribute).raw();
        // SGR survives, the erase-line sequence does not.
        assert_eq!(joined(&t, "\x1b[31mred\x1b[0m\x1b[2K"), "\x1b[31mred\x1b[0m");
    }

    #[test]
    fn ansi_strips_escapes_with_intermediates() {
        // Charset designations like `ESC ( B` (part of `tput sgr0` on most
        // terminals) carry an intermediate byte; the final byte must not leak.
        let t = Transform::builder().ansi(Ansi::StripAll).raw();
        assert_eq!(joined(&t, "\x1b(Bhello\x1b)0!"), "hello!");
    }

    #[test]
    fn ansi_strips_osc_and_dcs_bodies() {
        let t = Transform::builder().ansi(Ansi::StripAll).raw();
        // OSC title, terminated by BEL, leaves no body behind.
        assert_eq!(joined(&t, "a\x1b]0;title\x07b"), "ab");
        // DCS body is consumed rather than leaked as text.
        assert_eq!(joined(&t, "a\x1bP1;2q body \x1b\\b"), "ab");
    }

    #[test]
    fn ansi_keeps_newlines() {
        // C0 controls pass through so the line framer downstream still splits.
        let t = Transform::builder().ansi(Ansi::StripAll).raw();
        assert_eq!(joined(&t, "\x1b[31ma\nb\x1b[0m\n"), "a\nb\n");
    }

    #[test]
    fn ansi_handles_a_sequence_split_across_pushes() {
        let mut f = AnsiFilter::new(Ansi::StripAll);
        let mut out = String::new();
        // The CSI sequence straddles the boundary between two feeds.
        f.push(b"a\x1b[3", &mut |b| out.push_str(&String::from_utf8_lossy(b)));
        f.push(b"1mb", &mut |b| out.push_str(&String::from_utf8_lossy(b)));
        f.flush(&mut |b| out.push_str(&String::from_utf8_lossy(b)));
        assert_eq!(out, "ab");
    }

    #[test]
    fn lines_max_line_caps_and_tags_overlong() {
        let t = Transform::builder().max_line(4).lines();
        let out = run_transform(&t, "abcdefghij\n");
        let pieces: Vec<_> = out
            .iter()
            .map(|l| (l.as_str_lossy().into_owned(), l.ending))
            .collect();
        assert_eq!(
            pieces,
            vec![
                ("abcd".into(), LineEnding::Overlong),
                ("efgh".into(), LineEnding::Overlong),
                ("ij".into(), LineEnding::Lf),
            ]
        );
    }

    #[test]
    fn pipeline_applies_fixed_order_and_types_line() {
        // A coloured progress bar: strip motion, collapse the rewrite, frame the
        // line — the terminal framer sets the output type to Line.
        let t = Transform::builder()
            .ansi(Ansi::StripAll)
            .overwrite(Overwrite::CollapseLine)
            .lines();
        let out = run_transform(&t, "\x1b[32m10%\r100%\x1b[0m\n");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].as_str_lossy(), "100%");
        assert_eq!(out[0].ending, LineEnding::Lf);
    }

    #[test]
    fn utf8_stage_runs_before_framing() {
        // Invalid bytes are sanitized, then the line is framed. 0xff is not
        // valid in a &str, so feed bytes through the pipeline directly.
        let t = Transform::builder().utf8(Utf8::Lossy).lines();
        let mut pipeline = t.build();
        let mut out = Vec::new();
        pipeline.push(b"a\xffb\n", &mut |l| out.push(l));
        pipeline.flush(&mut |l| out.push(l));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].as_str_lossy(), "a\u{FFFD}b");
        assert_eq!(out[0].ending, LineEnding::Lf);
    }
}
