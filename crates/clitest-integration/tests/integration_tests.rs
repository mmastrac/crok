use std::time::Instant;

use clitest_lib::{
    cprint, cprintln, cprintln_rule, term::Color, try_run_file_captured, util::NicePathBuf,
};

use clitest_integration::testing::{TestCase, load_test_scripts, root_dir, tests_dir};

fn main() {
    let root = root_dir();
    std::env::set_current_dir(&root)
        .unwrap_or_else(|_| panic!("failed to set current directory to {root:?}"));

    let mut total = 0;
    let mut failed = 0;
    cprintln!();

    let tests = load_test_scripts(std::env::args().nth(1).as_deref());

    cprint!("Running ");
    cprint!(fg = Color::Yellow, "{}", tests.len());
    cprint!(" test(s) from ");
    cprint!(
        fg = Color::Cyan,
        "<workspace>/{}/",
        NicePathBuf::from(tests_dir())
    );
    cprintln!();

    let mut failed_tests = Vec::new();

    for test in tests {
        let is_fail = test.path.to_string().contains("-fail");
        cprint!("Running ");
        cprint!(fg = Color::Green, "{}", test.name);
        cprint!(" ... ");

        total += 1;
        let start = Instant::now();

        match try_run_file_captured(test.path.as_ref()) {
            Err(e) => {
                if is_fail {
                    cprint!(fg = Color::Green, "✅ OK ({})", e.error);
                    if !check_output(
                        &test,
                        if e.output.is_empty() {
                            e.error
                        } else {
                            e.output
                        },
                    ) {
                        failed += 1;
                    }
                } else {
                    cprint!(fg = Color::Red, "❌ FAIL");
                    failed += 1;
                    cprint!(fg = Color::Red, " {}", e.error);
                    failed_tests.push(test);
                }
            }
            Ok(output) => {
                if is_fail {
                    cprint!(fg = Color::Red, "❌ FAIL (expected a failure)");
                    failed += 1;
                    failed_tests.push(test);
                } else {
                    cprint!(fg = Color::Green, "✅ OK");
                    if !check_output(&test, output) {
                        failed += 1;
                    }
                }
            }
        }

        let duration = start.elapsed();
        cprintln!(dimmed = true, " ({:.2}s)", duration.as_secs_f64());
    }

    for test in failed_tests {
        cprintln!();
        cprintln_rule!();
        cprint!("Re-running failed test ");
        cprint!(fg = Color::Green, "{}", test.name);
        cprintln!(" ... ");
        cprintln_rule!();
        match clitest_lib::try_run_file_captured(test.path.as_ref()) {
            Ok(output) => cprintln!("{}", output),
            Err(e) => {
                if !e.output.is_empty() {
                    cprintln!("{}", e.output);
                }
                cprintln!(fg = Color::Red, "{}", e.error);
            }
        }
        cprintln_rule!();
    }

    cprintln!();
    cprintln!(
        fg = if failed > 0 { Color::Red } else { Color::White },
        dimmed = true,
        "{} tests run, {} failed",
        total,
        failed
    );
    cprintln!();

    if failed > 0 {
        std::process::exit(1);
    }
}

/// Munge the output to make it easier to compare.
fn munge_output(root: &str, s: &str) -> String {
    let tmp = dunce::canonicalize(std::env::temp_dir())
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();

    let apple_path = tmp.strip_prefix("/private");
    let tmps = if tmp != "/tmp" {
        if let Some(apple_path) = apple_path {
            vec![apple_path, &tmp, "/tmp"]
        } else if cfg!(windows) {
            vec!["%TEMP%", &tmp, r"\tmp"]
        } else {
            vec![&tmp, "/tmp"]
        }
    } else {
        vec![tmp.as_str()]
    };

    // Replace any line that starts with "───" with "---"
    let mut output = String::new();
    // On Windows, use \ for everything, then replace back to / for final test
    #[cfg(windows)]
    let s = &s.replace('/', r"\");
    #[cfg(windows)]
    let s = s.replace(r"\\?\", "");
    for line in s.lines() {
        munge_line(root, &tmps, &mut output, line);
    }
    if cfg!(windows) {
        output = output.replace("\\", "/");
    }
    output
}

fn munge_line(root: &str, tmp: &[&str], output: &mut String, line: &str) {
    // Normalize ASCII fallbacks so expected output matches regardless of UTF-8 locale.
    let line = line
        .replace("[X] FAIL", "❌ FAIL")
        .replace("[X]-", "❌-")
        .replace("[*] OK", "✅ OK")
        .replace(" -> ", " → ");

    // Windows/Unix differs here
    #[cfg(windows)]
    let line = line.replace("exit code", "exit status");
    // Windows kills the tree via the Job object, so there is no signal detail;
    // both platforms still report "killed".
    #[cfg(windows)]
    let line = line.replace("; signal: 15 (SIGTERM)", "");

    if line.contains("<ignore>") {
        output.push_str("<ignore>\n");
    } else {
        let line = line.replace(
            "~/",
            &format!("{}/", std::env::var("HOME").unwrap_or_default()),
        );
        let line = line.replace(root, "<root>");
        for tmp in tmp {
            if line.contains(tmp) {
                munge_tmp(tmp, output, &line);
                output.push('\n');
                return;
            }
        }
        output.push_str(&line);
        output.push('\n');
    }
}

fn munge_tmp(tmp: &str, output: &mut String, line: &str) {
    let tmp_char = |c: char| !c.is_alphanumeric() && c != '_' && c != '-' && c != '.';
    let sep = if cfg!(windows) { '\\' } else { '/' };

    // Replace /tmp or /tmp/<filename> with <tmp>
    let tmp_path = line.split_once(tmp).unwrap().1;
    let tmp_path = if tmp_path.is_empty() || tmp_path.chars().next().unwrap() != sep {
        None
    } else {
        tmp_path[1..].split(tmp_char).next()
    };

    if let Some(tmp_path) = tmp_path {
        output.push_str(
            line.replace(format!("{tmp}{sep}{tmp_path}").as_str(), "<tmp>")
                .as_str(),
        );
    } else {
        output.push_str(line.replace(tmp, "<tmp>").as_str());
    }
}

fn check_output(test: &TestCase, output: String) -> bool {
    let root = &NicePathBuf::from(test.path.as_ref().parent().unwrap()).to_string();
    let b = munge_output(root, &output);

    if std::env::var("UPDATE_TESTS").is_ok() {
        if let Some(expected_output_file) = &test.expected_output_file {
            std::fs::write(expected_output_file, &b).unwrap();
        }
    }

    if let Some(expected_output) = &test.expected_output {
        let a = munge_output(root, expected_output);
        if a == b {
            return true;
        }
        cprintln!();
        cprintln!(fg = Color::Red, "⚠️  Contents differ for {}!", test.path);
        cprintln_rule!();
        let comparison = pretty_assertions::StrComparison::new(&a, &b);
        println!("{comparison}");
        cprintln_rule!();
        cprintln!("\nOriginal output before munge (root = {root:?}):");
        cprintln_rule!();
        cprintln!("{}", output);
        cprintln_rule!();
        false
    } else {
        true
    }
}
