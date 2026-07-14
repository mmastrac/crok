//! Spawning a process into an isolated job and killing the whole tree.
//!
//! [`CommandJobExt`] extends [`std::process::Command`] with `spawn_job`, which
//! places the child in a fresh isolation unit (a process group on Unix, a Job
//! object on Windows) so that [`Job::signal`] and [`Child::shutdown`] act
//! on the whole tree rather than just the immediate child.

use std::io;
use std::process::{Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::capture::{Capture, DriverHandle, Event, Output};

/// A signal to deliver to a whole process tree.
///
/// On Unix these map to `SIGINT`/`SIGTERM`/`SIGKILL`. On Windows there is no
/// portable graceful signal for a job tree, so only [`Signal::Kill`] does
/// anything (it terminates the Job). The others are a no-op.
#[derive(Copy, Clone, Debug)]
pub enum Signal {
    Interrupt,
    Terminate,
    Kill,
}

/// Extends [`Command`] with process-tree isolation and capture.
pub trait CommandJobExt {
    /// Spawn into a fresh isolated job (a new process group / Job object) with
    /// the given capture, returning the child and its output queue. The output
    /// type `T` comes from the capture's transform.
    ///
    /// `spawn_job` owns the stdio (it sets it from `capture`) and, on Unix, the
    /// process group, and on Windows the creation flags. If you need those knobs
    /// yourself, they collide with the isolation.
    fn spawn_job<T: Send + 'static>(
        &mut self,
        capture: Capture<T>,
    ) -> io::Result<(Child, Output<T>)>;
}

impl CommandJobExt for Command {
    fn spawn_job<T: Send + 'static>(
        &mut self,
        capture: Capture<T>,
    ) -> io::Result<(Child, Output<T>)> {
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
        let pid = child.id();
        let exit = Arc::new(ExitState::default());

        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let (output, driver, readers, stdin) = {
            let (rx, streams, stdin, exit_tx) = capture.build_streams(&mut child);
            let on_exit: Box<dyn FnMut(ExitStatus) + Send> =
                Box::new(move |status| _ = exit_tx.send(Event::Exit(status)));
            let driver: Arc<Mutex<dyn crate::capture::Advance>> = Arc::new(Mutex::new(
                crate::driver::Driver::new(child, streams, Arc::clone(&exit), on_exit)?,
            ));
            let output = Output::new(rx, Some(Arc::clone(&driver)));
            (output, Some(driver), Vec::new(), stdin)
        };

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let (output, driver, readers, stdin) = {
            let (output, mut readers, stdin, tx) = capture.start(&mut child);

            let watcher_exit = Arc::clone(&exit);
            readers.push(std::thread::spawn(move || {
                let result = child.wait();
                let status = result.as_ref().ok().copied();
                // Store before sending so a consumer that sees the event can
                // immediately observe the status through `try_wait`.
                watcher_exit.set(result);
                if let Some(status) = status {
                    _ = tx.send(Event::Exit(status));
                }
            }));

            (output, None, readers, stdin)
        };

        Ok((
            Child {
                pid,
                job,
                exit,
                stdin,
                driver,
                readers,
            },
            output,
        ))
    }
}

/// The leader's reaped exit, shared between whoever reaps it and `wait`/`try_wait`.
#[derive(Default)]
pub(crate) struct ExitState {
    status: Mutex<Option<Result<ExitStatus, Arc<io::Error>>>>,
    cond: Condvar,
}

impl ExitState {
    pub(crate) fn set(&self, result: io::Result<ExitStatus>) {
        let mut guard = self.status.lock().unwrap();
        *guard = Some(result.map_err(Arc::new));
        self.cond.notify_all();
    }

    fn get(&self) -> io::Result<Option<ExitStatus>> {
        share_result(self.status.lock().unwrap().as_ref())
    }

    // Thread backend only. The driver backend reaches exit via `poll_once`.
    fn wait(&self) -> io::Result<ExitStatus> {
        let mut guard = self.status.lock().unwrap();
        loop {
            if guard.is_some() {
                return share_result(guard.as_ref()).map(|s| s.expect("checked above"));
            }
            guard = self.cond.wait(guard).unwrap();
        }
    }
}

fn share_result(
    stored: Option<&Result<ExitStatus, Arc<io::Error>>>,
) -> io::Result<Option<ExitStatus>> {
    match stored {
        None => Ok(None),
        Some(Ok(status)) => Ok(Some(*status)),
        Some(Err(e)) => Err(io::Error::new(e.kind(), Arc::clone(e))),
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
    pgid: i32,
    terminated: AtomicBool,
}

#[cfg(windows)]
struct JobInner {
    /// Dropping terminates the tree via kill-on-close.
    job: std::sync::Mutex<Option<win32job::Job>>,
    terminated: AtomicBool,
}

impl Job {
    /// Whether procstream has delivered a terminate or kill signal to this tree.
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

    /// Send `sig`, wait up to `grace` for the tree to die, then `SIGKILL`
    /// anything still alive. For contexts that only hold a [`Job`] clone.
    pub fn shutdown(&self, sig: Signal, grace: Duration) -> io::Result<()> {
        self.signal(sig)?;

        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            // Null signal: once the group is gone there is nothing to escalate.
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

        let job = W::create().map_err(map_job_error)?;

        let mut info = job.query_extended_limit_info().map_err(map_job_error)?;
        info.limit_kill_on_job_close();
        job.set_extended_limit_info(&info).map_err(map_job_error)?;
        job.assign_process(child.as_raw_handle() as _)
            .map_err(map_job_error)?;

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

    /// Only [`Signal::Kill`] does anything on Windows: it terminates the Job.
    /// Graceful signals are a no-op, so escalate to `Kill`.
    pub fn signal(&self, sig: Signal) -> io::Result<()> {
        if let Signal::Kill = sig {
            self.inner.terminated.store(true, Ordering::Relaxed);
            if let Some(job) = self.inner.job.lock().unwrap().take() {
                // Kill-on-job-close (handle drop) exits with code 0, which
                // reads as clean success and hides the kill.
                use windows_sys::Win32::System::JobObjects::TerminateJobObject;
                unsafe { TerminateJobObject(job.handle() as _, 1) };
            }
        }
        Ok(())
    }

    /// Graceful signals are undeliverable on Windows, so rather than burning a
    /// grace period nothing observed, this terminates the Job immediately.
    pub fn shutdown(&self, _sig: Signal, _grace: Duration) -> io::Result<()> {
        self.signal(Signal::Kill)
    }
}

/// A spawned, isolated child process. Captured output arrives on the [`Output`]
/// returned by [`spawn_job`](CommandJobExt::spawn_job).
pub struct Child {
    pid: u32,
    job: Job,
    exit: Arc<ExitState>,
    stdin: Option<std::process::ChildStdin>,
    /// `None` on the thread backend.
    driver: DriverHandle,
    /// Empty on the driver backend.
    readers: Vec<JoinHandle<()>>,
}

impl Child {
    pub fn id(&self) -> u32 {
        self.pid
    }

    /// The isolation job for the whole tree. Clone it to signal from another thread.
    pub fn job(&self) -> &Job {
        &self.job
    }

    /// Send `sig` to the whole tree without waiting.
    pub fn signal(&self, sig: Signal) -> io::Result<()> {
        self.job.signal(sig)
    }

    pub fn terminated(&self) -> bool {
        self.job.terminated()
    }

    pub fn stdin(&mut self) -> Option<std::process::ChildStdin> {
        self.stdin.take()
    }

    pub fn try_wait(&self) -> io::Result<Option<ExitStatus>> {
        // Skip if another thread holds the driver: a blocking recv must not
        // stall a non-blocking try_wait.
        if let Some(driver) = &self.driver
            && let Ok(mut driver) = driver.try_lock()
        {
            driver.poll_once(Some(Duration::ZERO));
        }
        self.exit.get()
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        // As std's `wait`: close stdin so a child reading to EOF can exit.
        drop(self.stdin.take());
        let status = self.await_exit()?;
        self.join_readers();
        Ok(status)
    }

    // Driver backend: drive (and drain streams) until exit. Thread backend:
    // the watcher publishes the exit.
    fn await_exit(&self) -> io::Result<ExitStatus> {
        let Some(driver) = &self.driver else {
            return self.exit.wait();
        };
        loop {
            if let Some(status) = self.exit.get()? {
                return Ok(status);
            }
            driver.lock().unwrap().poll_once(None);
        }
    }

    /// Send `sig`, wait up to `grace` for the leader to exit, then `SIGKILL`
    /// anything still alive in the tree, and reap.
    ///
    /// The kill is sent even when the leader exits within the grace period:
    /// descendants that outlive it would otherwise hold the output pipes open
    /// and stall the drain indefinitely.
    pub fn shutdown(&mut self, sig: Signal, grace: Duration) -> io::Result<ExitStatus> {
        drop(self.stdin.take());
        self.signal(sig)?;

        // Graceful signals are a no-op on Windows. Don't burn a grace period.
        let grace = if cfg!(windows) && !matches!(sig, Signal::Kill) {
            Duration::ZERO
        } else {
            grace
        };

        let deadline = Instant::now() + grace;
        let status = loop {
            if let Some(status) = self.try_wait()? {
                break Some(status);
            }
            if Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };

        // Always kill the tree: while the leader runs the pgid is ours. After
        // it is reaped the reuse window is a two-syscall gap, not the grace.
        _ = self.signal(Signal::Kill);

        let status = match status {
            Some(status) => status,
            None => self.await_exit()?,
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
    use crate::capture::{Capture, RecvTimeout};
    use crate::transform::Transform;

    #[test]
    fn captures_lines() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("printf 'a\\nb\\nc\\n'");
        let (mut child, output) = cmd.spawn_job(Capture::lines()).unwrap();

        let lines: Vec<String> = output
            .iter()
            .filter_map(|e| match e {
                Event::Chunk(c) => Some(c.item.as_str_lossy().into_owned()),
                Event::Exit(_) => None,
            })
            .collect();
        assert_eq!(lines, vec!["a", "b", "c"]);
        assert!(child.wait().unwrap().success());
    }

    #[test]
    fn exit_event_is_delivered() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("echo out; exit 3");
        let (child, output) = cmd.spawn_job(Capture::lines()).unwrap();

        let mut exit = None;
        for event in output.iter() {
            if let Event::Exit(status) = event {
                assert!(child.try_wait().unwrap().is_some());
                exit = Some(status);
            }
        }
        assert_eq!(exit.unwrap().code(), Some(3));
    }

    #[test]
    fn signal_kill_stops_a_sleep() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("sleep 30");
        let (mut child, _output) = cmd.spawn_job(Capture::raw()).unwrap();

        child.signal(Signal::Kill).unwrap();
        let start = Instant::now();
        let status = child.wait().unwrap();
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(!status.success());
    }

    #[test]
    fn shutdown_reaps_a_child_that_ignores_sigterm() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("trap '' TERM; sleep 30");
        let (mut child, _output) = cmd.spawn_job(Capture::raw()).unwrap();

        let start = Instant::now();
        let status = child
            .shutdown(Signal::Terminate, Duration::from_millis(200))
            .unwrap();
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(!status.success());
    }

    #[test]
    fn shutdown_kills_descendants_that_outlive_the_leader() {
        // Leader exits immediately. TERM-ignoring grandchild holds the pipe.
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("trap '' TERM; sleep 30 & echo go; exit 0");
        let (mut child, output) = cmd.spawn_job(Capture::raw()).unwrap();

        // Wait for the echo so the trap is installed before signalling.
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
        // `cat` reads stdin to EOF. Wait must close our end or deadlock.
        let mut cmd = Command::new("cat");
        let (mut child, _output) = cmd
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
    fn recv_timeout_expires_then_drains_after_kill() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("sleep 30");
        let (mut child, output) = cmd.spawn_job(Capture::raw()).unwrap();

        let start = Instant::now();
        assert!(matches!(
            output.recv_timeout(Duration::from_millis(50)),
            Err(RecvTimeout::Timeout)
        ));
        assert!(start.elapsed() < Duration::from_secs(5));

        child.signal(Signal::Kill).unwrap();
        loop {
            match output.recv_timeout(Duration::from_secs(5)) {
                Ok(_) => {}
                Err(RecvTimeout::Closed) => break,
                Err(RecvTimeout::Timeout) => panic!("queue did not close after kill"),
            }
        }
        child.wait().unwrap();
    }

    #[test]
    fn terminated_tracks_our_own_kill() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("sleep 30");
        let (mut child, _output) = cmd.spawn_job(Capture::raw()).unwrap();

        assert!(!child.terminated());
        child.signal(Signal::Kill).unwrap();
        assert!(child.terminated());
        child.wait().unwrap();
    }

    #[test]
    fn job_shutdown_returns_early_when_the_tree_dies() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("sleep 30");
        let (mut child, output) = cmd.spawn_job(Capture::raw()).unwrap();
        let job = child.job().clone();

        // Drain so the leader is reaped (driver only reaps while driven) and
        // the liveness probe can see the group vanish.
        let start = Instant::now();
        std::thread::scope(|s| {
            s.spawn(move || output.iter().for_each(drop));
            job.shutdown(Signal::Terminate, Duration::from_secs(10))
                .unwrap();
        });
        assert!(start.elapsed() < Duration::from_secs(5));

        assert!(!child.wait().unwrap().success());
    }
}
