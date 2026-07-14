//! The inline capture driver.
//!
//! One [`Driver`] per child owns a [`Poller`](crate::poller::Poller) over that
//! child's stdout, stderr, and exit. It has no thread of its own: the consumer
//! advances it by calling [`Output::recv`](crate::Output::recv) or
//! [`Child::wait`](crate::Child::wait), each of which locks the shared driver
//! and calls [`poll_once`](Driver::poll_once). One poll reads whatever is ready,
//! runs it through the stream sinks (which push onto the child's queue), and
//! reaps the child when it exits.
//!
//! `Driver` is non-generic: the sinks are type-erased, so it stays free of the
//! output type `T`, which lets [`Child`](crate::Child) stay non-generic too.

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;

use crate::capture::{Advance, StreamSink};
use crate::job::ExitState;
use crate::poller::{Poller, Ready};

// Token for the exit source. Stream tokens are their index (0, 1).
const EXIT_TOKEN: u64 = u64::MAX;

// The longest a single poll blocks. A finite but enormous deadline is clamped
// to this and the caller re-polls, which keeps huge timeouts from reaching the
// kernel as an out-of-range timespec.
const MAX_WAIT: Duration = Duration::from_secs(3600);

struct StreamState {
    fd: OwnedFd,
    sink: Box<dyn StreamSink>,
    closed: bool,
}

/// The per-child capture state, driven synchronously by the consumer.
pub(crate) struct Driver {
    poller: Poller,
    child: std::process::Child,
    streams: Vec<StreamState>,
    exit_state: Arc<ExitState>,
    /// Puts the exit onto the child's queue. Dropped at finalize so the queue
    /// closes.
    on_exit: Option<Box<dyn FnMut(ExitStatus) + Send>>,
    open_streams: usize,
    exit_reaped: bool,
    /// Every stream is closed and the exit is delivered: nothing left to do.
    done: bool,
}

impl Driver {
    /// Build a driver for a freshly-spawned child and register its streams and
    /// exit with a fresh poller.
    pub(crate) fn new(
        mut child: std::process::Child,
        streams: Vec<(OwnedFd, Box<dyn StreamSink>)>,
        exit_state: Arc<ExitState>,
        on_exit: Box<dyn FnMut(ExitStatus) + Send>,
    ) -> io::Result<Driver> {
        let poller = match Poller::new() {
            Ok(poller) => poller,
            // Nothing owns the child yet, so reap the leader here rather than
            // leak it as a zombie.
            Err(e) => return Err(kill_and_reap(&mut child, e)),
        };
        let pid = child.id() as i32;
        let mut driver = Driver {
            poller,
            child,
            streams: Vec::new(),
            exit_state,
            on_exit: Some(on_exit),
            open_streams: 0,
            exit_reaped: false,
            done: false,
        };

        for (idx, (fd, sink)) in streams.into_iter().enumerate() {
            set_nonblocking(fd.as_fd());
            // If registration fails the stream is simply never read; flush it so
            // its EOF still helps close the queue.
            if driver.poller.add_read(fd.as_fd(), idx as u64).is_ok() {
                driver.streams.push(StreamState {
                    fd,
                    sink,
                    closed: false,
                });
                driver.open_streams += 1;
            } else {
                let mut sink = sink;
                sink.on_eof();
            }
        }

        // Register exit readiness, then probe once: a child that already exited
        // may not deliver an exit event.
        match driver.poller.add_process(pid, EXIT_TOKEN) {
            Ok(true) => {
                if matches!(driver.child.try_wait(), Ok(Some(_)) | Err(_)) {
                    driver.reap();
                }
            }
            // The process is already gone; reap it directly.
            Ok(false) => driver.reap(),
            // A real failure to register the exit source is terminal for
            // capture: reap the leader and surface it rather than reap()'s
            // blocking wait on a child nobody is draining.
            Err(e) => return Err(kill_and_reap(&mut driver.child, e)),
        }

        Ok(driver)
    }


    // A terminal poller failure: publish it as the exit and close the queue so
    // no consumer waits forever.
    fn abort(&mut self, err: io::Error) {
        if !self.exit_reaped {
            self.exit_state.set(Err(err));
            self.exit_reaped = true;
        }
        self.open_streams = 0;
        self.streams.clear();
        self.on_exit = None;
        self.done = true;
    }

    fn on_readable(&mut self, idx: usize) {
        let Some(st) = self.streams.get_mut(idx) else {
            return;
        };
        if st.closed {
            return;
        }
        let mut buf = [0u8; 16 * 1024];
        let mut eof = false;
        loop {
            match read_fd(st.fd.as_fd(), &mut buf) {
                ReadResult::Data(n) => st.sink.on_read(&buf[..n]),
                ReadResult::WouldBlock => break,
                ReadResult::Eof => {
                    st.sink.on_eof();
                    eof = true;
                    break;
                }
            }
        }
        if eof {
            self.close_stream(idx);
        }
    }

    fn close_stream(&mut self, idx: usize) {
        let st = &mut self.streams[idx];
        if st.closed {
            return;
        }
        st.closed = true;
        self.open_streams -= 1;
        // Deregister before the fd is dropped at finalize.
        _ = self.poller.remove_read(st.fd.as_fd());
        self.finalize_if_done();
    }

    // Reap the child and publish its exit, once.
    fn reap(&mut self) {
        if self.exit_reaped {
            return;
        }
        // The exit source fired, so the reap does not block. Fall back to a
        // blocking wait only if try_wait somehow races ahead of the reap.
        let result = match self.child.try_wait() {
            Ok(Some(status)) => Ok(status),
            Ok(None) => self.child.wait(),
            Err(e) => Err(e),
        };
        self.exit_reaped = true;
        let status = result.as_ref().ok().copied();
        // Store before notifying, so a consumer that sees the exit event can
        // immediately observe the status through try_wait.
        self.exit_state.set(result);
        if let Some(status) = status
            && let Some(on_exit) = self.on_exit.as_mut()
        {
            on_exit(status);
        }
        self.poller.remove_process(EXIT_TOKEN);
        self.finalize_if_done();
    }

    // Once every stream is closed and the exit is delivered, drop the sinks and
    // the exit notifier. That drops the last queue senders, so the queue closes.
    fn finalize_if_done(&mut self) {
        if self.open_streams == 0 && self.exit_reaped {
            self.streams.clear();
            self.on_exit = None;
            self.done = true;
        }
    }
}

impl Advance for Driver {
    fn is_done(&self) -> bool {
        self.done
    }

    /// Advance once: wait up to `timeout` for readiness, then service whatever
    /// is ready. A `Some(Duration::ZERO)` timeout polls without blocking.
    ///
    /// Infallible: a poller failure is terminal, so it is folded into the exit
    /// (surfaced through `wait`/`try_wait`) and closes the queue.
    fn poll_once(&mut self, timeout: Option<Duration>) {
        if self.done {
            return;
        }
        // Clamp the wait: a near-infinite but finite deadline (a background
        // command's "no timeout") would otherwise reach the kernel as an absurd
        // timespec and be rejected. The caller loops, so waking early is fine.
        let timeout = timeout.map(|d| d.min(MAX_WAIT));
        let mut ready = Vec::new();
        match self.poller.wait(&mut ready, timeout) {
            Ok(()) => {}
            // A signal interrupted the wait; the caller loops and retries.
            Err(e) if e.kind() == io::ErrorKind::Interrupted => return,
            Err(e) => return self.abort(e),
        }
        for ready in ready {
            match ready {
                Ready::Readable(token) => self.on_readable(token as usize),
                Ready::Exited(_) => self.reap(),
            }
        }
    }
}

// Teardown when capture setup fails after the child spawned: kill the leader
// and reap it so it does not linger as a zombie, then return the original
// error. Descendants are the caller's `Job` to sweep.
fn kill_and_reap(child: &mut std::process::Child, err: io::Error) -> io::Error {
    _ = child.kill();
    _ = child.wait();
    err
}

enum ReadResult {
    Data(usize),
    WouldBlock,
    Eof,
}

fn read_fd(fd: BorrowedFd, buf: &mut [u8]) -> ReadResult {
    use rustix::io::{Errno, read};
    loop {
        match read(fd, &mut *buf) {
            Ok(0) => return ReadResult::Eof,
            Ok(n) => return ReadResult::Data(n),
            Err(Errno::INTR) => continue,
            Err(Errno::AGAIN) => return ReadResult::WouldBlock,
            Err(_) => return ReadResult::Eof,
        }
    }
}

fn set_nonblocking(fd: BorrowedFd) {
    _ = rustix::io::ioctl_fionbio(fd, true);
}
