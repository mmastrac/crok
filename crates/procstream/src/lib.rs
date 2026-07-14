#![doc = include_str!("../README.md")]

mod capture;
mod job;
// kqueue/epoll only. Other platforms fall back to reader threads.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod driver;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod poller;
mod transform;

pub use capture::{Capture, CaptureBuilder, Chunk, Event, Output, RecvTimeout, Sink, Stdin, Stream};
pub use job::{Child, CommandJobExt, Job, Signal};
pub use transform::{
    Ansi, AnsiFilter, ByteFilter, CollapseLine, Framer, Line, LineEnding, LineFramer, Overlong,
    Overwrite, Transform, TransformBuilder, Utf8, Utf8Filter,
};

pub mod prelude {
    pub use crate::{
        Ansi, Capture, Child, Chunk, CommandJobExt, Event, Job, Line, LineEnding, Output,
        Overlong, Overwrite, RecvTimeout, Signal, Stream, Transform, Utf8,
    };
}
