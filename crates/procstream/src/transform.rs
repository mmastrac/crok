//! Streaming byte transforms applied to a single captured stream.
//!
//! A [`Transform<T>`] records byte pre-stages and a terminal [`Framer`], and
//! builds a fresh [`Pipeline<T>`] per attached stream so one recipe can be
//! reused across spawns.
//!
//! Pre-stages ([`ByteFilter`]s) always run in fixed order regardless of builder
//! call order: `ansi`, then `overwrite`, then `utf8`. The framer sets the output
//! type (`lines()` → [`Line`], default → `Vec<u8>`, or `frame()` for a custom
//! framer).

use std::sync::Arc;

use vt_push_parser::VTPushParser;
use vt_push_parser::event::VTEvent;

const CR: u8 = b'\r';
const LF: u8 = b'\n';

/// Cap on an un-terminated line so a stream without newlines cannot grow forever.
const DEFAULT_MAX_LINE: usize = 1 << 20;

const REPLACEMENT: &[u8] = "\u{FFFD}".as_bytes();

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
    /// Exceeded the max-line cap: a forced piece under [`Overlong::Split`], or
    /// the kept prefix under [`Overlong::Truncate`].
    Overlong,
    Eof,
}

/// Policy when a framed line exceeds the max-line cap.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Overlong {
    /// Keep the first cap bytes as one [`LineEnding::Overlong`] line. Default.
    Truncate,
    /// Cap-sized pieces tagged [`LineEnding::Overlong`], last with the real
    /// terminator. Consumer must stitch.
    Split,
}

/// Framed line from [`TransformBuilder::lines`]: bytes without the terminator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub bytes: Vec<u8>,
    pub ending: LineEnding,
}

impl Line {
    pub fn as_str_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.bytes)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

pub trait ByteFilter: Send {
    fn push(&mut self, bytes: &[u8], out: &mut dyn FnMut(&[u8]));
    fn flush(&mut self, out: &mut dyn FnMut(&[u8]));
}

/// Terminal stage of a pipeline. Its [`Framer::Item`] is the transform output type.
pub trait Framer: Send {
    type Item;
    fn push(&mut self, bytes: &[u8], out: &mut dyn FnMut(Self::Item));
    fn flush(&mut self, out: &mut dyn FnMut(Self::Item));
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Ansi {
    Keep,
    StripAll,
    /// Drop motion/erase/OSC. Keep SGR (colour/attribute) sequences.
    StripNonAttribute,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Overwrite {
    Passthrough,
    /// Resolve `\r` rewrites within a physical line to the final render.
    CollapseLine,
    // TODO: CollapseBlock (cursor-up + erase across a few lines)
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Utf8 {
    Preserve,
    /// Replace invalid sequences with `U+FFFD`. Buffer an incomplete sequence
    /// that straddles a read boundary.
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
            ByteStage::Overwrite(_) => Some(Box::new(CollapseLine::default())),
            ByteStage::Utf8(Utf8::Preserve) => None,
            ByteStage::Utf8(Utf8::Lossy) => Some(Box::new(Utf8Filter::default())),
        }
    }
}

type FramerFactory<T> = Arc<dyn Fn() -> Box<dyn Framer<Item = T>> + Send + Sync>;

/// Reusable recipe for transforming a captured stream into items of type `T`.
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
    pub fn builder() -> TransformBuilder {
        TransformBuilder::default()
    }

    pub fn raw() -> Transform<Vec<u8>> {
        TransformBuilder::default().raw()
    }
}

impl<T> Transform<T> {
    pub(crate) fn build(&self) -> Pipeline<T> {
        Pipeline {
            byte_stages: self.stages.iter().filter_map(|s| s.build()).collect(),
            framer: (self.framer)(),
        }
    }
}

/// Builder for [`Transform`] pre-stages. Terminal methods choose the framer.
#[derive(Default)]
pub struct TransformBuilder {
    ansi: Option<Ansi>,
    overwrite: Option<Overwrite>,
    utf8: Option<Utf8>,
    max_line: Option<usize>,
    overlong: Option<Overlong>,
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

    /// Cap a framed line at `max` bytes (default 1 MiB). See [`overlong`](Self::overlong).
    pub fn max_line(mut self, max: usize) -> Self {
        self.max_line = Some(max);
        self
    }

    /// Policy past the max-line cap. Defaults to [`Overlong::Truncate`].
    pub fn overlong(mut self, policy: Overlong) -> Self {
        self.overlong = Some(policy);
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

    /// Frame into [`Line`]s on `\n`, stripping a trailing `\r`.
    pub fn lines(self) -> Transform<Line> {
        let max = self.max_line.unwrap_or(DEFAULT_MAX_LINE);
        let policy = self.overlong.unwrap_or(Overlong::Truncate);
        Transform {
            stages: self.stages(),
            framer: Arc::new(move || {
                Box::new(LineFramer::new(max, policy)) as Box<dyn Framer<Item = Line>>
            }),
        }
    }

    pub fn raw(self) -> Transform<Vec<u8>> {
        Transform {
            stages: self.stages(),
            framer: Arc::new(|| Box::new(RawFramer) as Box<dyn Framer<Item = Vec<u8>>>),
        }
    }

    /// Custom framer. `make` builds a fresh one per stream.
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

pub(crate) struct Pipeline<T> {
    byte_stages: Vec<Box<dyn ByteFilter>>,
    framer: Box<dyn Framer<Item = T>>,
}

impl<T> Pipeline<T> {
    pub(crate) fn push(&mut self, bytes: &[u8], out: &mut dyn FnMut(T)) {
        let Self {
            byte_stages,
            framer,
        } = self;
        run_bytes(byte_stages, bytes, &mut |b| framer.push(b, out));
    }

    pub(crate) fn flush(&mut self, out: &mut dyn FnMut(T)) {
        let Self {
            byte_stages,
            framer,
        } = self;
        flush_bytes(byte_stages, &mut |b| framer.push(b, out));
        framer.flush(out);
    }
}

fn run_bytes(stages: &mut [Box<dyn ByteFilter>], bytes: &[u8], sink: &mut dyn FnMut(&[u8])) {
    match stages.split_first_mut() {
        None => sink(bytes),
        Some((first, rest)) => first.push(bytes, &mut |b| run_bytes(rest, b, sink)),
    }
}

fn flush_bytes(stages: &mut [Box<dyn ByteFilter>], sink: &mut dyn FnMut(&[u8])) {
    if let Some((first, rest)) = stages.split_first_mut() {
        first.flush(&mut |b| run_bytes(rest, b, sink));
        flush_bytes(rest, sink);
    }
}

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

/// Frames on `\n` (strips trailing `\r`). Caps via [`Overlong`].
pub struct LineFramer {
    buf: Vec<u8>,
    max: usize,
    policy: Overlong,
    truncated: bool,
}

impl LineFramer {
    pub fn new(max: usize, policy: Overlong) -> Self {
        LineFramer {
            buf: Vec::new(),
            max,
            policy,
            truncated: false,
        }
    }
}

impl Framer for LineFramer {
    type Item = Line;

    fn push(&mut self, bytes: &[u8], out: &mut dyn FnMut(Line)) {
        for &b in bytes {
            if b == LF {
                let ending = if self.truncated {
                    self.truncated = false;
                    LineEnding::Overlong
                } else if self.buf.last() == Some(&CR) {
                    self.buf.pop();
                    LineEnding::CrLf
                } else {
                    LineEnding::Lf
                };
                out(Line {
                    bytes: std::mem::take(&mut self.buf),
                    ending,
                });
            } else if self.buf.len() == self.max {
                match self.policy {
                    Overlong::Truncate => self.truncated = true,
                    Overlong::Split => {
                        out(Line {
                            bytes: std::mem::take(&mut self.buf),
                            ending: LineEnding::Overlong,
                        });
                        self.buf.push(b);
                    }
                }
            } else {
                self.buf.push(b);
            }
        }
    }

    fn flush(&mut self, out: &mut dyn FnMut(Line)) {
        // Trailing CR on an unterminated line is a rewrite artifact, not content.
        if self.buf.last() == Some(&CR) {
            self.buf.pop();
        }
        if !self.buf.is_empty() {
            let ending = if self.truncated {
                LineEnding::Overlong
            } else {
                LineEnding::Eof
            };
            self.truncated = false;
            out(Line {
                bytes: std::mem::take(&mut self.buf),
                ending,
            });
        }
    }
}

/// Resolves `\r` rewrites within a physical line to the final render.
///
/// `\r` resets the cursor to column 0. Later bytes overwrite in place (longest
/// write wins on the tail). `\n` commits the line including the terminator.
/// Cursor is a byte index -- fine for ASCII progress bars. Mid-character
/// multi-byte overwrites can tear.
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

/// Replaces invalid UTF-8 with `U+FFFD`. Buffers a sequence split across reads.
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
                            sanitized.extend_from_slice(REPLACEMENT);
                            start += valid + len;
                        }
                        None => {
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
        if !self.pending.is_empty() {
            out(REPLACEMENT);
            self.pending.clear();
        }
    }
}

/// Strips ANSI escapes. C0 controls pass through for line structure.
/// [`Ansi::StripNonAttribute`] keeps SGR (`m`) sequences.
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

fn strip_event(event: VTEvent, keep_sgr: bool, out: &mut dyn FnMut(&[u8])) {
    match event {
        VTEvent::Raw(text) => out(text),
        VTEvent::C0(b) => out(&[b]),
        VTEvent::Csi(csi) if keep_sgr && csi.final_byte == b'm' => {
            let event = VTEvent::Csi(csi);
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

    fn byte_out(mut filter: impl ByteFilter, input: &str) -> Vec<String> {
        let mut out = Vec::new();
        filter.push(input.as_bytes(), &mut |b| {
            out.push(String::from_utf8_lossy(b).into_owned())
        });
        filter.flush(&mut |b| out.push(String::from_utf8_lossy(b).into_owned()));
        out
    }

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
        let mut framer = LineFramer::new(40, super::Overlong::Truncate);
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
        let mut framer = LineFramer::new(40, Overlong::Split);
        let mut lines = Vec::new();
        framer.push(long.as_bytes(), &mut |l| lines.push(l));
        framer.flush(&mut |l| lines.push(l));
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].ending, LineEnding::Overlong);
        assert_eq!(lines[0].bytes.len(), 40);
        assert_eq!(lines[1].ending, LineEnding::Eof);
        assert_eq!(lines[1].bytes.len(), 20);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].ending, LineEnding::Eof);
    }

    #[test]
    fn line_framer_truncates_by_default() {
        let mut framer = LineFramer::new(4, Overlong::Truncate);
        let mut lines = Vec::new();
        framer.push(b"abcdefghij\nok\n", &mut |l| lines.push(l));
        framer.flush(&mut |l| lines.push(l));
        let pieces: Vec<_> = lines
            .iter()
            .map(|l| (l.as_str_lossy().into_owned(), l.ending))
            .collect();
        assert_eq!(
            pieces,
            vec![
                ("abcd".into(), LineEnding::Overlong),
                ("ok".into(), LineEnding::Lf),
            ]
        );
    }

    #[test]
    fn line_framer_strips_bare_trailing_cr_at_eof() {
        let t = Transform::builder().lines();
        let out = run_transform(&t, "done\r");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].as_str_lossy(), "done");
        assert_eq!(out[0].ending, LineEnding::Eof);
        assert!(run_transform(&t, "a\n\r").len() == 1);
    }

    #[test]
    fn collapse_line_keeps_final_render() {
        assert_eq!(byte_out(CollapseLine::default(), "10%\r20%\r100%\n"), vec!["100%\n"]);
        assert_eq!(byte_out(CollapseLine::default(), "a\rbc\n"), vec!["bc\n"]);
        assert_eq!(byte_out(CollapseLine::default(), "abc\rX\n"), vec!["Xbc\n"]);
    }

    #[test]
    fn utf8_lossy_replaces_and_reassembles() {
        let mut f = Utf8Filter::default();
        let mut out = String::new();
        let smiley = "😀".as_bytes(); // F0 9F 98 80
        f.push(&smiley[..2], &mut |b| out.push_str(&String::from_utf8_lossy(b)));
        f.push(&smiley[2..], &mut |b| out.push_str(&String::from_utf8_lossy(b)));
        f.flush(&mut |b| out.push_str(&String::from_utf8_lossy(b)));
        assert_eq!(out, "😀");
    }

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
        assert_eq!(joined(&t, "\x1b[31mred\x1b[0m\x1b[2K"), "\x1b[31mred\x1b[0m");
    }

    #[test]
    fn ansi_strips_escapes_with_intermediates() {
        // `ESC ( B` (charset designator / part of tput sgr0) must not leak the final byte.
        let t = Transform::builder().ansi(Ansi::StripAll).raw();
        assert_eq!(joined(&t, "\x1b(Bhello\x1b)0!"), "hello!");
    }

    #[test]
    fn ansi_strips_osc_and_dcs_bodies() {
        let t = Transform::builder().ansi(Ansi::StripAll).raw();
        assert_eq!(joined(&t, "a\x1b]0;title\x07b"), "ab");
        assert_eq!(joined(&t, "a\x1bP1;2q body \x1b\\b"), "ab");
    }

    #[test]
    fn ansi_keeps_newlines() {
        let t = Transform::builder().ansi(Ansi::StripAll).raw();
        assert_eq!(joined(&t, "\x1b[31ma\nb\x1b[0m\n"), "a\nb\n");
    }

    #[test]
    fn ansi_handles_a_sequence_split_across_pushes() {
        let mut f = AnsiFilter::new(Ansi::StripAll);
        let mut out = String::new();
        f.push(b"a\x1b[3", &mut |b| out.push_str(&String::from_utf8_lossy(b)));
        f.push(b"1mb", &mut |b| out.push_str(&String::from_utf8_lossy(b)));
        f.flush(&mut |b| out.push_str(&String::from_utf8_lossy(b)));
        assert_eq!(out, "ab");
    }

    #[test]
    fn lines_max_line_caps_and_tags_overlong() {
        let t = Transform::builder().max_line(4).overlong(Overlong::Split).lines();
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
        // 0xff is not valid in a &str. Feed bytes directly.
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
