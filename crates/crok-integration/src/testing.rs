use std::path::{Path, PathBuf};

use crok_lib::util::NicePathBuf;

pub struct TestCase {
    pub name: String,
    pub content: String,
    pub expected_output: Option<String>,
    pub expected_output_file: Option<PathBuf>,
    pub path: NicePathBuf,
}

pub fn root_dir() -> PathBuf {
    dunce::canonicalize(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap(),
    )
    .expect("failed to canonicalize tests directory")
}

pub fn tests_dir() -> PathBuf {
    dunce::canonicalize(root_dir().join("tests")).expect("failed to canonicalize tests directory")
}

pub fn load_test_scripts(pattern: Option<&str>) -> Vec<TestCase> {
    let mut scripts = Vec::new();
    for test_dir in std::fs::read_dir(tests_dir())
        .into_iter()
        .flatten()
        .flatten()
    {
        let test_dir_name = test_dir.file_name().to_str().unwrap().to_owned();
        for test in std::fs::read_dir(test_dir.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            let test_name = test.file_name().to_str().unwrap().to_owned();
            if !test_name.ends_with(".cli") {
                continue;
            }
            if let Some(pattern) = pattern
                && !format!("{test_dir_name}/{test_name}").contains(pattern)
            {
                continue;
            }

            let test_content = std::fs::read_to_string(test.path()).unwrap();
            let mut output_file = test.path().with_extension("out");
            if cfg!(windows) {
                let windows_output_file = output_file.with_extension("windows.out");
                if windows_output_file.exists() {
                    output_file = windows_output_file;
                }
            }
            let expected_output = if output_file.exists() {
                Some(std::fs::read_to_string(&output_file).unwrap())
            } else {
                None
            };
            scripts.push(TestCase {
                name: format!("{test_dir_name}/{test_name}"),
                expected_output,
                content: test_content,
                path: NicePathBuf::from(test.path()),
                expected_output_file: Some(output_file),
            });
        }
    }

    scripts.sort_by_key(|test| test.name.clone());

    scripts
}
