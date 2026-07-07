use std::sync::Mutex;
pub use termcolor::Color;
use termcolor::{ColorChoice, StandardStream};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub static STDOUT: std::sync::LazyLock<Mutex<StandardStream>> =
    std::sync::LazyLock::new(|| Mutex::new(StandardStream::stdout(ColorChoice::Auto)));

pub static IS_UTF8: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
    utf8_supported::utf8_supported() == utf8_supported::Utf8Support::UTF8
});

/// Estimate the width of the terminal. Falls back to 79 if the width cannot be
/// determined.
pub fn term_width() -> usize {
    termsize::get().map(|s| (s.cols - 1) as usize).unwrap_or(79)
}

#[allow(clippy::while_let_loop)]
pub fn compute_rule_string(message: &str, max_width: usize) -> String {
    if message.width() <= max_width {
        message.to_string()
    } else {
        let mut chars = message.graphemes(true);

        let mut start = String::with_capacity(max_width / 2);
        let mut end = String::with_capacity(max_width / 2);
        let ellipsis = "…";

        loop {
            if let Some(grapheme) = chars.next() {
                let prev_len = start.len();
                start.push_str(grapheme);
                if start.width() + end.width() + ellipsis.width() > max_width {
                    start.truncate(prev_len);
                    break;
                }
            } else {
                break;
            }
            if let Some(grapheme) = chars.next_back() {
                let prev_len = end.len();
                end.insert_str(0, grapheme);
                if start.width() + end.width() + ellipsis.width() > max_width {
                    end.drain(0..(end.len() - prev_len));
                    break;
                }
            } else {
                break;
            }
        }

        let s = format!("{start}{ellipsis}{end}");
        debug_assert!(s.width() <= max_width);
        s
    }
}

#[macro_export]
macro_rules! println {
    ($($arg:tt)*) => {
        {
            use std::io::Write;
            _ = writeln!($crate::term::STDOUT.lock().unwrap(), $($arg)*);
        }
    };
}

#[macro_export]
macro_rules! cwriteln {
    ($stream:expr) => {
        {
            use termcolor::{WriteColor, ColorSpec};
            let stream: &mut dyn WriteColor = &mut $stream;
            _ = stream.set_color(&ColorSpec::new());
            _ = writeln!(stream);
        }
    };
    ($stream:expr, $(fg=$fg:expr,)? $(bg=$bg:expr,)? $(bold=$bold:expr,)? $(dimmed=$dimmed:expr,)? $literal:literal $($arg:tt)*) => {
        {
            #[allow(unused_imports)]
            use std::io::Write;
            $crate::cwrite!($stream, $(fg=$fg,)? $(bg=$bg,)? $(bold=$bold,)? $(dimmed=$dimmed,)? $literal $($arg)*);
            _ = writeln!($stream);
        }
    };
}

#[macro_export]
macro_rules! cwrite {
    ($stream:expr, $(fg=$fg:expr,)? $(bg=$bg:expr,)? $(bold=$bold:expr,)? $(dimmed=$dimmed:expr,)? $literal:literal $($arg:tt)*) => {
        {
            #[allow(unused_imports)]
            use termcolor::{WriteColor, ColorSpec};
            #[allow(unused_imports)]
            use std::io::Write;

            #[allow(unused_mut)]
            let mut color = ColorSpec::new();
            $(
                color.set_bg(Some($bg));
            )?
            $(
                color.set_fg(Some($fg));
            )?
            $(
                color.set_bold($bold);
            )?
            $(
                color.set_dimmed($dimmed);
            )?
            _ = $stream.set_color(&color);
            let mut s = format!($literal $($arg)*);
            if s.contains('\x1b') {
                s = s.replace('\x1b', "\u{241B}"); // "ESC"
            }
            _ = write!($stream, "{s}");
            _ = $stream.set_color(&ColorSpec::new());
        }
    };
}

#[macro_export]
macro_rules! cprintln {
    () => {
        {
            let mut stdout = $crate::term::STDOUT.lock().unwrap();
            $crate::cwriteln!(&mut *stdout);
        }
    };
    ($(fg=$fg:expr,)? $(bg=$bg:expr,)? $(bold=$bold:expr,)? $(dimmed=$dimmed:expr,)? $literal:literal $($arg:tt)*) => {
        {
            let mut stdout = $crate::term::STDOUT.lock().unwrap();
            $crate::cwriteln!(&mut stdout, $(fg=$fg,)? $(bg=$bg,)? $(bold=$bold,)? $(dimmed=$dimmed,)? $literal $($arg)*);
        }
    };
}

#[macro_export]
macro_rules! cprint {
    ($(fg=$fg:expr,)? $(bg=$bg:expr,)? $(bold=$bold:expr,)? $(dimmed=$dimmed:expr,)? $literal:literal $($arg:tt)*) => {
        {
            let mut stdout = $crate::term::STDOUT.lock().unwrap();
            $crate::cwrite!(&mut stdout, $(fg=$fg,)? $(bg=$bg,)? $(bold=$bold,)? $(dimmed=$dimmed,)? $literal $($arg)*);
        }
    };
}

/// Print a rule of dashes, optionally with an embedded message.
///
/// ```nocompile
/// -[messsage]----------------- ...
/// ```
#[macro_export]
macro_rules! cwriteln_rule {
    ($stream:expr) => {
        let is_utf8 = *$crate::term::IS_UTF8;

        if is_utf8 {
            $crate::cwriteln!(
                $stream,
                dimmed = true,
                "{:─>count$}",
                "",
                count = $crate::term::term_width() - 1
            );
        } else {
            $crate::cwriteln!(
                $stream,
                dimmed = true,
                "{:->count$}",
                "",
                count = $crate::term::term_width() - 1
            );
        }
    };
    ($stream:expr, $(fg=$fg:expr,)? $(bg=$bg:expr,)? $(bold=$bold:expr,)? $(dimmed=$dimmed:expr,)? $literal:literal $($arg:tt)*) => {
        use ::unicode_width::UnicodeWidthStr;

        let message = format!($literal $($arg)*);
        const UNTOUCHABLE: usize = 1 + 8; // --[ ... ]--

        // If there's not enough space, just skip printing the extra rule overlay.
        if $crate::term::term_width() > UNTOUCHABLE {
            let max_width = $crate::term::term_width() - UNTOUCHABLE;
            let message = $crate::term::compute_rule_string(&message, max_width);
            let message_width = message.width();

            let is_utf8 = *$crate::term::IS_UTF8;

            if is_utf8 {
                $crate::cwrite!($stream, dimmed = true, "{:─>count$}", "", count = max_width - message_width);
            } else {
                $crate::cwrite!($stream, dimmed = true, "{:->count$}", "", count = max_width - message_width);
            }

            if is_utf8 {
                $crate::cwrite!($stream, dimmed = true, "┨ ");
            } else {
                $crate::cwrite!($stream, dimmed = true, "[ ");
            }
            $crate::cwrite!($stream, $(fg = $fg,)? $(bg = $bg,)? $(bold = $bold,)? $(dimmed = $dimmed,)? "{message}");
            if is_utf8 {
                $crate::cwrite!($stream, dimmed = true, " ┣");
            } else {
                $crate::cwrite!($stream, dimmed = true, " ]");
            }

            if is_utf8 {
                $crate::cwrite!($stream, dimmed = true, "━━");
            } else {
                $crate::cwrite!($stream, dimmed = true, "--");
            }
            $crate::cwriteln!($stream);
        } else {
            $crate::cwriteln_rule!($stream);
        }
    }
}

#[macro_export]
macro_rules! cprintln_rule {
    () => {
        {
            let mut stdout = $crate::term::STDOUT.lock().unwrap();
            $crate::cwriteln_rule!(&mut stdout);
        }
    };
    ($(fg=$fg:expr,)? $(bg=$bg:expr,)? $(bold=$bold:expr,)? $(dimmed=$dimmed:expr,)? $literal:literal $($arg:tt)*) => {
        {
            let mut stdout = $crate::term::STDOUT.lock().unwrap();
            $crate::cwriteln_rule!(&mut *stdout, $(fg=$fg,)? $(bg=$bg,)? $(bold=$bold,)? $(dimmed=$dimmed,)? $literal $($arg)*);
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_rule_string() {
        assert_eq!(compute_rule_string("Hello, world!", 10), "Hello…rld!");
        assert_eq!(compute_rule_string("Hello, world!", 11), "Hello…orld!");
        assert_eq!(compute_rule_string("Hello, world!", 12), "Hello,…orld!");
        assert_eq!(compute_rule_string("Hello, world!", 13), "Hello, world!");
        assert_eq!(compute_rule_string("Hello, world!", 14), "Hello, world!");
    }
}
