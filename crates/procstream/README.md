# procstream

Make management of process trees and their output streams easy.

Spawn background processes, capture their output as a stream of typed items, and
kill the whole process tree across platforms.

`procstream` extends `std::process::Command` rather than wrapping it: you build
the child with the full std builder and add things std can't do on its own.

## Process-tree isolation (and termination)

`spawn_job` places the child in a new process group (Unix) or Job object
(Windows).

`signal(Signal::...)` sends a signal to the whole tree. Pair it with
`try_wait`/`wait` to drive your own deadlines, or use the `shutdown(signal, grace)`
convenience function to escalate to `SIGKILL`. `job().clone()` gives a
handle that can signal the tree from another thread.

## Streamed, typed capture

stdout/stderr are delivered as a queue of events, run through a configurable
transform pipeline (ANSI stripping, `\r` overwrite collapse, UTF-8 sanitizing)
terminated by a `Framer` that sets the output type. `Capture::lines()` yields
`Line`s, `Capture::raw()` yields `Vec<u8>` byte runs, and a custom framer
yields anything.

## Inline exit events

Delivers `Event::Exit(status)` alongside the chunks, so one `recv` loop sees
output, exit, and end-of-stream with no polling.

## Capture backend

On macOS and Linux polling the stream makes use of non-blocking system calls. An
inline driver over kqueue or epoll reads the streams and reaps the child,
advanced by the consumer's own `recv`/`wait` calls. Other platforms use one
reader thread per stream plus a watcher thread.

Two consequences of the inline driver worth knowing:

- Driving a single child is serialized: `recv` and `wait` share the child's
  driver behind a lock, so only one advances it at a time. Signalling the tree
  through a `Job` handle stays free from another thread.
- Dropping both handles without draining leaves the child unreaped (a zombie),
  since nothing advances the driver after the drop. Drain to EOF, or `wait`,
  before dropping.

```rust,no_run
use std::process::Command;
use std::time::Duration;
use procstream::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
  let mut cmd = Command::new("some-long-running-command");
  let (mut child, output) = cmd.spawn_job(Capture::lines())?;

  for event in output.iter() {
      match event {
          // chunk.item is a Line, tagged with chunk.item.ending.
          Event::Chunk(chunk) => println!("{:?}: {}", chunk.stream, chunk.item.as_str_lossy()),
          Event::Exit(status) => println!("exited: {status}"),
      }
  }

  let _status = child.shutdown(Signal::Terminate, Duration::from_secs(5))?;
  Ok(())
}
```
