#![doc = include_str!("../README.md")]

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
