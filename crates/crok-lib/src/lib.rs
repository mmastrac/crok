//! This crate provides the core functionality for the `crok` crate as a library.

pub mod command;
pub mod failure;
pub mod output;
pub mod parser;
pub mod script;
pub mod term;
pub mod util;

use std::path::Path;

use script::{ScriptFile, ScriptOutput, ScriptRunArgs, ScriptRunError};

/// Error returned by [`try_run_captured`] and [`try_run_file_captured`].
pub struct RunError {
    pub error: String,
    pub output: String,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.error)
    }
}

impl std::fmt::Debug for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.error)
    }
}

fn make_args() -> ScriptRunArgs {
    ScriptRunArgs {
        quiet: true,
        no_color: true,
        simplified_output: true,
        ..Default::default()
    }
}

fn execute(parsed: &script::Script, output: ScriptOutput) -> Result<(), ScriptRunError> {
    parsed.run_with_args(make_args(), output)
}

fn get_inline_file() -> ScriptFile {
    ScriptFile::new(std::env::current_dir().unwrap().join("<inline>"))
}

/// Parse and run a crok script string. Output goes to stdout. Panics on failure.
pub fn run(script: &str) {
    let file = get_inline_file();
    let parsed =
        parser::parse_script(file, script).unwrap_or_else(|e| panic!("crok parse error: {e}"));
    let output = ScriptOutput::no_color();
    execute(&parsed, output).unwrap_or_else(|e| panic!("crok failed: {e}"));
}

/// Parse and run a crok script string. Output goes to stdout. Panics on failure.
pub fn run_with_path(path: impl AsRef<Path>, script: &str) {
    let file = ScriptFile::new(path);
    let parsed =
        parser::parse_script(file, script).unwrap_or_else(|e| panic!("crok parse error: {e}"));
    let output = ScriptOutput::no_color();
    execute(&parsed, output).unwrap_or_else(|e| panic!("crok failed: {e}"));
}

/// Parse and run a crok script string. Returns captured output. Panics on failure.
pub fn run_captured(script: &str) -> String {
    match try_run_captured(script) {
        Ok(output) => output,
        Err(e) => panic!("crok failed: {}\n\nOutput:\n{}", e.error, e.output),
    }
}

/// Parse and run a crok script string. Returns captured output. Panics on failure.
pub fn run_with_path_captured(
    name: &str,
    line: usize,
    path: impl AsRef<Path>,
    script: &str,
) -> String {
    let file =
        ScriptFile::new_with_line(dunce::canonicalize(path.as_ref()).unwrap().join(name), line);
    let parsed = match parser::parse_script(file, script) {
        Ok(s) => s,
        Err(e) => panic!("crok parse error: {e}"),
    };
    let output = ScriptOutput::quiet(true);
    match execute(&parsed, output.clone()) {
        Ok(()) => output.take_buffer(),
        Err(e) => panic!("crok failed: {e}\n\nOutput:\n{}", output.take_buffer()),
    }
}

/// Parse and run a crok script string. Returns `Ok(output)` on success,
/// or `Err(RunError)` with both the error message and captured output on failure.
pub fn try_run_captured(script: &str) -> Result<String, RunError> {
    let file = get_inline_file();
    let parsed = match parser::parse_script(file, script) {
        Ok(s) => s,
        Err(e) => {
            return Err(RunError {
                error: e.to_string(),
                output: String::new(),
            });
        }
    };
    let output = ScriptOutput::quiet(true);
    match execute(&parsed, output.clone()) {
        Ok(()) => Ok(output.take_buffer()),
        Err(e) => Err(RunError {
            error: e.to_string(),
            output: output.take_buffer(),
        }),
    }
}

/// Parse and run a crok script file. Output goes to stdout. Panics on failure.
pub fn run_file(path: impl AsRef<Path>) {
    let file = ScriptFile::new(path);
    let parsed = parser::parse_script_file(None, file)
        .unwrap_or_else(|e| panic!("crok parse error: {:?}", e));
    let output = ScriptOutput::no_color();
    execute(&parsed, output).unwrap_or_else(|e| panic!("crok failed: {e}"));
}

/// Parse and run a crok script file. Returns captured output. Panics on failure.
pub fn run_file_captured(path: impl AsRef<Path>) -> String {
    match try_run_file_captured(path) {
        Ok(output) => output,
        Err(e) => panic!("crok failed: {}\n\nOutput:\n{}", e.error, e.output),
    }
}

/// Parse and run a crok script file. Returns `Ok(output)` on success,
/// or `Err(RunError)` with the error message and captured output on failure.
pub fn try_run_file_captured(path: impl AsRef<Path>) -> Result<String, RunError> {
    let file = ScriptFile::new(path);
    let parsed = match parser::parse_script_file(None, file) {
        Ok(s) => s,
        Err(e) => {
            let msg = e
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            return Err(RunError {
                error: msg,
                output: String::new(),
            });
        }
    };
    let output = ScriptOutput::quiet(true);
    match execute(&parsed, output.clone()) {
        Ok(()) => Ok(output.take_buffer()),
        Err(e) => Err(RunError {
            error: e.to_string(),
            output: output.take_buffer(),
        }),
    }
}

/// Generate `#[test]` functions from inline crok scripts. The `PWD` for the
/// script is set to the current directory, which for `cargo test` is the root
/// of the crate.
///
/// ```rust
/// use crok_lib::crok;
///
/// crok!(my_test, r#"
/// $ echo hello
/// ! hello
/// "#);
/// ```
#[macro_export]
macro_rules! crok {
    ($name:ident, $script:expr) => {
        #[test]
        fn $name() {
            let output = $crate::run_with_path_captured(
                stringify!($name),
                line!() as _,
                std::env::current_dir().unwrap(),
                &format!("#!/usr/bin/env crok --v0\n{}", $script),
            );
            eprintln!("{output}");
        }
    };
}

crok!(
    test_run_macro,
    r#"
$ echo $PWD
*
cd "src/parser";
$ echo $PWD
*
"#
);
