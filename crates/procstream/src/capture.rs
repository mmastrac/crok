//! Capturing a child's streams and delivering them as transformed items.
//!
//! [`Capture<T>`] describes how each of the three standard streams is wired, and
//! turns a spawned [`std::process::Child`] into an [`Output<T>`] queue. `T` is
//! the transform's output type: `Vec<u8>` for raw byte runs, [`Line`](crate::Line)
//! for framed lines, or any framer's item.
//!
//! On kqueue/epoll platforms an inline driver feeds the queue. Elsewhere a
//! reader thread does. The [`Output`] API is the same either way.

use std::io::Read;
use std::process::{ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::transform::{Pipeline, Transform};

/// Type-erased capture advancer. Present on kqueue/epoll, `None` on the thread
/// backend. Keeps [`Output`] and [`Child`](crate::Child) one shape, free of `T`.
pub(crate) trait Advance: Send {
    fn poll_once(&mut self, timeout: Option<Duration>);
    fn is_done(&self) -> bool;
}

pub(crate) type DriverHandle = Option<Arc<Mutex<dyn Advance>>>;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// One unit of captured output, tagged with the stream it came from.
#[derive(Clone, Debug)]
pub struct Chunk<T> {
    pub stream: Stream,
    pub item: T,
}

#[derive(Clone, Debug)]
pub enum Event<T> {
    Chunk(Chunk<T>),
    /// Leader exited (and has been reaped). Not end-of-stream and not ordered
    /// last: trailing chunks can still arrive. Drain until the queue closes.
    Exit(ExitStatus),
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum RecvTimeout {
    Timeout,
    Closed,
}

/// Queue of [`Event`]s from a child. Closes once every capturing stream has hit
/// EOF and the exit has been delivered.
///
/// With an inline driver, each `recv` advances the shared driver just enough to
/// serve the call. On the thread backend `driver` is `None` and `recv` blocks
/// on the queue.
pub struct Output<T> {
    rx: Receiver<Event<T>>,
    driver: DriverHandle,
}

impl<T> Output<T> {
    pub(crate) fn new(rx: Receiver<Event<T>>, driver: DriverHandle) -> Self {
        Output { rx, driver }
    }

    /// Block until the next event, or `None` once the queue has closed.
    pub fn recv(&self) -> Option<Event<T>> {
        match &self.driver {
            Some(driver) => self.drive(driver, None).ok(),
            None => self.rx.recv().ok(),
        }
    }

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

    pub fn iter(&self) -> impl Iterator<Item = Event<T>> + '_ {
        std::iter::from_fn(move || self.recv())
    }

    /// Advance the driver until an event is buffered, the queue closes, or
    /// `deadline` passes. `None` means block indefinitely.
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
                // Finalize dropped the senders. Drain any last buffered event.
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

pub enum Sink<T> {
    Null,
    Inherit,
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

#[derive(Copy, Clone)]
pub enum Stdin {
    Null,
    Inherit,
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

/// What [`Capture::start`] hands back. Dead on the driver backend.
#[allow(dead_code)]
pub(crate) type Started<T> = (
    Output<T>,
    Vec<JoinHandle<()>>,
    Option<ChildStdin>,
    Sender<Event<T>>,
);

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
    /// Shorthand for `Capture::piped(Transform::builder().lines())`.
    pub fn lines() -> Self {
        Capture::piped(Transform::builder().lines())
    }
}

impl Capture<Vec<u8>> {
    /// Shorthand for `Capture::piped(Transform::raw())`.
    pub fn raw() -> Self {
        Capture::piped(Transform::raw())
    }
}

impl<T: Send + 'static> Capture<T> {
    pub fn piped(transform: Transform<T>) -> Self {
        Capture {
            stdout: Sink::Piped(transform.clone()),
            stderr: Sink::Piped(transform),
            stdin: Stdin::Null,
        }
    }

    pub fn builder() -> CaptureBuilder<T> {
        CaptureBuilder {
            capture: Capture {
                stdout: Sink::Null,
                stderr: Sink::Null,
                stdin: Stdin::Null,
            },
        }
    }

    pub(crate) fn apply(&self, command: &mut Command) {
        command.stdout(self.stdout.to_stdio());
        command.stderr(self.stderr.to_stdio());
        command.stdin(self.stdin.to_stdio());
    }

    /// Start reader threads that feed the returned [`Output`]. The sender is
    /// for the exit watcher. The queue closes once it and every reader are done.
    #[allow(dead_code)] // Dead on the driver backend.
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

#[allow(dead_code)] // Only used by the driver backend.
pub(crate) trait StreamSink: Send {
    fn on_read(&mut self, bytes: &[u8]);
    fn on_eof(&mut self);
}

#[allow(dead_code)] // Only used by the driver backend.
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) type StreamParts<T> = (
    Receiver<Event<T>>,
    Vec<(std::os::fd::OwnedFd, Box<dyn StreamSink>)>,
    Option<ChildStdin>,
    Sender<Event<T>>,
);

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl<T: Send + 'static> Capture<T> {
    /// Pair each piped read end with a sink for the driver. No threads.
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
            // Relinquish std's ownership. The driver reads and closes the fd.
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

#[allow(dead_code)] // Dead on the driver backend.
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
