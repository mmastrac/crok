use crate::command::CommandLine;
use crate::parser::v0::ESCAPED_MULTILINE;
use crate::parser::v0::LITERAL_MULTILINE;
use crate::parser::v0::REGEX_MULTILINE;
use crate::script::*;
use crate::util::ShellBit;
use crate::util::shell_split;

#[derive(Debug, Clone, derive_more::IsVariant, derive_more::Unwrap)]
pub enum BlockType {
    /// A command block.
    Command(CommandLine),
    /// Comments and whitespace lines.
    Ineffectual,
    /// Pattern lines.
    Pattern,
    /// Meta lines (`%EXPECT_FAILURE`, `%EXIT`, etc.).
    Meta,
    /// Any (`*`) block
    Any,
}

impl BlockType {
    pub fn is_same_type_as(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (BlockType::Command(_), BlockType::Command(_))
                | (BlockType::Ineffectual, BlockType::Ineffectual)
                | (BlockType::Pattern, BlockType::Pattern)
                | (BlockType::Meta, BlockType::Meta)
                | (BlockType::Any, BlockType::Any)
        )
    }
}

pub struct ScriptV0Block {
    pub location: ScriptLocation,
    pub block_type: BlockType,
    pub lines: Vec<ScriptLine>,
}

impl ScriptV0Block {
    /// Take the current block, replacing with an empty block at the given location.
    pub fn take(&mut self, location: ScriptLocation, block_type: BlockType) -> Self {
        Self {
            location: std::mem::replace(&mut self.location, location),
            block_type: std::mem::replace(&mut self.block_type, block_type),
            lines: std::mem::take(&mut self.lines),
        }
    }

    /// Split the first pattern line from the rest. If not a pattern block,
    /// return None. May leave an empty block if the first line is the only line.
    pub fn split_first(&mut self) -> Option<Self> {
        match self.block_type {
            BlockType::Pattern => {
                let lines = &mut self.lines;
                if lines.is_empty() {
                    debug_assert!(false, "split_first called on empty pattern block");
                    return None;
                }
                let first = lines.remove(0);
                Some(Self {
                    location: first.location.clone(),
                    block_type: BlockType::Pattern,
                    lines: vec![first],
                })
            }
            _ => None,
        }
    }
}

impl std::fmt::Debug for ScriptV0Block {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if f.alternate() {
            let indent = f.width().unwrap_or_default();
            let indent = " ".repeat(indent);
            // HACK: Repurpose width as indent
            // Left-pad by "indent" spaces
            let c = match self.block_type {
                BlockType::Command(_) => "$",
                BlockType::Ineffectual => "#",
                BlockType::Pattern => "",
                BlockType::Meta => "%",
                BlockType::Any => "*",
            };
            writeln!(f, "{indent}:{} {c}[", self.location.line)?;
            for line in &self.lines {
                writeln!(f, "{indent}  {:?}", line.text())?;
            }
            write!(f, "{indent}]")?;
            Ok(())
        } else {
            f.debug_struct("ScriptBlock")
                .field("location", &self.location)
                .field("block_type", &self.block_type)
                .field("lines", &self.lines)
                .finish()
        }
    }
}

/// A segment of a script. This is the first stage of parsing, where we split
/// the script.
pub enum ScriptV0Segment {
    Block(ScriptV0Block),
    SubBlock(ScriptLocation, String, Vec<ShellBit>, Vec<ScriptV0Segment>),
    Semi(ScriptLocation, String, Vec<ShellBit>),
}

impl std::fmt::Debug for ScriptV0Segment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if f.alternate() {
            let indent = f.width().unwrap_or_default();
            let indent_str = " ".repeat(indent);
            // HACK: Indent the segments by using width, but don't print indent here
            match self {
                ScriptV0Segment::Block(block) => writeln!(f, "{block:#indent$?}"),
                ScriptV0Segment::SubBlock(location, text, args, segments) => {
                    writeln!(f, "{indent_str}:{} {text:?}{args:?} {{", location.line)?;
                    for segment in segments {
                        write!(f, "{segment:#indent$?}", indent = indent + 2)?;
                    }
                    writeln!(f, "{indent_str}}}")?;
                    Ok(())
                }
                ScriptV0Segment::Semi(location, text, args) => {
                    writeln!(f, "{indent_str}:{} {text:?}{args:?};", location.line)?;
                    Ok(())
                }
            }
        } else {
            match self {
                ScriptV0Segment::Block(block) => f
                    .debug_struct("Block")
                    .field("location", &block.location)
                    .field("block_type", &block.block_type)
                    .field("lines", &block.lines)
                    .finish(),
                ScriptV0Segment::SubBlock(location, text, args, segments) => f
                    .debug_struct("SubBlock")
                    .field("location", &location)
                    .field("text", &text)
                    .field("args", &args)
                    .field("segments", &segments)
                    .finish(),
                ScriptV0Segment::Semi(location, text, args) => f
                    .debug_struct("Semi")
                    .field("location", &location)
                    .field("text", &text)
                    .field("args", &args)
                    .finish(),
            }
        }
    }
}

impl ScriptV0Segment {
    pub fn is_empty(&self) -> bool {
        match self {
            ScriptV0Segment::Block(block) => block.lines.is_empty(),
            ScriptV0Segment::SubBlock(_, text, _args, segments) => {
                text != "*"
                    && (segments.is_empty() || segments.iter().all(|segment| segment.is_empty()))
            }
            ScriptV0Segment::Semi(..) => false,
        }
    }

    /// Used by wildcard handling.
    pub fn split_first(&mut self) -> Option<Self> {
        match self {
            ScriptV0Segment::Block(block) => block.split_first().map(ScriptV0Segment::Block),
            &mut ScriptV0Segment::SubBlock(ref location, ..) => {
                if self.is_command_block() {
                    None
                } else {
                    Some(std::mem::replace(
                        self,
                        ScriptV0Segment::Block(ScriptV0Block {
                            location: location.clone(),
                            block_type: BlockType::Ineffectual,
                            lines: vec![],
                        }),
                    ))
                }
            }
            ScriptV0Segment::Semi(..) => None,
        }
    }

    /// Returns true if this segment is a command block, or the first block it
    /// contains is a command block. Note that this should only be called on
    /// normalized segments.
    pub fn is_command_block(&self) -> bool {
        match self {
            ScriptV0Segment::Block(block) => block.block_type.is_command(),
            ScriptV0Segment::SubBlock(.., segments) => segments
                .iter()
                .any(|segment: &ScriptV0Segment| segment.is_command_block()),
            ScriptV0Segment::Semi(..) => true,
        }
    }

    pub fn location(&self) -> &ScriptLocation {
        match self {
            ScriptV0Segment::Block(block) => &block.location,
            ScriptV0Segment::SubBlock(location, ..) => location,
            ScriptV0Segment::Semi(location, ..) => location,
        }
    }

    #[allow(dead_code)]
    pub fn last_location(&self) -> &ScriptLocation {
        match self {
            ScriptV0Segment::Block(block) => &block.lines.last().unwrap().location,
            ScriptV0Segment::SubBlock(location, .., segments) => {
                if let Some(last) = segments.last() {
                    last.last_location()
                } else {
                    location
                }
            }
            ScriptV0Segment::Semi(location, ..) => location,
        }
    }
}

/// Split the script into parsing segments. These allow us to more easily parse
/// in later phases because we avoid having to check for block boundaries.
pub fn segment_script(
    top_level: bool,
    lines_slice: &mut &[ScriptLine],
) -> Result<Vec<ScriptV0Segment>, ScriptError> {
    let mut segments = Vec::new();
    let mut current_segment = None;

    fn is_subblock(text: &str) -> Result<Option<(bool, &str, &str)>, ScriptErrorType> {
        // Workaround for missing let chains
        if text.starts_with(|c: char| c.is_alphabetic()) {
            let is_semi = text.ends_with(';');
            let Some(text) = text.strip_suffix(|c: char| c == '{' || c == ';') else {
                return Err(ScriptErrorType::ExpectedBlockOrSemi);
            };
            if let Some((block_type, args)) = text.trim().split_once(char::is_whitespace) {
                Ok(Some((is_semi, block_type.trim(), args.trim())))
            } else {
                Ok(Some((is_semi, text.trim(), "")))
            }
        } else {
            Ok(None)
        }
    }

    let mut lines = lines_slice.iter();
    let orig_slice = *lines_slice;
    let mut multiline_terminator = None;
    while let Some(line) = lines.next() {
        if let Some(terminator) = multiline_terminator {
            if line.text() == terminator {
                multiline_terminator = None;
            }
        } else if line.text() == ESCAPED_MULTILINE {
            multiline_terminator = Some(ESCAPED_MULTILINE);
        } else if line.text() == REGEX_MULTILINE {
            multiline_terminator = Some(REGEX_MULTILINE);
        } else if line.text() == LITERAL_MULTILINE {
            multiline_terminator = Some(LITERAL_MULTILINE);
        }

        if multiline_terminator.is_some() {
            let segment = current_segment.get_or_insert(ScriptV0Block {
                block_type: BlockType::Pattern,
                lines: Vec::new(),
                location: line.location.clone(),
            });
            if !segment.block_type.is_same_type_as(&BlockType::Pattern) {
                segments.push(ScriptV0Segment::Block(
                    segment.take(line.location.clone(), BlockType::Pattern),
                ));
            }
            segment.lines.push(line.clone());
            continue;
        }

        // For commands, we greedily consume all lines until we successfully
        // parse a command (or fail to parse).
        if line.starts_with("$") {
            if let Some(segment) = current_segment.take() {
                segments.push(ScriptV0Segment::Block(segment));
            }
            let mut block_lines = vec![line.clone()];
            let mut command = line.text()[1..].trim().to_string();
            let mut line_count = 1;
            let command = loop {
                match parse_command_line(line.location.clone(), line_count, &command) {
                    Ok(command) => break command,
                    Err(e @ ScriptErrorType::UnclosedQuote)
                    | Err(e @ ScriptErrorType::UnclosedBackslash) => match lines.next() {
                        Some(line) => {
                            block_lines.push(line.clone());
                            command.push('\n');
                            command.push_str(line.text());
                            line_count += 1;
                        }
                        None => {
                            return Err(ScriptError::new(e, line.location.clone()));
                        }
                    },
                    Err(e) => {
                        return Err(ScriptError::new(e, line.location.clone()));
                    }
                }
            };

            segments.push(ScriptV0Segment::Block(ScriptV0Block {
                block_type: BlockType::Command(command),
                lines: block_lines,
                location: line.location.clone(),
            }));
        } else if let Some((is_semi, block_type, args)) =
            is_subblock(line.text()).map_err(|e| ScriptError::new(e, line.location.clone()))?
        {
            if let Some(segment) = current_segment.take() {
                segments.push(ScriptV0Segment::Block(segment));
            }

            let args = shell_split(args).map_err(|_| {
                ScriptError::new_with_data(
                    ScriptErrorType::InvalidBlockArgs,
                    line.location.clone(),
                    format!("{block_type} {args}"),
                )
            })?;

            if is_semi {
                segments.push(ScriptV0Segment::Semi(
                    line.location.clone(),
                    block_type.to_string(),
                    args,
                ));
            } else {
                // Temporaraliy swap from iterator to slice
                let mut rest = lines.as_slice();
                if rest.is_empty() {
                    return Err(ScriptError::new(
                        ScriptErrorType::InvalidBlockEnd,
                        line.location.clone(),
                    ));
                }

                segments.push(ScriptV0Segment::SubBlock(
                    line.location.clone(),
                    block_type.to_string(),
                    args,
                    segment_script(false, &mut rest)?,
                ));
                lines = rest.iter();
            }
        } else if line.text() == "}" {
            // Note that the closing brace is not included in the current
            // segment, we omit these lines from the segment tree.
            if top_level {
                return Err(ScriptError::new(
                    ScriptErrorType::InvalidBlockEnd,
                    line.location.clone(),
                ));
            }
            *lines_slice = lines.as_slice();
            if let Some(segment) = current_segment.take() {
                segments.push(ScriptV0Segment::Block(segment));
            }
            return Ok(segments);
        } else {
            // Split into ineffectual and non-ineffectual lines
            let block_type = if multiline_terminator.is_some() {
                BlockType::Pattern
            } else if line.starts_with("#") || line.is_empty() {
                BlockType::Ineffectual
            } else if line.starts_with("%") {
                BlockType::Meta
            } else if line.starts_with("*") {
                BlockType::Any
            } else {
                BlockType::Pattern
            };

            let segment = current_segment.get_or_insert(ScriptV0Block {
                block_type: block_type.clone(),
                lines: Vec::new(),
                location: line.location.clone(),
            });
            if !segment.block_type.is_same_type_as(&block_type) {
                segments.push(ScriptV0Segment::Block(
                    segment.take(line.location.clone(), block_type),
                ));
            }
            segment.lines.push(line.clone());
        }
    }

    if !top_level {
        return Err(ScriptError::new(
            ScriptErrorType::InvalidBlockEnd,
            orig_slice.last().unwrap().location.clone(),
        ));
    }

    if let Some(segment) = current_segment.take() {
        segments.push(ScriptV0Segment::Block(segment));
    }

    Ok(segments)
}

fn insert_virtual_end_block(location: ScriptLocation, segments: &mut Vec<ScriptV0Segment>) {
    let line = ScriptLine::new(location.file.clone(), location.line - 1, "end");

    segments.push(ScriptV0Segment::Block(ScriptV0Block {
        location: line.location.clone(),
        block_type: BlockType::Pattern,
        lines: vec![line],
    }));
}

/// Remove all ineffectual blocks, and merge consecutive blocks that are of the same type.
pub fn normalize_segments(segments: Vec<ScriptV0Segment>) -> Vec<ScriptV0Segment> {
    let mut new_segments = vec![];
    let mut command_needs_end = false;

    let Some(last_line) = segments.last().map(|segment| segment.location().clone()) else {
        return segments;
    };

    for mut segment in segments {
        if segment.is_command_block() && command_needs_end {
            insert_virtual_end_block(segment.location().clone(), &mut new_segments);
            command_needs_end = false;
        }
        match segment {
            ScriptV0Segment::Block(ref mut block) => {
                debug_assert!(
                    !block.lines.is_empty(),
                    "empty blocks should not exist here"
                );
                if block.block_type.is_ineffectual() {
                    continue;
                }
                if block.block_type.is_command() {
                    command_needs_end = true;
                }
                if let Some(ScriptV0Segment::Block(last_block)) = new_segments.last_mut() {
                    if block.block_type.is_command() {
                        new_segments.push(segment);
                    } else if block.block_type.is_same_type_as(&last_block.block_type) {
                        last_block.lines.extend(std::mem::take(&mut block.lines));
                    } else {
                        new_segments.push(segment);
                    }
                } else {
                    new_segments.push(segment);
                }
            }
            ScriptV0Segment::SubBlock(location, text, args, segments) => {
                let normalized = normalize_segments(segments);
                new_segments.push(ScriptV0Segment::SubBlock(location, text, args, normalized));
            }
            ScriptV0Segment::Semi(location, text, args) => {
                new_segments.push(ScriptV0Segment::Semi(location, text, args));
            }
        }
    }

    // Add a virtual "end" block to the end of the last command block.
    if command_needs_end {
        insert_virtual_end_block(last_line, &mut new_segments);
    }

    // Pass 2: Convert any "any"-type blocks to sub-blocks and steal the next line or non-command subblock.
    let mut i = 0;
    while i < new_segments.len() {
        if let ScriptV0Segment::Block(block) = &mut new_segments[i]
            && block.block_type.is_any()
        {
            let location = block.location.clone();
            new_segments[i] =
                ScriptV0Segment::SubBlock(location.clone(), "*".to_string(), vec![], vec![]);

            if i + 1 < new_segments.len()
                && let Some(first) = new_segments[i + 1].split_first()
            {
                new_segments[i] = ScriptV0Segment::SubBlock(
                    location.clone(),
                    "*".to_string(),
                    vec![],
                    vec![first],
                );
            }
        }
        if new_segments[i].is_empty() {
            new_segments.remove(i);
        } else {
            i += 1;
        }
    }

    new_segments
}

/// This does a light-weight parse of a command-line to most determine the
/// extent of the command-line. The shell is currently responsible for actual
/// validation and running of the command.
///
/// Important rules:
///
///  - Backslashes escape the next character, except within a single-quoted
///    string
///  - Newlines within quoted strings or after a backslash continue the shell
///    command to the next line
///  - Outside of a quoted string, a comment always ends the command-line
///
/// Returns:
///
///  - UnclosedQuote if the command-line is unclosed because of a missing quote
///  - UnclosedBackslash if the command-line is unclosed because of a missing
///    backslash
///  - IllegalShellCommand if the command-line is invalid
///  - BackgroundProcessNotAllowed if the command-line contains a background
///    process
///  - UnsupportedRedirection if the command-line contains an unsupported
///    redirection
pub fn parse_command_line(
    location: ScriptLocation,
    line_count: usize,
    command: &str,
) -> Result<CommandLine, ScriptErrorType> {
    let command_str = command.to_string();
    // Process the accumulated command
    const SEPARATORS: &[&[u8; 2]] = &[b"&&", b"||", b">&"];

    const SEPARATOR_CHARS: &[char] = &['>', '<', '&', '|', ';', '(', ')', '='];

    enum State {
        GroundFirst,
        Ground,
        Separator,
        SingleQuoted,
        DoubleQuoted,
        DoubleQuotedBackslash,
        Backslash,
        Comment,
    }

    let mut state = State::Ground;
    let mut last_char = '\0';

    for c in command.chars() {
        match state {
            State::GroundFirst => {
                if c == '\'' {
                    state = State::SingleQuoted;
                } else if c == '"' {
                    state = State::DoubleQuoted;
                } else if c == '\\' {
                    state = State::Backslash;
                } else if c == '\n' {
                    unreachable!("newline in ground state");
                } else if SEPARATOR_CHARS.contains(&c) {
                    state = State::Separator;
                } else if !c.is_ascii_whitespace() {
                    state = State::Ground;
                }
            }
            State::Ground => {
                if c == '\'' {
                    state = State::SingleQuoted;
                } else if c == '"' {
                    state = State::DoubleQuoted;
                } else if c == '\\' {
                    state = State::Backslash;
                } else if c == '#' {
                    state = State::Comment;
                } else if c == '\n' {
                    unreachable!("newline in ground state");
                } else if c.is_ascii_whitespace() {
                    state = State::GroundFirst;
                }
            }
            State::Separator => {
                let potential = [last_char as u8, c as u8];
                if c.is_ascii() && SEPARATORS.contains(&&potential) {
                    if &potential == b">&" {
                        return Err(ScriptErrorType::UnsupportedRedirection);
                    }
                    state = State::GroundFirst;
                } else {
                    if last_char == '&' {
                        return Err(ScriptErrorType::BackgroundProcessNotAllowed);
                    }
                    if c == '\'' {
                        state = State::SingleQuoted;
                    } else if c == '"' {
                        state = State::DoubleQuoted;
                    } else if c == '\\' {
                        state = State::Backslash;
                    } else if c == '#' {
                        state = State::Comment;
                    } else if c == '\n' {
                        unreachable!("newline in ground state");
                    } else if c.is_ascii_whitespace() {
                        state = State::GroundFirst;
                    }
                }
            }
            State::SingleQuoted => {
                if c == '\'' {
                    state = State::Ground;
                }
            }
            State::DoubleQuoted => {
                if c == '"' {
                    state = State::Ground;
                } else if c == '\\' {
                    state = State::DoubleQuotedBackslash;
                }
            }
            State::DoubleQuotedBackslash => {
                // Treat the next character as "not special"
                state = State::DoubleQuoted;
            }
            State::Backslash => {
                state = State::Ground;
            }
            State::Comment => {
                if c == '\n' {
                    state = State::Ground;
                }
            }
        }
        last_char = c;
    }

    match state {
        State::SingleQuoted | State::DoubleQuoted => Err(ScriptErrorType::UnclosedQuote),
        State::Separator if last_char == '&' => Err(ScriptErrorType::BackgroundProcessNotAllowed),
        State::Separator if last_char != ';' => Err(ScriptErrorType::IllegalShellCommand),
        State::Backslash | State::DoubleQuotedBackslash => Err(ScriptErrorType::UnclosedBackslash),
        State::Comment | State::Ground | State::GroundFirst | State::Separator => {
            Ok(CommandLine::new(command_str, location, line_count))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_segments() {
        let script = r#"
ignore {
  ! pattern
}
$ echo "hello"
ignore {
  ! pattern
}
$ echo "world"
"#;
        let lines = ScriptLine::parse(ScriptFile::new("test"), script);

        let segments = segment_script(true, &mut lines.as_slice()).unwrap();
        eprintln!("{segments:#?}");
        let normalized = normalize_segments(segments);
        eprintln!("{normalized:#?}");
    }

    #[test]
    fn test_parse_command_line() {
        let location = ScriptLocation::new(ScriptFile::new("test"), 1);

        let command = "echo 'hello' && echo 'world'";
        let command = parse_command_line(location.clone(), 1, command).unwrap();
        assert_eq!(command.command, "echo 'hello' && echo 'world'");

        let command = r#"echo "hello\n" && echo 'world'"#;
        let command = parse_command_line(location.clone(), 1, command).unwrap();
        assert_eq!(command.command, r#"echo "hello\n" && echo 'world'"#);

        let command = r#"echo "hello\x1b" && echo 'world'"#;
        let command = parse_command_line(location.clone(), 1, command).unwrap();
        assert_eq!(command.command, r#"echo "hello\x1b" && echo 'world'"#);
    }
}
