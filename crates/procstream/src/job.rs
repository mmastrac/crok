//! Spawning a process into an isolated job and killing the whole tree.
//!
//! [`CommandJobExt`] extends [`std::process::Command`] with `spawn_job`, which
//! places the child in a fresh isolation unit (a process group on Unix, a Job
//! object on Windows) so that [`Job::signal`] and [`Child::shutdown`] act
//! on the whole tree rather than just the immediate child.
//!
//! The platform bodies are lifted from clitest's `ScriptKillReceiver::run_cmd`.

use std::io;
use std::process::{Command, ExitStatus};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::capture::{Capture, Output};

/// A signal to deliver to a whole process tree.
///
/// On Unix these map to `SIGINT`/`SIGTERM`/`SIGKILL`. On Windows there is no
/// portable graceful signal for a job tree, so only [`Signal::Kill`] does
/// anything (it terminates the Job); the others are a no-op.
#[derive(Copy, Clone, Debug)]
pub enum Signal {
    /// A polite interrupt (`SIGINT` on Unix).
    Interrupt,
    /// A request to terminate (`SIGTERM` on Unix).
    Terminate,
    /// An unconditional kill (`SIGKILL` on Unix; terminates the Job on Windows).
    Kill,
}

/// Extends [`Command`] with process-tree isolation and capture.
pub trait CommandJobExt {
    /// Spawn into a fresh isolated job (a new process group / Job object) with
    /// the given capture. The child's output type `T` comes from the capture's
    /// transform.
    ///
    /// `spawn_job` owns the stdio (it sets it from `capture`) and, on Unix, the
    /// process group, and on Windows the creation flags. If you need those knobs
    /// yourself, they collide with the isolation.
    fn spawn_job<T: Send + 'static>(&mut self, capture: Capture<T>) -> io::Result<Child<T>>;
}

impl CommandJobExt for Command {
    fn spawn_job<T: Send + 'static>(&mut self, capture: Capture<T>) -> io::Result<Child<T>> {
        capture.apply(self);

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            self.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_SUSPENDED: u32 = 0x00000004;
            self.creation_flags(CREATE_SUSPENDED);
        }

        let mut child = self.spawn().map_err(|e| {
            io::Error::new(e.kind(), format!("failed to spawn command {self:?}: {e}"))
        })?;

        let job = Job::adopt(&mut child)?;
        let (output, readers, stdin) = capture.start(&mut child);

        Ok(Child {
            proc: child,
            job,
            output: Some(output),
            readers,
            stdin,
        })
    }
}

/// A cheaply-cloneable handle to an isolated process tree. Every clone refers
/// to the same tree, so a clone can signal it from another thread.
#[derive(Clone)]
pub struct Job {
    inner: Arc<JobInner>,
}

#[cfg(unix)]
struct JobInner {
    /// The process group id, which equals the leader child's pid.
    pgid: i32,
    /// Set once we deliver a terminate or kill signal to the tree.
    terminated: AtomicBool,
}

#[cfg(windows)]
struct JobInner {
    /// The Job object. Dropping it terminates the tree via kill-on-close, so it
    /// also serves as the on-demand kill.
    job: std::sync::Mutex<Option<win32job::Job>>,
    /// Set once we terminate the tree.
    terminated: AtomicBool,
}

impl Job {
    /// Whether procstream has delivered a terminate or kill signal to this
    /// tree.
    pub fn terminated(&self) -> bool {
        self.inner.terminated.load(Ordering::Relaxed)
    }
}

#[cfg(unix)]
impl Job {
    fn adopt(child: &mut std::process::Child) -> io::Result<Job> {
        Ok(Job {
            inner: Arc::new(JobInner {
                pgid: child.id() as i32,
                terminated: AtomicBool::new(false),
            }),
        })
    }

    /// Send `sig` to every process in the tree.
    pub fn signal(&self, sig: Signal) -> io::Result<()> {
        let os_sig = match sig {
            Signal::Interrupt => libc::SIGINT,
            Signal::Terminate => libc::SIGTERM,
            Signal::Kill => libc::SIGKILL,
        };
        if matches!(sig, Signal::Terminate | Signal::Kill) {
            self.inner.terminated.store(true, Ordering::Relaxed);
        }
        // A negative pid targets the whole process group.
        let rc = unsafe { libc::kill(-(self.inner.pgid as libc::pid_t), os_sig) };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// Escalating tree shutdown for contexts that only hold a [`Job`] clone:
    /// send `sig`, wait up to `grace` for the tree to die, then `SIGKILL`
    /// anything still alive.
    ///
    /// A `Job` cannot reap the leader, so pair this with [`Child::wait`]
    /// elsewhere. If the tree dies (or is reaped) during the grace period this
    /// returns early without sending the kill, so a stale group id is never
    /// signalled.
    pub fn shutdown(&self, sig: Signal, grace: Duration) -> io::Result<()> {
        self.signal(sig)?;

        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            // Probe with the null signal: once the group is gone there is
            // nothing left to escalate against.
            if unsafe { libc::kill(-(self.inner.pgid as libc::pid_t), 0) } != 0 {
                return Ok(());
            }
        }

        self.signal(Signal::Kill)
    }
}

#[cfg(windows)]
impl Job {
    fn adopt(child: &mut std::process::Child) -> io::Result<Job> {
        use std::os::windows::io::AsRawHandle;
        use win32job::Job as W;

        fn map_job_error(e: win32job::JobError) -> io::Error {
            match e {
                win32job::JobError::AssignFailed(e) => e,
                win32job::JobError::CreateFailed(e) => e,
                win32job::JobError::GetInfoFailed(e) => e,
                win32job::JobError::SetInfoFailed(e) => e,
                _ => io::Error::new(io::ErrorKind::Other, "Unknown error"),
            }
        }

        // Create a new Job object
        let job = W::create().map_err(map_job_error)?;

        // Configure the job to terminate all child processes when the job is closed
        let mut info = job.query_extended_limit_info().map_err(map_job_error)?;
        info.limit_kill_on_job_close();
        job.set_extended_limit_info(&info).map_err(map_job_error)?;
        job.assign_process(child.as_raw_handle() as _)
            .map_err(map_job_error)?;

        // Resume the main thread for the process
        let id = child.id();
        for thread_entry in tlhelp32::Snapshot::new_thread()? {
            if thread_entry.owner_process_id == id {
                use windows_sys::Win32::Foundation::CloseHandle;
                use windows_sys::Win32::System::Threading::*;

                unsafe {
                    let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, thread_entry.thread_id);
                    if thread.is_null() {
                        return Err(io::Error::last_os_error());
                    }
                    ResumeThread(thread);
                    CloseHandle(thread);
                }
            }
        }

        Ok(Job {
            inner: Arc::new(JobInner {
                job: std::sync::Mutex::new(Some(job)),
                terminated: AtomicBool::new(false),
            }),
        })
    }

    /// Send `sig` to the tree. Only [`Signal::Kill`] does anything on Windows:
    /// it terminates the Job (and with it every process). Graceful signals are
    /// a no-op, so escalate to `Kill`.
    pub fn signal(&self, sig: Signal) -> io::Result<()> {
        if let Signal::Kill = sig {
            self.inner.terminated.store(true, Ordering::Relaxed);
            if let Some(job) = self.inner.job.lock().unwrap().take() {
                // Terminate synchronously with a non-zero code. Kill-on-job-close
                // (via the handle drop) exits the processes with code 0, which
                // reads as clean success and hides the kill.
                use windows_sys::Win32::System::JobObjects::TerminateJobObject;
                unsafe { TerminateJobObject(job.handle() as _, 1) };
            }
        }
        Ok(())
    }

    /// Escalating tree shutdown for contexts that only hold a [`Job`] clone.
    ///
    /// Graceful signals are undeliverable on Windows, so rather than burning a
    /// grace period nothing observed, this terminates the Job immediately.
    pub fn shutdown(&self, _sig: Signal, _grace: Duration) -> io::Result<()> {
        self.signal(Signal::Kill)
    }
}

/// A spawned, isolated child process and its captured output of type `T`.
pub struct Child<T> {
    proc: std::process::Child,
    job: Job,
    output: Option<Output<T>>,
    readers: Vec<JoinHandle<()>>,
    stdin: Option<std::process::ChildStdin>,
}

impl<T> Child<T> {
    /// The immediate child's process id.
    pub fn id(&self) -> u32 {
        self.proc.id()
    }

    /// The isolation job for the whole tree. Clone it for a handle that can
    /// signal the tree from another thread.
    pub fn job(&self) -> &Job {
        &self.job
    }

    /// Send `sig` to the whole tree without waiting. Equivalent to
    /// `self.job().signal(sig)`.
    pub fn signal(&self, sig: Signal) -> io::Result<()> {
        self.job.signal(sig)
    }

    /// Whether procstream terminated the tree. See [`Job::terminated`].
    pub fn terminated(&self) -> bool {
        self.job.terminated()
    }

    /// Take the captured output queue. Panics if called more than once.
    pub fn output(&mut self) -> Output<T> {
        self.output.take().expect("output already taken")
    }

    /// Take the child's stdin handle, if stdin was piped.
    pub fn stdin(&mut self) -> Option<std::process::ChildStdin> {
        self.stdin.take()
    }

    /// Check whether the child has exited without blocking.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.proc.try_wait()
    }

    /// Wait for the child to exit and drain the reader threads.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        // Close our end of stdin first (as std's `wait` does) so a child
        // reading stdin to EOF exits rather than deadlocking against us.
        drop(self.stdin.take());
        let status = self.proc.wait()?;
        self.join_readers();
        Ok(status)
    }

    /// Convenience escalation: send `sig`, wait up to `grace` for the leader
    /// to exit, then `SIGKILL` anything still alive in the tree, and reap.
    ///
    /// The kill is sent even when the leader exits within the grace period:
    /// descendants that outlive it would otherwise hold the output pipes open
    /// and stall the drain indefinitely.
    ///
    /// For finer control (your own deadlines, back-off, or signal sequence),
    /// drive [`Child::signal`] and [`Child::try_wait`] directly instead.
    pub fn shutdown(&mut self, sig: Signal, grace: Duration) -> io::Result<ExitStatus> {
        drop(self.stdin.take());
        self.signal(sig)?;

        // Graceful signals are undeliverable on Windows (see [`Job::signal`]),
        // so don't burn a grace period nothing observed.
        let grace = if cfg!(windows) && !matches!(sig, Signal::Kill) {
            Duration::ZERO
        } else {
            grace
        };

        let deadline = Instant::now() + grace;
        let status = loop {
            if let Some(status) = self.proc.try_wait()? {
                break Some(status);
            }
            if Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };

        // Kill whatever remains, whether or not the leader exited in time. If
        // it is still running the group id is provably ours. After the
        // `try_wait` reap the reuse window is a two-syscall gap, not the grace.
        _ = self.signal(Signal::Kill);

        let status = match status {
            Some(status) => status,
            None => self.proc.wait()?,
        };
        self.join_readers();
        Ok(status)
    }

    fn join_readers(&mut self) {
        for handle in self.readers.drain(..) {
            _ = handle.join();
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::capture::Capture;
    use crate::transform::Transform;

    #[test]
    fn captures_lines() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("printf 'a\\nb\\nc\\n'");
        let mut child = cmd
            .spawn_job(Capture::piped(Transform::builder().lines()))
            .unwrap();

        let lines: Vec<String> = child
            .output()
            .iter()
            .map(|c| c.item.as_str_lossy().into_owned())
            .collect();
        assert_eq!(lines, vec!["a", "b", "c"]);
        assert!(child.wait().unwrap().success());
    }

    #[test]
    fn signal_kill_stops_a_sleep() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("sleep 30");
        let mut child = cmd.spawn_job(Capture::piped(Transform::raw())).unwrap();

        child.signal(Signal::Kill).unwrap();
        // The child must die promptly rather than sleeping for 30s.
        let start = Instant::now();
        let status = child.wait().unwrap();
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(!status.success());
    }

    #[test]
    fn shutdown_reaps_a_child_that_ignores_sigterm() {
        // Trap SIGTERM so only the escalating SIGKILL can bring it down.
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("trap '' TERM; sleep 30");
        let mut child = cmd.spawn_job(Capture::piped(Transform::raw())).unwrap();

        let start = Instant::now();
        let status = child
            .shutdown(Signal::Terminate, Duration::from_millis(200))
            .unwrap();
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(!status.success());
    }

    #[test]
    fn shutdown_kills_descendants_that_outlive_the_leader() {
        // The leader exits immediately but leaves behind a TERM-ignoring
        // grandchild holding the output pipe; shutdown must SIGKILL the group
        // rather than hang draining the readers.
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("trap '' TERM; sleep 30 & echo go; exit 0");
        let mut child = cmd.spawn_job(Capture::piped(Transform::raw())).unwrap();

        // Wait for the echo so the trap is installed (and the leader is about
        // to exit) before we start signalling.
        let output = child.output();
        assert!(output.recv().is_some());

        let start = Instant::now();
        let status = child
            .shutdown(Signal::Terminate, Duration::from_millis(200))
            .unwrap();
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(status.success());
    }

    #[test]
    fn wait_closes_piped_stdin() {
        // `cat` reads stdin to EOF; wait() must close our end of the pipe
        // rather than deadlock against a child waiting for input.
        let mut cmd = Command::new("cat");
        let mut child = cmd
            .spawn_job(
                Capture::builder()
                    .stdout(Transform::raw())
                    .stdin_piped()
                    .build(),
            )
            .unwrap();

        let start = Instant::now();
        assert!(child.wait().unwrap().success());
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn terminated_tracks_our_own_kill() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("sleep 30");
        let mut child = cmd.spawn_job(Capture::piped(Transform::raw())).unwrap();

        assert!(!child.terminated());
        child.signal(Signal::Kill).unwrap();
        assert!(child.terminated());
        child.wait().unwrap();
    }

    #[test]
    fn job_shutdown_returns_early_when_the_tree_dies() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("sleep 30");
        let mut child = cmd.spawn_job(Capture::piped(Transform::raw())).unwrap();
        let job = child.job().clone();

        // A Job cannot reap, so the terminated leader lingers as a zombie in
        // its own group until a Child owner waits on it (as clitest's main
        // thread does). Reap on another thread so shutdown's liveness probe
        // sees the group vanish and returns without burning the whole grace.
        let reaper = std::thread::spawn(move || child.wait());

        let start = Instant::now();
        job.shutdown(Signal::Terminate, Duration::from_secs(10))
            .unwrap();
        assert!(start.elapsed() < Duration::from_secs(5));

        assert!(!reaper.join().unwrap().unwrap().success());
    }
}
