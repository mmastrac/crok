use std::{
    borrow::Cow,
    ffi::OsStr,
    path::{Component, Path, PathBuf, Prefix},
};

use keepcalm::SharedGlobalMut;
use serde::Serialize;
use tempfile::TempDir;

static CANONICAL_TEMP_DIR: SharedGlobalMut<PathBuf> = SharedGlobalMut::new_lazy(|| {
    let tmp = if cfg!(target_vendor = "apple") {
        Path::new("/tmp").to_owned()
    } else {
        std::env::temp_dir()
    };
    match dunce::canonicalize(&tmp) {
        Ok(canonical) => canonical,
        Err(_) => tmp,
    }
});

static CANONICAL_CWD: SharedGlobalMut<Option<PathBuf>> = SharedGlobalMut::new_lazy(|| {
    let cwd = std::env::current_dir().ok()?;
    match dunce::canonicalize(&cwd) {
        Ok(canonical) => Some(canonical),
        Err(_) => Some(cwd),
    }
});

static CANONICAL_HOME_DIR: SharedGlobalMut<Option<PathBuf>> = SharedGlobalMut::new_lazy(|| {
    dirs::home_dir().map(|home| dunce::canonicalize(&home).unwrap_or(home))
});

#[derive(Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct NicePathBuf {
    path: PathBuf,
}

impl serde::Serialize for NicePathBuf {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.path.display().to_string())
    }
}

impl<'de> serde::Deserialize<'de> for NicePathBuf {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(Self::new(&s))
    }
}

impl From<&'_ NicePathBuf> for NicePathBuf {
    fn from(path: &NicePathBuf) -> Self {
        path.clone()
    }
}

impl From<&'_ Path> for NicePathBuf {
    fn from(path: &Path) -> Self {
        NicePathBuf::new(path)
    }
}

impl AsRef<Path> for NicePathBuf {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl NicePathBuf {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn exists(&self) -> std::io::Result<bool> {
        std::fs::exists(&self.path)
    }

    pub fn join(&self, other: impl AsRef<Path>) -> Self {
        Self {
            path: self.path.join(other.as_ref()),
        }
    }

    pub fn create_dir_all(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.path)
    }

    pub fn remove_dir_all(&self) -> std::io::Result<()> {
        std::fs::remove_dir_all(&self.path)
    }

    pub fn parent(&self) -> Option<NicePathBuf> {
        self.path.parent().map(NicePathBuf::new)
    }

    pub fn cwd() -> NicePathBuf {
        let cwd = std::env::current_dir().expect("Couldn't get current directory");
        cwd.into()
    }

    /// Returns a string that can be used in the environment to refer to this
    /// path.
    ///
    /// In the case where this path may be accessed via multiple routes, we will
    /// choose the shortest (ie: /tmp on macOS rather than /private/tmp).
    pub fn env_string(&self) -> String {
        let path = &self.path;
        let canonical = canonicalize_path(path);
        if cfg!(target_vendor = "apple") {
            if let Ok(tmp) = canonical.strip_prefix(CANONICAL_TEMP_DIR.read()) {
                format!("/tmp/{}", tmp.display())
            } else {
                canonical.display().to_string()
            }
        } else {
            canonical.display().to_string()
        }
    }
}

impl From<PathBuf> for NicePathBuf {
    fn from(path: PathBuf) -> Self {
        Self { path }
    }
}

impl From<String> for NicePathBuf {
    fn from(path: String) -> Self {
        Self {
            path: PathBuf::from(path),
        }
    }
}

impl std::fmt::Display for NicePathBuf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write_pretty_path(false, &self.path, f)
    }
}

impl std::fmt::Debug for NicePathBuf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write_pretty_path(true, &self.path, f)
    }
}

pub struct NiceTempDir {
    path: TempDir,
}

impl Default for NiceTempDir {
    fn default() -> Self {
        Self::new()
    }
}

impl NiceTempDir {
    pub fn new() -> Self {
        let path = if cfg!(target_vendor = "apple") {
            tempfile::Builder::new()
                .tempdir_in("/tmp")
                .expect("Couldn't create tempdir")
        } else {
            tempfile::tempdir().expect("Couldn't create tempdir")
        };
        debug_assert!(path.path().is_absolute());
        debug_assert!(matches!(std::fs::exists(path.path()), Ok(true)));
        Self { path }
    }

    pub fn exists(&self) -> Result<bool, std::io::Error> {
        std::fs::exists(self.path.path())
    }

    pub fn remove_dir_all(self) -> std::io::Result<()> {
        self.path.close()
    }

    pub fn join(&self, other: impl AsRef<Path>) -> NicePathBuf {
        NicePathBuf::new(self.path.path().join(other.as_ref()))
    }

    pub fn file_name(&self) -> Option<&OsStr> {
        self.path.path().file_name()
    }

    pub fn env_string(&self) -> String {
        NicePathBuf::from(self).env_string()
    }
}

impl std::fmt::Display for NiceTempDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", NicePathBuf::new(self.path.path()))
    }
}

impl From<&'_ NiceTempDir> for NicePathBuf {
    fn from(tempdir: &NiceTempDir) -> Self {
        NicePathBuf::new(tempdir.path.path())
    }
}

/// Best effort to canonicalize a path.
fn canonicalize_path(path: &Path) -> Cow<'_, Path> {
    if let Ok(path) = dunce::canonicalize(path) {
        return path.into();
    }

    let mut components = path.components();
    let Some(last) = components.next_back() else {
        return path.into();
    };

    let mut rest = PathBuf::from(last.as_os_str());

    // Walk up the path, canonicalizing each component and taking the first
    // component that exists.
    let mut path = path;
    while let Some(parent) = path.parent() {
        if let Ok(mut path) = dunce::canonicalize(parent) {
            for component in rest.components() {
                match component {
                    Component::ParentDir => {
                        if let Some(parent) = path.parent() {
                            path = parent.to_path_buf();
                        }
                    }
                    Component::CurDir => {}
                    _ => {
                        path = path.join(component.as_os_str());
                    }
                }
            }
            return path.into();
        }

        path = parent;
        let mut components = path.components();
        let Some(last) = components.next_back() else {
            return path.into();
        };

        rest = PathBuf::from(last.as_os_str()).join(rest);
    }

    path.into()
}

fn write_pretty_path(
    debug: bool,
    path: &Path,
    f: &mut std::fmt::Formatter<'_>,
) -> std::fmt::Result {
    let tmp = &*CANONICAL_TEMP_DIR.read();
    let home = &*CANONICAL_HOME_DIR.read();
    let cwd = &*CANONICAL_CWD.read();

    let mut canon_path = canonicalize_path(path);

    // On Apple, we can strip the /private prefix from the path for display purposes
    if cfg!(target_vendor = "apple")
        && canon_path.is_absolute()
        && let Ok(without_private) = canon_path.strip_prefix("/private")
    {
        canon_path = Path::new("/").join(without_private).into();
    }

    // If the path is relative, we can try strip the cwd from its canonical
    // version to eliminate any relative paths.
    if let Some(cwd) = cwd
        && let Ok(path) = canon_path.strip_prefix(cwd)
    {
        if debug {
            write_debug_path(f, path)?;
        } else {
            #[cfg(windows)]
            write!(f, ".\\{}", path.display())?;
            #[cfg(not(windows))]
            write!(f, "./{}", path.display())?;
        }
        return Ok(());
    }

    // Unlikely, but just print the path if we're not on unix or windows
    if !cfg!(unix) && !cfg!(windows) {
        if debug {
            write_debug_path(f, path)?;
        } else {
            write!(f, "{}", path.display())?;
        }
        return Ok(());
    }

    // If the path is in tmp, try to prettify it
    if let Ok(path) = canon_path.strip_prefix(tmp) {
        if cfg!(unix) {
            let path = Path::new("/tmp").join(path);
            if debug {
                write_debug_path(f, &path)?;
            } else {
                write!(f, "{}", path.display())?;
            }
        } else if cfg!(windows) {
            let path = Path::new("%TEMP%").join(path);
            if debug {
                write_debug_path(f, &path)?;
            } else {
                write!(f, "{}", path.display())?;
            }
        }
        return Ok(());
    }

    // Skip out here in debug mode
    if debug {
        // On Windows, we can strip the \\?\ prefix from the path for display purposes
        if cfg!(windows)
            && let Some(Component::Prefix(prefix)) = canon_path.components().next()
        {
            // This is a backslash explosion in debug mode...
            if let Prefix::VerbatimDisk(_) = prefix.kind() {
                return f.write_str(&format!("<{}>", canon_path.display()).replace(r"\\?\", ""));
            }
        }

        write_debug_path(f, &canon_path)?;
        return Ok(());
    }

    // If the path is in home, try to prettify it
    if let Some(home) = home
        && let Ok(path) = canon_path.strip_prefix(home)
    {
        if cfg!(unix) {
            write!(f, "~/{}", path.display())?;
        } else if cfg!(windows) {
            write!(f, "%USERPROFILE%\\{}", path.display())?;
        }
        return Ok(());
    }

    // On Windows, we can strip the \\?\ prefix from the path for display purposes
    if cfg!(windows)
        && let Some(Component::Prefix(prefix)) = canon_path.components().next()
        && let Prefix::VerbatimDisk(_) = prefix.kind()
    {
        return write!(
            f,
            "{}",
            canon_path.display().to_string().replace(r"\\?\", "")
        );
    }

    write!(f, "{}", canon_path.display())
}

fn write_debug_path(f: &mut std::fmt::Formatter<'_>, path: &Path) -> std::fmt::Result {
    if cfg!(windows) {
        write!(f, "<{}>", path.display())
    } else {
        write!(f, "{path:?}")
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, derive_more::Error, derive_more::Display)]
pub enum ShellParseError {
    #[display("unmatched quote ({_0})")]
    UnmatchedQuote(#[error(not(source))] char),
    #[display("invalid hex escape ({_0})")]
    InvalidHexEscape(#[error(not(source))] char),
}

/// A single bit of a shell-ish string.
#[derive(derive_more::Debug, Clone, Hash, Eq, PartialEq, PartialOrd, Ord)]
pub enum ShellBit {
    /// A literal string that does not participate in expansion. Comes from
    /// `'string'`.
    #[debug("{_0:?}")]
    Literal(String),
    /// A string that is (possibly) quoted and participates in expansion. Comes
    /// from `"string"` or `string`.
    #[debug("{_0:?}")]
    Quoted(String),
}

impl PartialEq<str> for ShellBit {
    fn eq(&self, other: &str) -> bool {
        match self {
            ShellBit::Literal(s) => s == other,
            ShellBit::Quoted(s) => s == other,
        }
    }
}

impl PartialEq<&'_ str> for ShellBit {
    fn eq(&self, other: &&str) -> bool {
        match self {
            ShellBit::Literal(s) => s == other,
            ShellBit::Quoted(s) => s == other,
        }
    }
}

impl std::fmt::Display for ShellBit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShellBit::Literal(s) => f.write_str(s),
            ShellBit::Quoted(s) => f.write_str(s),
        }
    }
}

impl Serialize for ShellBit {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            ShellBit::Literal(s) => serializer.serialize_str(s),
            ShellBit::Quoted(s) => serializer.serialize_str(s),
        }
    }
}

/// Split a shell-ish string into a vector of strings.
pub fn shell_split(input: &str) -> Result<Vec<ShellBit>, ShellParseError> {
    let mut result = Vec::new();
    let mut in_string = None;
    let mut in_escape = false;
    let mut in_hex_escape = 0;
    let mut hex_accum = 0;
    let mut accum = String::new();

    for c in input.chars() {
        match in_hex_escape {
            2 => {
                in_hex_escape = 1;
                if c.is_ascii_hexdigit() {
                    hex_accum = c.to_digit(16).unwrap();
                    continue;
                } else {
                    return Err(ShellParseError::InvalidHexEscape(c));
                }
            }
            1 => {
                in_hex_escape = 0;
                if c.is_ascii_hexdigit() {
                    hex_accum = hex_accum * 16 + c.to_digit(16).unwrap();
                    accum.push(char::from_u32(hex_accum).unwrap());
                    continue;
                } else {
                    return Err(ShellParseError::InvalidHexEscape(c));
                }
            }
            _ => {}
        }

        if in_escape {
            in_escape = false;
            match c {
                // alert (BEL)
                'a' => accum.push('\x07'),
                // backspace
                'b' => accum.push('\x08'),
                // form feed
                'f' => accum.push('\x0c'),
                // new line
                'n' => accum.push('\n'),
                // carriage return
                'r' => accum.push('\r'),
                // horizontal tab
                't' => accum.push('\t'),
                // vertical tab
                'v' => accum.push('\x0b'),
                // escape
                'e' => accum.push('\x1b'),
                // null
                '0' => accum.push('\0'),

                '"' => accum.push('"'),
                'x' => in_hex_escape = 2,
                _ => {
                    accum.push('\\');
                    accum.push(c);
                }
            }
            continue;
        }

        if let Some(string_char) = in_string {
            if string_char == '\'' {
                if c == string_char {
                    in_string = None;
                    result.push(ShellBit::Literal(std::mem::take(&mut accum)));
                } else {
                    accum.push(c);
                }
            } else if c == '\\' {
                in_escape = true;
            } else if c == string_char {
                in_string = None;
                if c == '"' {
                    result.push(ShellBit::Quoted(std::mem::take(&mut accum)));
                }
            } else {
                accum.push(c);
            }
        } else if c == '\\' {
            in_escape = true;
        } else if c == '"' || c == '\'' {
            in_string = Some(c);
        } else if c == ' ' {
            if accum.is_empty() {
                continue;
            }
            result.push(ShellBit::Quoted(std::mem::take(&mut accum)));
        } else {
            accum.push(c);
        }
    }
    if let Some(string_char) = in_string {
        return Err(ShellParseError::UnmatchedQuote(string_char));
    }

    if !accum.is_empty() {
        result.push(ShellBit::Quoted(std::mem::take(&mut accum)));
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn test_nice_path_buf_tmp_unix() {
        let path = NicePathBuf::new(Path::new("/tmp/hello.world"));

        assert_eq!("/tmp/hello.world", format!("{path}"));
        assert_eq!("\"/tmp/hello.world\"", format!("{path:?}"));

        let path = NicePathBuf::new(Path::new("//tmp//hello.world"));

        assert_eq!("/tmp/hello.world", format!("{path}"));
        assert_eq!("\"/tmp/hello.world\"", format!("{path:?}"));

        let path = NicePathBuf::new(Path::new("//does-not-exist-anywhere/..//tmp//hello.world"));

        assert_eq!("/tmp/hello.world", format!("{path}"));
        assert_eq!("\"/tmp/hello.world\"", format!("{path:?}"));

        let path = NicePathBuf::new(
            Path::new("/tmp")
                .canonicalize()
                .unwrap()
                .join("hello.world"),
        );

        assert_eq!("/tmp/hello.world", format!("{path}"));
        assert_eq!("\"/tmp/hello.world\"", format!("{path:?}"));

        // Test partial canonicalization
        let temp_dir = NiceTempDir::new();
        let path = temp_dir.join("a/b/c/d");

        let name = temp_dir.file_name().unwrap().to_string_lossy();

        assert_eq!(format!("/tmp/{name}/a/b/c/d"), format!("{}", path));
        assert_eq!(format!("\"/tmp/{name}/a/b/c/d\""), format!("{:?}", path));
    }

    #[cfg(windows)]
    #[test]
    fn test_nice_path_buf_tmp_windows() {
        let tmp = std::env::temp_dir();
        let tmp = tmp.join("hello.world");

        let path = NicePathBuf::new(&tmp);

        assert_eq!(r"%TEMP%\hello.world", format!("{}", path));
        assert_eq!(r"<%TEMP%\hello.world>", format!("{:?}", path));

        let path = NicePathBuf::new(
            &std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join("hello.world"),
        );

        assert_eq!(r"%TEMP%\hello.world", format!("{}", path));
        assert_eq!(r"<%TEMP%\hello.world>", format!("{:?}", path));

        let path = NicePathBuf::new(r#"C:\directory"#);

        assert_eq!(r"C:\directory", format!("{}", path));
        assert_eq!(r"<C:\directory>", format!("{:?}", path));
    }

    #[test]
    fn test_shell_split() {
        assert_eq!(format!("{:?}", shell_split("").unwrap()), r#"[]"#);
        assert_eq!(format!("{:?}", shell_split("a").unwrap()), r#"["a"]"#);
        assert_eq!(
            format!("{:?}", shell_split("a b").unwrap()),
            r#"["a", "b"]"#
        );
        assert_eq!(
            format!("{:?}", shell_split("a b c").unwrap()),
            r#"["a", "b", "c"]"#
        );
        assert_eq!(
            format!("{:?}", shell_split("a 'b' c").unwrap()),
            r#"["a", "b", "c"]"#
        );
        assert_eq!(
            format!("{:?}", shell_split("a 'b c' d").unwrap()),
            r#"["a", "b c", "d"]"#
        );
        assert_eq!(
            format!("{:?}", shell_split(r#"a "b" c"#).unwrap()),
            r#"["a", "b", "c"]"#
        );
        assert_eq!(
            format!("{:?}", shell_split(r#"a "b c" d"#).unwrap()),
            r#"["a", "b c", "d"]"#
        );
        assert_eq!(
            format!("{:?}", shell_split(r#"a "b\"c" d"#).unwrap()),
            r#"["a", "b\"c", "d"]"#
        );
        assert_eq!(
            format!("{:?}", shell_split(r#"a "b\'c" d"#).unwrap()),
            r#"["a", "b\\'c", "d"]"#
        );
        assert_eq!(
            format!("{:?}", shell_split(r#"a "b\nc" d"#).unwrap()),
            r#"["a", "b\nc", "d"]"#
        );
        assert_eq!(
            format!("{:?}", shell_split(r#"a "a\\b" d"#).unwrap()),
            r#"["a", "a\\\\b", "d"]"#
        );
        assert_eq!(
            format!("{:?}", shell_split(r#"a 'a\\b' d"#).unwrap()),
            r#"["a", "a\\\\b", "d"]"#
        );
    }

    #[test]
    fn test_shell_split_errors() {
        assert_eq!(
            shell_split("a 'b").unwrap_err(),
            ShellParseError::UnmatchedQuote('\'')
        );
        assert_eq!(
            shell_split("a \"b c").unwrap_err(),
            ShellParseError::UnmatchedQuote('"')
        );
        assert_eq!(
            shell_split("a '").unwrap_err(),
            ShellParseError::UnmatchedQuote('\'')
        );
    }
}
