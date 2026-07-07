# procstream

Spawn background processes, capture their output as a stream of typed items, and
kill the whole process tree across platforms.

`procstream` extends `std::process::Command` rather than wrapping it: you build
the child with the full std builder and add three things std can't do on its
own.

- **Process-tree isolation**: `spawn_job` places the child in a new process
  group (Unix) or Job object (Windows).
- **Streamed, typed capture**: stdout/stderr are delivered as a queue of
  events, run through a configurable transform pipeline (ANSI stripping,
  `\r` overwrite collapse, UTF-8 sanitizing) terminated by a `Framer` that sets
  the output type. `Capture::lines()` yields `Line`s, `Capture::raw()` yields
  `Vec<u8>` byte runs, and a custom framer yields anything.
- **Exit on the same queue**: a watcher thread reaps the child and delivers
  `Event::Exit(status)` alongside the chunks, so one `recv` loop sees output,
  exit, and end-of-stream with no polling. `wait`/`try_wait` still work.
- **Tree-wide termination**: `signal(Signal::…)` sends a signal to the whole
  tree. Pair it with `try_wait`/`wait` to drive your own deadlines, or use the
  `shutdown(signal, grace)` convenience to escalate to SIGKILL. `job().clone()`
  gives a handle that can signal the tree from another thread.

```rust
use std::process::Command;
use std::time::Duration;
use procstream::prelude::*;

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
```

## Status

Extracted from the process-management code in crok, stylus, and ssu. The
readiness reactor (a thread-free, runtime-free capture backend built on
`rustix`) is designed for but not yet implemented. It slots in behind `Output`
without an API change.
