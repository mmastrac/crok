mod parse;
mod segment;

pub use parse::parse_script;

const REGEX_MULTILINE: &str = "???";
const ESCAPED_MULTILINE: &str = "!!!";
const LITERAL_MULTILINE: &str = r#"""""#;
