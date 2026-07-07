use crate::script::{Script, ScriptError, ScriptErrorType, ScriptFile, ScriptLocation};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

pub mod v0;

#[derive(thiserror::Error, Debug)]
pub enum ScriptReadError {
    #[error("Error reading script file {file}: {error}")]
    ParseError {
        file: ScriptFile,
        error: ScriptError,
    },
    #[error("Error reading script file {file}: {error}")]
    IoError {
        file: ScriptFile,
        error: std::io::Error,
    },
}

#[derive(Default)]
pub struct Scripts {
    pub scripts: BTreeMap<ScriptFile, Script>,
}

const V0_HEADERS: &[&str] = &[
    "#!/usr/bin/env crok --v0",
    "#!/usr/bin/env clitest --v0",
    "#!/usr/bin/env clitest",
];

fn is_v0_header(line: &str) -> bool {
    V0_HEADERS.contains(&line)
}

pub fn parse_script(file: ScriptFile, script: &str) -> Result<Script, ScriptError> {
    let version = script.lines().next().unwrap_or_default();
    if is_v0_header(version) {
        v0::parse_script(file, script)
    } else {
        Err(ScriptError::new(
            ScriptErrorType::InvalidVersion,
            ScriptLocation::new(file, 1),
        ))
    }
}

pub fn parse_script_file(
    parent: Option<ScriptFile>,
    file: ScriptFile,
) -> Result<Script, Vec<ScriptReadError>> {
    let mut errors = Vec::new();
    let path = parent
        .map(|p| Arc::new(p.file.parent().unwrap().join(&*file.file)))
        .unwrap_or(file.file.clone());
    let script_contents = match std::fs::read_to_string(path.as_ref()) {
        Ok(contents) => contents,
        Err(e) => {
            errors.push(ScriptReadError::IoError {
                file: file.clone(),
                error: e,
            });
            return Err(errors);
        }
    };
    let script = parse_script(file.clone(), &script_contents);
    let mut script = match script {
        Ok(script) => script,
        Err(e) => {
            errors.push(ScriptReadError::ParseError {
                file: file.clone(),
                error: e,
            });
            return Err(errors);
        }
    };

    let mut includes = HashMap::new();
    for (location, include_path) in script.includes() {
        // Create a new ScriptFile for the included path
        // Note: This assumes relative paths from the current script's directory
        match parse_script_file(Some(location.file.clone()), ScriptFile::new(&include_path)) {
            Ok(script) => {
                includes.insert(include_path.clone(), script);
            }
            Err(e) => {
                errors.extend(e);
            }
        }
    }

    script.includes = Arc::new(includes);

    if errors.is_empty() {
        Ok(script)
    } else {
        Err(errors)
    }
}

pub fn parse_script_files<T>(files: &[T]) -> Result<Scripts, Vec<ScriptReadError>>
where
    for<'a> &'a T: Into<ScriptFile>,
{
    let mut errors = Vec::new();
    let mut scripts = Scripts::default();

    // Add initial files to the queue
    for file in files {
        let file = file.into();
        let script = match parse_script_file(None, file.clone()) {
            Ok(script) => script,
            Err(e) => {
                errors.extend(e);
                continue;
            }
        };
        scripts.scripts.insert(file, script);
    }

    if errors.is_empty() {
        Ok(scripts)
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_clitest_shebang() {
        let script = "#!/usr/bin/env clitest --v0\n\n$ echo hi\n! hi\n";
        parse_script(ScriptFile::new("test.cli"), script).unwrap();
    }

    #[test]
    fn accepts_clitest_shebang_without_v0_flag() {
        let script = "#!/usr/bin/env clitest\n\n$ echo hi\n! hi\n";
        parse_script(ScriptFile::new("test.cli"), script).unwrap();
    }
}
