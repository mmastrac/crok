//! Platform readiness for the driver: one wait point over a child's fds. macOS
//! rides kqueue, Linux rides epoll.
//!
//! The poller only reports readiness. Reads, transforms, and reaping happen in
//! the driver above it, so this layer stays non-generic and platform-local.
//!
//! There is no wakeup source: the driver is advanced only by its owner's own
//! `recv`/`wait` calls, and killing the child from another thread wakes a
//! blocking wait through the exit event itself.

/// One thing the poller observed during a wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ready {
    /// A registered read fd has data or has hit EOF. The driver reads it.
    Readable(u64),
    /// A registered process has exited. The driver reaps it.
    Exited(u64),
}

#[cfg(target_os = "macos")]
mod imp {
    use super::Ready;
    use std::io;
    use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
    use std::time::Duration;

    use rustix::buffer::spare_capacity;
    use rustix::event::kqueue::{
        Event, EventFilter, EventFlags, ProcessEvents, kevent, kqueue,
    };
    use rustix::process::Pid;

    fn token_ptr(token: u64) -> *mut core::ffi::c_void {
        token as usize as *mut core::ffi::c_void
    }

    // Apply a changelist without waiting: a zero-capacity eventlist makes
    // `kevent` return as soon as the changes are registered.
    fn apply(kq: &OwnedFd, changes: &[Event]) -> io::Result<()> {
        let mut empty: Vec<Event> = Vec::new();
        // SAFETY: every fd named in `changes` is owned by the driver and is
        // removed from the kqueue before it is closed.
        unsafe { kevent(kq, changes, &mut empty, Some(Duration::ZERO))? };
        Ok(())
    }

    pub(crate) struct Poller {
        kq: OwnedFd,
    }

    impl Poller {
        pub(crate) fn new() -> io::Result<Self> {
            Ok(Poller { kq: kqueue()? })
        }

        pub(crate) fn add_read(&mut self, fd: BorrowedFd, token: u64) -> io::Result<()> {
            let ev = Event::new(
                EventFilter::Read(fd.as_raw_fd()),
                EventFlags::ADD,
                token_ptr(token),
            );
            apply(&self.kq, &[ev])
        }

        pub(crate) fn remove_read(&mut self, fd: BorrowedFd) -> io::Result<()> {
            let ev = Event::new(
                EventFilter::Read(fd.as_raw_fd()),
                EventFlags::DELETE,
                std::ptr::null_mut(),
            );
            match apply(&self.kq, &[ev]) {
                Err(e) if e.raw_os_error() == Some(libc::ENOENT) => Ok(()),
                other => other,
            }
        }

        pub(crate) fn add_process(&mut self, pid: i32, token: u64) -> io::Result<bool> {
            let Some(pid) = Pid::from_raw(pid) else {
                return Ok(false);
            };
            let ev = Event::new(
                EventFilter::Proc {
                    pid,
                    flags: ProcessEvents::EXIT,
                },
                EventFlags::ADD | EventFlags::CLEAR,
                token_ptr(token),
            );
            match apply(&self.kq, &[ev]) {
                Ok(()) => Ok(true),
                // The process is already gone; the driver reaps it directly.
                Err(e) if e.raw_os_error() == Some(libc::ESRCH) => Ok(false),
                Err(e) => Err(e),
            }
        }

        pub(crate) fn remove_process(&mut self, _token: u64) {
            // EVFILT_PROC deletes itself when the process exits, so once we have
            // seen the exit there is nothing left to remove.
        }

        pub(crate) fn wait(
            &mut self,
            out: &mut Vec<Ready>,
            timeout: Option<Duration>,
        ) -> io::Result<()> {
            let mut evs: Vec<Event> = Vec::with_capacity(64);
            // SAFETY: an empty changelist names no fds.
            unsafe { kevent(&self.kq, &[], spare_capacity(&mut evs), timeout)? };
            for ev in &evs {
                let token = ev.udata() as usize as u64;
                match ev.filter() {
                    EventFilter::Proc { .. } => out.push(Ready::Exited(token)),
                    _ => out.push(Ready::Readable(token)),
                }
            }
            Ok(())
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::Ready;
    use std::collections::HashMap;
    use std::io;
    use std::os::fd::{BorrowedFd, OwnedFd};
    use std::time::Duration;

    use rustix::buffer::spare_capacity;
    use rustix::event::epoll::{self, CreateFlags, EventData, EventFlags};
    use rustix::event::Timespec;
    use rustix::process::{Pid, PidfdFlags, pidfd_open};

    fn to_timespec(d: Duration) -> Timespec {
        Timespec {
            tv_sec: d.as_secs() as _,
            tv_nsec: d.subsec_nanos() as _,
        }
    }

    pub(crate) struct Poller {
        epfd: OwnedFd,
        // Tokens whose readiness means a process exit, mapped to the pidfd we
        // keep alive for the registration.
        pidfds: HashMap<u64, OwnedFd>,
    }

    impl Poller {
        pub(crate) fn new() -> io::Result<Self> {
            Ok(Poller {
                epfd: epoll::create(CreateFlags::CLOEXEC)?,
                pidfds: HashMap::new(),
            })
        }

        pub(crate) fn add_read(&mut self, fd: BorrowedFd, token: u64) -> io::Result<()> {
            epoll::add(&self.epfd, fd, EventData::new_u64(token), EventFlags::IN)?;
            Ok(())
        }

        pub(crate) fn remove_read(&mut self, fd: BorrowedFd) -> io::Result<()> {
            match epoll::delete(&self.epfd, fd) {
                Err(e) if e.raw_os_error() == libc::ENOENT => Ok(()),
                other => other.map_err(Into::into),
            }
        }

        pub(crate) fn add_process(&mut self, pid: i32, token: u64) -> io::Result<bool> {
            let Some(pid) = Pid::from_raw(pid) else {
                return Ok(false);
            };
            let pidfd = match pidfd_open(pid, PidfdFlags::empty()) {
                Ok(fd) => fd,
                // The process is already gone; the driver reaps it directly.
                Err(e) if e.raw_os_error() == libc::ESRCH => return Ok(false),
                Err(e) => return Err(e.into()),
            };
            epoll::add(&self.epfd, &pidfd, EventData::new_u64(token), EventFlags::IN)?;
            self.pidfds.insert(token, pidfd);
            Ok(true)
        }

        pub(crate) fn remove_process(&mut self, token: u64) {
            if let Some(fd) = self.pidfds.remove(&token) {
                _ = epoll::delete(&self.epfd, &fd);
            }
        }

        pub(crate) fn wait(
            &mut self,
            out: &mut Vec<Ready>,
            timeout: Option<Duration>,
        ) -> io::Result<()> {
            let ts = timeout.map(to_timespec);
            let mut evs: Vec<epoll::Event> = Vec::with_capacity(64);
            epoll::wait(&self.epfd, spare_capacity(&mut evs), ts.as_ref())?;
            for ev in &evs {
                let token = ev.data.u64();
                if self.pidfds.contains_key(&token) {
                    out.push(Ready::Exited(token));
                } else {
                    out.push(Ready::Readable(token));
                }
            }
            Ok(())
        }
    }
}

pub(crate) use imp::Poller;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::AsFd;
    use std::time::Duration;

    #[test]
    fn read_fd_reports_readable() {
        let (mut rd, mut wr) = os_pipe::pipe_pair();
        let mut poller = Poller::new().unwrap();
        poller.add_read(rd.as_fd(), 7).unwrap();

        wr.write_all(b"hi").unwrap();
        let mut out = Vec::new();
        poller.wait(&mut out, Some(Duration::from_secs(5))).unwrap();
        assert!(out.contains(&Ready::Readable(7)));

        // Drain, then the write end closing shows up as another readable that
        // reads zero bytes (EOF).
        use std::io::Read;
        let mut buf = [0u8; 8];
        let _ = rd.read(&mut buf);
        drop(wr);
        out.clear();
        poller.wait(&mut out, Some(Duration::from_secs(5))).unwrap();
        assert!(out.contains(&Ready::Readable(7)));
        poller.remove_read(rd.as_fd()).unwrap();
    }

    #[test]
    fn process_exit_reports_exited() {
        // The brief sleep keeps the child alive across registration, so it must
        // take; a bare `exit 0` could be gone first, and a process already gone
        // registers as false (the driver reaps it directly instead).
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 0.2")
            .spawn()
            .unwrap();
        let mut poller = Poller::new().unwrap();
        assert!(poller.add_process(child.id() as i32, 3).unwrap());

        let mut out = Vec::new();
        poller
            .wait(&mut out, Some(Duration::from_secs(5)))
            .unwrap();
        assert!(out.contains(&Ready::Exited(3)));
        child.wait().unwrap();
    }

    // A minimal pipe pair for the tests, avoiding an extra dependency.
    mod os_pipe {
        use std::fs::File;
        use std::os::fd::FromRawFd;

        pub(super) fn pipe_pair() -> (File, File) {
            let mut fds = [0i32; 2];
            let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
            assert_eq!(rc, 0, "pipe() failed");
            unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) }
        }
    }
}
