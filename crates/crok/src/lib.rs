#![doc = include_str!("../README.md")]

use clap::{Parser, ValueEnum};
use std::{path::PathBuf, time::Duration};
use termcolor::Color;

use crok_lib::parser::parse_script_files;
use crok_lib::script::ScriptOutput;
use crok_lib::*;

use script::ScriptRunArgs;

const README: &str = include_str!("../README.md");

fn print_help() {
    // Extract content between markers
    let start_marker = "<!-- clihelp:start -->";
    let end_marker = "<!-- clihelp:end -->";

    let help_content = README
        .find(start_marker)
        .and_then(|start| {
            let content_start = start + start_marker.len();
            README[content_start..]
                .find(end_marker)
                .map(|end| README[content_start..content_start + end].trim())
        })
        .unwrap_or("Help content not found");

    // Print version info first
    cprintln!(bold = true, "crok {}", env!("CARGO_PKG_VERSION"));
    cprintln!();
    cprintln!("crok --help for command-line usage");
    cprintln!();
    cprintln!("Visit https://mmastrac.github.io/crok/ for more help.");
    cprintln!();

    // Render markdown to terminal
    termimad::print_text(help_content);
}

#[derive(Parser, Debug, Clone, Copy, ValueEnum)]
enum DumpFormat {
    Json,
    Toml,
    Yaml,
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the script to run.
    #[arg(value_hint = clap::ValueHint::FilePath)]
    scripts: Vec<PathBuf>,

    /// Delay between steps.
    #[arg(long)]
    delay_steps: Option<u64>,

    /// Ignore mismatched exit codes.
    #[arg(long)]
    ignore_exit_codes: bool,

    /// Ignore failed matches.
    #[arg(long)]
    ignore_matches: bool,

    /// Quiet.
    #[arg(long, short)]
    quiet: bool,

    /// Verbose output
    #[arg(long, short)]
    verbose: bool,

    /// Command timeout in seconds.
    #[arg(long)]
    timeout: Option<f32>,

    /// Show line numbers in command output.
    #[arg(long)]
    show_line_numbers: bool,

    /// The command to run the script with. Default is 'sh -c'.
    #[arg(long)]
    runner: Option<String>,

    /// Dump the script to JSON or TOML.
    #[arg(long)]
    dump: Option<DumpFormat>,

    /// Version (used for shebang only)
    #[arg(long, hide = true)]
    v0: bool,

    /// Show syntax help
    #[arg(long)]
    help_syntax: bool,
}

/// Run the crok CLI with the current process arguments.
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    if args.help_syntax || args.scripts.is_empty() {
        print_help();
        std::process::exit(if args.help_syntax { 0 } else { 1 });
    }

    let script_files = match parse_script_files(&args.scripts) {
        Ok(script_files) => script_files,
        Err(e) => {
            cprintln!(fg = Color::Red, bold = true, "Errors:");
            for e in e {
                cprintln!(fg = Color::Red, " {e}");
            }
            cprintln!();
            cprintln!(dimmed = true, "Run `crok --help-syntax` for syntax help.");
            std::process::exit(1);
        }
    };

    if let Some(format) = args.dump {
        let s = script_files.scripts;
        match format {
            DumpFormat::Json => {
                cprintln!(
                    "{}",
                    serde_json::to_string_pretty(&s).expect("Failed to serialize script files")
                );
            }
            DumpFormat::Yaml => {
                for script in s {
                    cprintln!(
                        "{}",
                        serde_yaml::to_string(&script).expect("Failed to serialize script files")
                    );
                }
            }
            DumpFormat::Toml => {
                for script in s {
                    cprintln!(
                        "{}",
                        toml::to_string_pretty(&script).expect("Failed to serialize script files")
                    );
                }
            }
        }
        return Ok(());
    }

    let mut failed = 0;
    let total = script_files.scripts.len();

    for (script_file, script) in script_files.scripts {
        let args = ScriptRunArgs {
            delay_steps: args.delay_steps,
            ignore_exit_codes: args.ignore_exit_codes,
            ignore_matches: args.ignore_matches,
            runner: args.runner.clone(),
            show_line_numbers: args.show_line_numbers,
            quiet: args.quiet,
            no_color: false,
            verbose: args.verbose,
            global_timeout: args.timeout.map(Duration::from_secs_f32),
            simplified_output: false,
        };

        if args.quiet {
            cprint!("Running ");
            cprint!(fg = Color::Cyan, "{} ... ", script_file);
            match script.run_with_args(args, ScriptOutput::quiet(true)) {
                Ok(_) => cprintln!(fg = Color::Green, "OK"),
                Err(e) => {
                    cprintln!(fg = Color::Red, "FAILED");
                    failed += 1;
                    cprint!(fg = Color::Red, "Error: ");
                    cprintln!("{e}");
                }
            }
        } else if script.run_with_args(args, ScriptOutput::default()).is_err() {
            failed += 1;
        }
    }

    if failed > 0 {
        cprintln!(fg = Color::Red, "{failed}/{total} test(s) failed");
        std::process::exit(1);
    } else {
        cprintln!(fg = Color::Green, "{total} test(s) passed");
    }

    Ok(())
}
