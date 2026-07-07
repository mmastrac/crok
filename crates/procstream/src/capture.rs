//! Capturing a child's streams and delivering them as transformed items.
//!
//! [`Capture<T>`] describes how each of the three standard streams is wired, and
//! turns a spawned [`std::process::Child`] into an [`Output<T>`] queue plus the
//! reader threads that feed it. `T` is the transform's output type: `Vec<u8>`
//! for raw byte runs, [`Line`](crate::Line) for framed lines, or any framer's
//! item.
//!
//! The reader threads are the transport for now. They live behind [`Output`],
//! which never names a thread: when the readiness reactor lands it slots in
//! behind this same type and the threads go away without an API change.

use std::io::Read;
use std::process::{ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::transform::Transform;

/// Which standard stream a chunk came from.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// One unit of captured output: the transform's output `item`, tagged with the
/// stream it came from.
#[derive(Clone, Debug)]
pub struct Chunk<T> {
    pub stream: Stream,
    pub item: T,
}

/// One event on the [`Output`] queue.
#[derive(Clone, Debug)]
pub enum Event<T> {
    /// A unit of captured output.
    Chunk(Chunk<T>),
    /// The leader process exited (and has been reaped). Chunks may still
    /// arrive after this while descendants hold the pipes open.
    Exit(ExitStatus),
}

/// The error returned by [`Output::recv_timeout`].
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum RecvTimeout {
    /// No chunk arrived within the timeout.
    Timeout,
    /// Every stream has closed; no more chunks will ever arrive.
    Closed,
}

/// A queue of [`Event`]s from a child: captured chunks plus its exit.
///
/// The queue closes once every capturing stream has hit EOF and the exit has
/// been delivered, so a consumer that drains it to the end has seen everything.
/// This is the seam the reactor slots behind.
pub struct Output<T> {
    rx: Receiver<Event<T>>,
}

impl<T> Output<T> {
    /// Block until the next event, or return `None` once the queue has closed.
    pub fn recv(&self) -> Option<Event<T>> {
        self.rx.recv().ok()
    }

    /// Block for up to `timeout` for the next event.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Event<T>, RecvTimeout> {
        use std::sync::mpsc::RecvTimeoutError;
        self.rx.recv_timeout(timeout).map_err(|e| match e {
            RecvTimeoutError::Timeout => RecvTimeout::Timeout,
            RecvTimeoutError::Disconnected => RecvTimeout::Closed,
        })
    }

    /// Iterate events until the queue closes.
    pub fn iter(&self) -> impl Iterator<Item = Event<T>> + '_ {
        std::iter::from_fn(move || self.rx.recv().ok())
    }
}

// reactor seam: a future `#[cfg(feature = "async")] Output::poll_next(&mut self,
// cx)` lives here, backed by the same channel, so callers can await chunks
// without a runtime dependency.

/// How a single output stream (stdout or stderr) is captured.
pub enum Sink<T> {
    /// Discard the stream.
    Null,
    /// Inherit the parent's stream.
    Inherit,
    /// Pipe the stream through `transform` and deliver it as items.
    Piped(Transform<T>),
}

impl<T> Clone for Sink<T> {
    fn clone(&self) -> Self {
        match self {
            Sink::Null => Sink::Null,
            Sink::Inherit => Sink::Inherit,
            Sink::Piped(transform) => Sink::Piped(transform.clone()),
        }
    }
}

impl<T> Sink<T> {
    fn to_stdio(&self) -> Stdio {
        match self {
            Sink::Null => Stdio::null(),
            Sink::Inherit => Stdio::inherit(),
            Sink::Piped(_) => Stdio::piped(),
        }
    }
}

/// How the child's stdin is wired.
#[derive(Copy, Clone)]
pub enum Stdin {
    Null,
    Inherit,
    /// Keep a handle so the caller can write to the child.
    Piped,
}

impl Stdin {
    fn to_stdio(self) -> Stdio {
        match self {
            Stdin::Null => Stdio::null(),
            Stdin::Inherit => Stdio::inherit(),
            Stdin::Piped => Stdio::piped(),
        }
    }
}

/// What [`Capture::start`] hands back to `spawn_job`: the queue, the reader
/// threads, the piped stdin, and a sender for the exit watcher.
pub(crate) type Started<T> = (
    Output<T>,
    Vec<JoinHandle<()>>,
    Option<ChildStdin>,
    Sender<Event<T>>,
);

/// Describes how all three of a child's standard streams are captured.
pub struct Capture<T> {
    pub stdout: Sink<T>,
    pub stderr: Sink<T>,
    pub stdin: Stdin,
}

impl<T> Clone for Capture<T> {
    fn clone(&self) -> Self {
        Capture {
            stdout: self.stdout.clone(),
            stderr: self.stderr.clone(),
            stdin: self.stdin,
        }
    }
}

impl Capture<crate::Line> {
    /// Frame both stdout and stderr into [`Line`](crate::Line)s with the
    /// default transform, stdin discarded. Shorthand for
    /// `Capture::piped(Transform::builder().lines())`.
    pub fn lines() -> Self {
        Capture::piped(Transform::builder().lines())
    }
}

impl Capture<Vec<u8>> {
    /// Deliver both stdout and stderr as raw byte runs, stdin discarded.
    /// Shorthand for `Capture::piped(Transform::raw())`.
    pub fn raw() -> Self {
        Capture::piped(Transform::raw())
    }
}

impl<T: Send + 'static> Capture<T> {
    /// Pipe both stdout and stderr through `transform`, with stdin discarded.
    pub fn piped(transform: Transform<T>) -> Self {
        Capture {
            stdout: Sink::Piped(transform.clone()),
            stderr: Sink::Piped(transform),
            stdin: Stdin::Null,
        }
    }

    /// Start building a capture with each stream discarded.
    pub fn builder() -> CaptureBuilder<T> {
        CaptureBuilder {
            capture: Capture {
                stdout: Sink::Null,
                stderr: Sink::Null,
                stdin: Stdin::Null,
            },
        }
    }

    /// Apply the stdio configuration to `command` before it is spawned.
    pub(crate) fn apply(&self, command: &mut Command) {
        command.stdout(self.stdout.to_stdio());
        command.stderr(self.stderr.to_stdio());
        command.stdin(self.stdin.to_stdio());
    }

    /// Take the piped handles off a freshly-spawned child and start the reader
    /// threads that feed the returned [`Output`]. The returned sender is for
    /// the exit watcher; the queue closes once it and every reader are done.
    pub(crate) fn start(&self, child: &mut std::process::Child) -> Started<T> {
        let (tx, rx) = channel();
        let mut readers = Vec::new();

        if let Sink::Piped(transform) = &self.stdout {
            let stdout = child.stdout.take().expect("stdout was piped");
            readers.push(pump(stdout, Stream::Stdout, transform, tx.clone()));
        }
        if let Sink::Piped(transform) = &self.stderr {
            let stderr = child.stderr.take().expect("stderr was piped");
            readers.push(pump(stderr, Stream::Stderr, transform, tx.clone()));
        }

        let stdin = child.stdin.take();
        (Output { rx }, readers, stdin, tx)
    }
}

/// Builder for a [`Capture`].
pub struct CaptureBuilder<T> {
    capture: Capture<T>,
}

impl<T> CaptureBuilder<T> {
    pub fn stdout(mut self, transform: Transform<T>) -> Self {
        self.capture.stdout = Sink::Piped(transform);
        self
    }

    pub fn stderr(mut self, transform: Transform<T>) -> Self {
        self.capture.stderr = Sink::Piped(transform);
        self
    }

    pub fn stdout_null(mut self) -> Self {
        self.capture.stdout = Sink::Null;
        self
    }

    pub fn stderr_null(mut self) -> Self {
        self.capture.stderr = Sink::Null;
        self
    }

    pub fn stdin_null(mut self) -> Self {
        self.capture.stdin = Stdin::Null;
        self
    }

    pub fn stdin_piped(mut self) -> Self {
        self.capture.stdin = Stdin::Piped;
        self
    }

    pub fn build(self) -> Capture<T> {
        self.capture
    }
}

// Spawn a reader thread that reads `reader` to EOF, runs each read through a
// fresh pipeline built from `transform`, and sends every resulting item on `tx`.
fn pump<R, T>(
    mut reader: R,
    stream: Stream,
    transform: &Transform<T>,
    tx: Sender<Event<T>>,
) -> JoinHandle<()>
where
    R: Read + Send + 'static,
    T: Send + 'static,
{
    let mut pipeline = transform.build();
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => pipeline.push(&buf[..n], &mut |item| {
                    _ = tx.send(Event::Chunk(Chunk { stream, item }));
                }),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        pipeline.flush(&mut |item| {
            _ = tx.send(Event::Chunk(Chunk { stream, item }));
        });
    })
}
