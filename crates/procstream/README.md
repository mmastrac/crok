# procstream

Spawn background processes, capture their output as a stream of typed items, and
kill the whole process tree across platforms.

`procstream` extends `std::process::Command` rather than wrapping it: you build
the child with the full std builder and add three things std can't do on its
own.

- **Process-tree isolation**: `spawn_job` places the child in a new process
  group (Unix) or Job object (Windows).
- **Streamed, typed capture**: stdout/stderr are delivered as a queue of
  `Chunk<T>`s, run through a configurable transform pipeline (ANSI stripping,
  `\r` overwrite collapse, UTF-8 sanitizing) terminated by a `Framer` that sets
  the output type. `.lines()` yields `Line`s, the default yields `Vec<u8>` byte
  runs, and a custom framer yields anything.
- **Tree-wide termination**: `signal(Signal::…)` sends a signal to the whole
  tree. Pair it with `try_wait`/`wait` to drive your own deadlines, or use the
  `shutdown(signal, grace)` convenience to escalate to SIGKILL. `job().clone()`
  gives a handle that can signal the tree from another thread.

```rust
use std::process::Command;
use std::time::Duration;
use procstream::prelude::*;

let mut cmd = Command::new("some-long-running-command");
let mut child = cmd.spawn_job(Capture::piped(Transform::builder().lines()))?;

for chunk in child.output().iter() {
    // chunk.item is a Line, tagged with chunk.item.ending.
    println!("{:?}: {}", chunk.stream, chunk.item.as_str_lossy());
}

let _status = child.shutdown(Signal::Terminate, Duration::from_secs(5))?;
```

## Status

Extracted from the process-management code in clitest, stylus, and ssu. The
readiness reactor (a thread-free, runtime-free capture backend built on
`rustix`) is designed for but not yet implemented. It slots in behind `Output`
without an API change.
