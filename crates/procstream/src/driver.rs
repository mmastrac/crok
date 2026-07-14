//! The inline capture driver.
//!
//! One [`Driver`] per child owns a [`Poller`](crate::poller::Poller) over that
//! child's stdout, stderr, and exit. It has no thread of its own: the consumer
//! advances it via [`Output::recv`](crate::Output::recv) or
//! [`Child::wait`](crate::Child::wait), which lock the shared driver and call
//! [`poll_once`](Driver::poll_once).
//!
//! Sinks are type-erased so `Driver` (and [`Child`](crate::Child)) stay free of `T`.

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;

use crate::capture::{Advance, StreamSink};
use crate::job::ExitState;
use crate::poller::{Poller, Ready};

// Stream tokens are their index (0, 1).
const EXIT_TOKEN: u64 = u64::MAX;

// Cap a single poll so a near-infinite deadline does not become an out-of-range
// kernel timespec. The caller re-polls.
const MAX_WAIT: Duration = Duration::from_secs(3600);

struct StreamState {
    fd: OwnedFd,
    sink: Box<dyn StreamSink>,
    closed: bool,
}

pub(crate) struct Driver {
    poller: Poller,
    child: std::process::Child,
    streams: Vec<StreamState>,
    exit_state: Arc<ExitState>,
    /// Dropped at finalize so the queue closes.
    on_exit: Option<Box<dyn FnMut(ExitStatus) + Send>>,
    open_streams: usize,
    exit_reaped: bool,
    done: bool,
}

impl Driver {
    pub(crate) fn new(
        mut child: std::process::Child,
        streams: Vec<(OwnedFd, Box<dyn StreamSink>)>,
        exit_state: Arc<ExitState>,
        on_exit: Box<dyn FnMut(ExitStatus) + Send>,
    ) -> io::Result<Driver> {
        let poller = match Poller::new() {
            Ok(poller) => poller,
            // Nothing owns the child yet. Reap rather than leak a zombie.
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
            // On registration failure the stream is never read. Flush so its
            // EOF still helps close the queue.
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

        // A child that already exited may not deliver an exit event.
        match driver.poller.add_process(pid, EXIT_TOKEN) {
            Ok(true) => {
                if matches!(driver.child.try_wait(), Ok(Some(_)) | Err(_)) {
                    driver.reap();
                }
            }
            Ok(false) => driver.reap(),
            // Don't fall into reap()'s blocking wait on a child nobody drains.
            Err(e) => return Err(kill_and_reap(&mut driver.child, e)),
        }

        Ok(driver)
    }

    // Publish the error as the exit and close the queue.
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
        // Before the fd is dropped at finalize.
        _ = self.poller.remove_read(st.fd.as_fd());
        self.finalize_if_done();
    }

    fn reap(&mut self) {
        if self.exit_reaped {
            return;
        }
        // Exit source fired, so try_wait should not block. Wait is the race fallback.
        let result = match self.child.try_wait() {
            Ok(Some(status)) => Ok(status),
            Ok(None) => self.child.wait(),
            Err(e) => Err(e),
        };
        self.exit_reaped = true;
        let status = result.as_ref().ok().copied();
        // Store before notifying so try_wait sees the status with the event.
        self.exit_state.set(result);
        if let Some(status) = status
            && let Some(on_exit) = self.on_exit.as_mut()
        {
            on_exit(status);
        }
        self.poller.remove_process(EXIT_TOKEN);
        self.finalize_if_done();
    }

    // Dropping sinks and on_exit drops the last queue senders.
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

    /// `Some(Duration::ZERO)` polls without blocking. Poller failures are folded
    /// into the exit and close the queue.
    fn poll_once(&mut self, timeout: Option<Duration>) {
        if self.done {
            return;
        }
        let timeout = timeout.map(|d| d.min(MAX_WAIT));
        let mut ready = Vec::new();
        match self.poller.wait(&mut ready, timeout) {
            Ok(()) => {}
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

/// Kill and reap the leader after a failed capture setup so it does not linger
/// as a zombie. Descendants are the caller's [`Job`](crate::Job) to sweep.
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
