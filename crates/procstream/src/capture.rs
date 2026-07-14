//! Capturing a child's streams and delivering them as transformed items.
//!
//! [`Capture<T>`] describes how each of the three standard streams is wired, and
//! turns a spawned [`std::process::Child`] into an [`Output<T>`] queue. `T` is
//! the transform's output type: `Vec<u8>` for raw byte runs, [`Line`](crate::Line)
//! for framed lines, or any framer's item.
//!
//! On kqueue/epoll platforms the queue is fed by an inline driver that the
//! consumer advances itself; elsewhere a reader thread feeds it. Either way the
//! [`Output`] API is the same.

use std::io::Read;
use std::process::{ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::transform::{Pipeline, Transform};

/// A type-erased handle to whatever advances a child's capture: the inline
/// driver on kqueue/epoll platforms, absent on the thread backend (its threads
/// advance themselves). Erasing the driver type keeps [`Output`] and
/// [`Child`](crate::Child) one shape on every platform, and free of `T`.
pub(crate) trait Advance: Send {
    /// Wait up to `timeout` for readiness, then service whatever is ready.
    fn poll_once(&mut self, timeout: Option<Duration>);
    /// Every stream is closed and the exit delivered: nothing left to do.
    fn is_done(&self) -> bool;
}

/// Shared, type-erased driver handle. `None` on the thread backend.
pub(crate) type DriverHandle = Option<Arc<Mutex<dyn Advance>>>;

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
    /// The leader process exited (and has been reaped).
    ///
    /// This is not end-of-stream, and it is not ordered last. Trailing chunks
    /// can arrive after it. Continue reading until the queue closes.
    Exit(ExitStatus),
}

/// The error returned by [`Output::recv_timeout`].
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum RecvTimeout {
    /// No event arrived within the timeout.
    Timeout,
    /// The queue has closed; no more events will ever arrive.
    Closed,
}

/// A queue of [`Event`]s from a child: captured chunks plus its exit.
///
/// The queue closes once every capturing stream has hit EOF and the exit has
/// been delivered, so a consumer that drains it to the end has seen everything.
///
/// With an inline driver there is no capture thread: each `recv` locks the
/// shared driver and advances it just enough to serve the call. On the thread
/// backend `driver` is `None` and `recv` simply blocks on the queue.
pub struct Output<T> {
    rx: Receiver<Event<T>>,
    driver: DriverHandle,
}

impl<T> Output<T> {
    pub(crate) fn new(rx: Receiver<Event<T>>, driver: DriverHandle) -> Self {
        Output { rx, driver }
    }

    /// Block until the next event, or return `None` once the queue has closed.
    pub fn recv(&self) -> Option<Event<T>> {
        match &self.driver {
            Some(driver) => self.drive(driver, None).ok(),
            None => self.rx.recv().ok(),
        }
    }

    /// Block for up to `timeout` for the next event.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Event<T>, RecvTimeout> {
        match &self.driver {
            Some(driver) => self.drive(driver, Instant::now().checked_add(timeout)),
            None => {
                use std::sync::mpsc::RecvTimeoutError;
                self.rx.recv_timeout(timeout).map_err(|e| match e {
                    RecvTimeoutError::Timeout => RecvTimeout::Timeout,
                    RecvTimeoutError::Disconnected => RecvTimeout::Closed,
                })
            }
        }
    }

    /// Iterate events until the queue closes.
    pub fn iter(&self) -> impl Iterator<Item = Event<T>> + '_ {
        std::iter::from_fn(move || self.recv())
    }

    /// Serve one event, advancing the driver until an event is buffered, the
    /// queue closes, or `deadline` passes. `None` means block indefinitely.
    fn drive(
        &self,
        driver: &Arc<Mutex<dyn Advance>>,
        deadline: Option<Instant>,
    ) -> Result<Event<T>, RecvTimeout> {
        use std::sync::mpsc::TryRecvError;
        loop {
            match self.rx.try_recv() {
                Ok(event) => return Ok(event),
                Err(TryRecvError::Disconnected) => return Err(RecvTimeout::Closed),
                Err(TryRecvError::Empty) => {}
            }
            let mut driver = driver.lock().unwrap();
            if driver.is_done() {
                // Finalize dropped the senders; drain any last buffered event.
                drop(driver);
                return self.rx.try_recv().map_err(|_| RecvTimeout::Closed);
            }
            let timeout = match deadline {
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Err(RecvTimeout::Timeout);
                    }
                    Some(deadline - now)
                }
                None => None,
            };
            driver.poll_once(timeout);
        }
    }
}

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
/// threads, the piped stdin, and a sender for the exit watcher. Dead on the
/// driver backend, which reads without threads.
#[allow(dead_code)]
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
    /// Dead on the driver backend, which reads without threads.
    #[allow(dead_code)]
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
        (Output::new(rx, None), readers, stdin, tx)
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

/// A type-erased consumer of one stream's bytes: run them through a pipeline
/// and deliver the framed items.
#[allow(dead_code)]
pub(crate) trait StreamSink: Send {
    /// Feed a run of bytes just read from the stream.
    fn on_read(&mut self, bytes: &[u8]);
    /// The stream hit EOF: flush any buffered item.
    fn on_eof(&mut self);
}

/// The concrete sink: a pipeline whose items go onto a child's [`Output`] queue.
#[allow(dead_code)]
struct PipeSink<T> {
    pipeline: Pipeline<T>,
    tx: Sender<Event<T>>,
    stream: Stream,
}

impl<T: Send + 'static> StreamSink for PipeSink<T> {
    fn on_read(&mut self, bytes: &[u8]) {
        let PipeSink { pipeline, tx, stream } = self;
        let stream = *stream;
        pipeline.push(bytes, &mut |item| _ = tx.send(Event::Chunk(Chunk { stream, item })));
    }

    fn on_eof(&mut self) {
        let PipeSink { pipeline, tx, stream } = self;
        let stream = *stream;
        pipeline.flush(&mut |item| _ = tx.send(Event::Chunk(Chunk { stream, item })));
    }
}

/// What [`Capture::build_streams`] hands back: the queue receiver, the read fds
/// paired with their sinks, the piped stdin, and a sender for the exit event.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) type StreamParts<T> = (
    Receiver<Event<T>>,
    Vec<(std::os::fd::OwnedFd, Box<dyn StreamSink>)>,
    Option<ChildStdin>,
    Sender<Event<T>>,
);

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl<T: Send + 'static> Capture<T> {
    /// Take the piped read ends off a freshly-spawned child and pair each with
    /// a sink, for the driver to read. No threads are spawned here.
    pub(crate) fn build_streams(&self, child: &mut std::process::Child) -> StreamParts<T> {
        use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};

        let (tx, rx) = channel();
        let mut streams: Vec<(OwnedFd, Box<dyn StreamSink>)> = Vec::new();

        let sink_for = |transform: &Transform<T>, stream: Stream| -> Box<dyn StreamSink> {
            Box::new(PipeSink {
                pipeline: transform.build(),
                tx: tx.clone(),
                stream,
            })
        };

        if let Sink::Piped(transform) = &self.stdout {
            let stdout = child.stdout.take().expect("stdout was piped");
            // into_raw_fd relinquishes std's ownership; we re-own it as an fd
            // the driver will read and close.
            let fd = unsafe { OwnedFd::from_raw_fd(stdout.into_raw_fd()) };
            streams.push((fd, sink_for(transform, Stream::Stdout)));
        }
        if let Sink::Piped(transform) = &self.stderr {
            let stderr = child.stderr.take().expect("stderr was piped");
            let fd = unsafe { OwnedFd::from_raw_fd(stderr.into_raw_fd()) };
            streams.push((fd, sink_for(transform, Stream::Stderr)));
        }

        let stdin = child.stdin.take();
        (rx, streams, stdin, tx)
    }
}

// Spawn a reader thread that reads `reader` to EOF, runs each read through a
// fresh pipeline built from `transform`, and sends every resulting item on `tx`.
// Dead on the driver backend, which reads without threads.
#[allow(dead_code)]
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
