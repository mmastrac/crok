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
use std::process::{ChildStdin, Command, Stdio};
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

/// The error returned by [`Output::recv_timeout`].
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum RecvTimeout {
    /// No chunk arrived within the timeout.
    Timeout,
    /// Every stream has closed; no more chunks will ever arrive.
    Closed,
}

/// A queue of captured [`Chunk`]s from a child's streams.
///
/// The queue drains once every capturing stream has closed (the child exited or
/// closed its pipes). This is the seam the reactor slots behind.
pub struct Output<T> {
    rx: Receiver<Chunk<T>>,
}

impl<T> Output<T> {
    /// Block until the next chunk, or return `None` once all streams have closed.
    pub fn recv(&self) -> Option<Chunk<T>> {
        self.rx.recv().ok()
    }

    /// Block for up to `timeout` for the next chunk.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Chunk<T>, RecvTimeout> {
        use std::sync::mpsc::RecvTimeoutError;
        self.rx.recv_timeout(timeout).map_err(|e| match e {
            RecvTimeoutError::Timeout => RecvTimeout::Timeout,
            RecvTimeoutError::Disconnected => RecvTimeout::Closed,
        })
    }

    /// Iterate chunks until every stream has closed.
    pub fn iter(&self) -> impl Iterator<Item = Chunk<T>> + '_ {
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
    /// threads that feed the returned [`Output`].
    pub(crate) fn start(
        &self,
        child: &mut std::process::Child,
    ) -> (Output<T>, Vec<JoinHandle<()>>, Option<ChildStdin>) {
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
        // Drop our own sender so the queue closes once the reader threads (which
        // hold the only remaining senders) finish.
        drop(tx);

        let stdin = child.stdin.take();
        (Output { rx }, readers, stdin)
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
    tx: Sender<Chunk<T>>,
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
                    _ = tx.send(Chunk { stream, item });
                }),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        pipeline.flush(&mut |item| {
            _ = tx.send(Chunk { stream, item });
        });
    })
}
