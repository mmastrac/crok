//! Spawn background processes, capture their output as a stream of typed items,
//! and kill the whole process tree across platforms.
//!
//! `procstream` extends [`std::process::Command`] rather than wrapping it, so
//! you configure the child with the full std builder and add three things std
//! can't do on its own: process-tree isolation, streamed output capture, and a
//! tree-wide kill.
//!
//! A capture's transform sets its output type: [`Transform::builder().lines()`]
//! yields [`Line`]s, the default yields `Vec<u8>` byte runs, and a custom
//! [`Framer`] yields anything. That type flows through to the [`Chunk`]s you
//! read.
//!
//! [`Transform::builder().lines()`]: TransformBuilder::lines
//!
//! ```no_run
//! use std::process::Command;
//! use std::time::Duration;
//! use procstream::prelude::*;
//!
//! // A plain std::process::Command, configured however you like.
//! let mut cmd = Command::new("some-long-running-command");
//!
//! // Spawn it into a fresh isolated job, framing output into lines.
//! let (mut child, output) = cmd.spawn_job(Capture::lines())?;
//!
//! for event in output.iter() {
//!     match event {
//!         // chunk.item is a Line, because the transform framed lines.
//!         Event::Chunk(chunk) => println!("{:?}: {}", chunk.stream, chunk.item.as_str_lossy()),
//!         // The leader exited; chunks may still follow from its descendants.
//!         Event::Exit(status) => println!("exited: {status}"),
//!     }
//! }
//!
//! // Signal the whole tree. Send `Signal::Terminate` now and drive your own
//! // deadline with `try_wait`, or let `shutdown` escalate to SIGKILL for you.
//! let _status = child.shutdown(Signal::Terminate, Duration::from_secs(5))?;
//! # Ok::<(), std::io::Error>(())
//! ```

mod capture;
mod job;
mod transform;

pub use capture::{Capture, CaptureBuilder, Chunk, Event, Output, RecvTimeout, Sink, Stdin, Stream};
pub use job::{Child, CommandJobExt, Job, Signal};
pub use transform::{
    Ansi, AnsiFilter, ByteFilter, CollapseLine, Framer, Line, LineEnding, LineFramer, Overlong,
    Overwrite, Transform, TransformBuilder, Utf8, Utf8Filter,
};

/// The common types, ready to glob-import: `use procstream::prelude::*;`.
pub mod prelude {
    pub use crate::{
        Ansi, Capture, Child, Chunk, CommandJobExt, Event, Job, Line, LineEnding, Output,
        Overlong, Overwrite, RecvTimeout, Signal, Stream, Transform, Utf8,
    };
}
