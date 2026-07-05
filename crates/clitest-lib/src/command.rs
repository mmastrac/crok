use std::{
    collections::HashMap,
    process::{Command, ExitStatus},
    time::{Duration, Instant},
};

use procstream::{Capture, CommandJobExt, Line, LineEnding, Signal, Stream, Transform};
use serde::Serialize;
use shellish_parse::ParseOptions;
use termcolor::Color;

use crate::{
    cwrite, cwriteln,
    output::Lines,
    script::{ScriptKillReceiver, ScriptKillSender, ScriptLocation},
};

#[derive(Copy, Clone, derive_more::Debug, PartialEq, Eq)]
pub enum CommandResult {
    #[debug("{_0:?}")]
    Exit(ExitStatus, bool),
    #[debug("timed out")]
    TimedOut,
}

impl CommandResult {
    pub fn success(&self) -> bool {
        match self {
            CommandResult::Exit(status, _) => status.success(),
            CommandResult::TimedOut => false,
        }
    }
}

impl std::fmt::Display for CommandResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommandResult::Exit(status, killed) => {
                if *killed {
                    write!(f, "killed")?;
                    // On Unix the status also names the signal that it gets.
                    #[cfg(unix)]
                    {
                        use std::os::unix::process::ExitStatusExt;
                        if status.signal().is_some() {
                            write!(f, "; {status}")?;
                        }
                    }
                    Ok(())
                } else {
                    write!(f, "{status}")
                }
            }
            CommandResult::TimedOut => write!(f, "timed out"),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(transparent)]
pub struct CommandLine {
    pub command: String,
    #[serde(skip)]
    pub location: ScriptLocation,
    #[serde(skip)]
    pub line_count: usize,
}

impl CommandLine {
    pub fn new(command: String, location: ScriptLocation, line_count: usize) -> Self {
        Self {
            command,
            location,
            line_count,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn run(
        &self,
        writer: &mut dyn termcolor::WriteColor,
        show_line_numbers: bool,
        runner: Option<String>,
        timeout: Duration,
        envs: &HashMap<String, String>,
        kill_receiver: &ScriptKillReceiver,
        kill_sender: &ScriptKillSender,
    ) -> Result<(Lines, CommandResult), std::io::Error> {
        let start = Instant::now();
        let warn_time = timeout.saturating_mul(90) / 100;
        let timeout = timeout.saturating_mul(110) / 100;

        let mut command = if let Some(runner) = runner {
            let bits = shellish_parse::parse(&runner, ParseOptions::default())
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
            let mut cmd = Command::new(&bits[0]);
            cmd.args(&bits[1..]);
            cmd
        } else {
            let mut cmd = Command::new("sh");
            cmd.arg("-c");
            cmd
        };
        command.arg(&self.command);
        command.envs(envs);
        if let Some(pwd) = envs.get("PWD") {
            command.current_dir(pwd);
        }

        // Spawn into an isolated job (a new process group / Job object) with each
        // line of stdout and stderr delivered as a chunk.
        let mut child = command.spawn_job(Capture::piped(Transform::builder().lines()))?;

        let job = child.job().clone();
        let output = child.output();

        // Watch the script-wide kill flag and bring the whole tree down if it is
        // set, while we consume the command's output on this thread. Terminate
        // gracefully, then hard-kill anything that ignores it.
        let result = kill_receiver.run_with(
            || _ = job.shutdown(Signal::Terminate, Duration::from_millis(250)),
            move || {
                let mut line_number = 1;
                let mut output_lines = vec![];
                let mut overlong = String::new();
                let mut warned = false;
                let mut closed = false;

                loop {
                    // Enforce the hard timeout on every pass: a chatty command
                    // keeps the Ok arm busy and a closed stream skips recv
                    // entirely, so neither may dodge the deadline.
                    if start.elapsed() >= timeout {
                        cwriteln!(writer, fg = Color::Yellow, "Process took too long!");
                        kill_sender.kill();
                        _ = child.shutdown(Signal::Terminate, Duration::from_millis(250));
                        return Ok((Lines::new(output_lines), CommandResult::TimedOut));
                    }

                    // The streams are done, but the child may still be running
                    // with its output redirected elsewhere; poll it against the
                    // same deadline.
                    if closed {
                        if child.try_wait()?.is_some() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }

                    // Wake at the warning threshold (once), then again at the hard
                    // timeout, even if the command is producing no output.
                    let remaining = timeout.saturating_sub(start.elapsed());
                    let wait = if warned {
                        remaining
                    } else {
                        remaining.min(warn_time.saturating_sub(start.elapsed()))
                    };

                    match output.recv_timeout(wait) {
                        Ok(chunk) => {
                            let stream = chunk.stream;
                            let Line { bytes, ending } = chunk.item;
                            // Move the bytes into a String, copying only when
                            // invalid UTF-8 forces a lossy pass.
                            let text = String::from_utf8(bytes).unwrap_or_else(|e| {
                                String::from_utf8_lossy(e.as_bytes()).into_owned()
                            });

                            // A line longer than the framer's cap arrives in
                            // pieces; stitch them back into one logical line.
                            if matches!(ending, LineEnding::Overlong) {
                                overlong.push_str(&text);
                                continue;
                            }
                            let mut line = if overlong.is_empty() {
                                text
                            } else {
                                overlong.push_str(&text);
                                std::mem::take(&mut overlong)
                            };
                            // Drop a bare trailing CR on an unterminated final
                            // line, matching the CRLF handling on terminated
                            // lines.
                            if matches!(ending, LineEnding::Eof) && line.ends_with('\r') {
                                line.pop();
                            }

                            if show_line_numbers {
                                cwrite!(
                                    writer,
                                    fg = Color::White,
                                    dimmed = true,
                                    "{line_number:>3} "
                                );
                            }

                            // Careful that we don't print ANSI escape sequences
                            let line_out = fast_strip_ansi::strip_ansi_string(&line);
                            if stream == Stream::Stdout {
                                cwriteln!(writer, fg = Color::White, "{line_out}");
                            } else {
                                cwriteln!(writer, fg = Color::Yellow, "{line_out}");
                            }

                            output_lines.push(line);
                            line_number += 1;
                        }
                        Err(procstream::RecvTimeout::Closed) => closed = true,
                        Err(procstream::RecvTimeout::Timeout) => {
                            if !warned && start.elapsed() < timeout {
                                eprintln!("Process #{} taking too long to finish.", child.id());
                                warned = true;
                            }
                        }
                    }
                }

                let status = child.wait()?;
                Ok((Lines::new(output_lines), CommandResult::Exit(status, false)))
            },
        );

        // `run_with` has joined the kill watcher, so any terminate it issued is
        // now visible here. Read the flag out here rather than inside the
        // closure, where it would race the watcher thread that sets it.
        match result {
            Ok((lines, CommandResult::Exit(status, _))) => {
                Ok((lines, CommandResult::Exit(status, job.terminated())))
            }
            other => other,
        }
    }
}
