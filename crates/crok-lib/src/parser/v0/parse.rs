use std::collections::HashMap;
use std::sync::Arc;

use crate::parser::v0::segment::ScriptV0Block;
use crate::parser::v0::{ESCAPED_MULTILINE, LITERAL_MULTILINE, REGEX_MULTILINE};
use crate::script::*;
use crate::util::ShellBit;
use crate::{output::*, util::shell_split};

use super::segment::{ScriptV0Segment, normalize_segments, segment_script};

#[derive(Default)]
struct OutputPatternBuilder {
    ignore: Vec<OutputPattern>,
    reject: Vec<OutputPattern>,
    patterns: Vec<OutputPattern>,
}

pub fn parse_script(file_name: ScriptFile, script: &str) -> Result<Script, ScriptError> {
    let lines = ScriptLine::parse(file_name.clone(), script);
    let segments = segment_script(true, &mut lines.as_slice())?;
    let normalized = normalize_segments(segments);
    parse_normalized_script_v0(&normalized, file_name)
}

fn parse_normalized_script_v0(
    segments: &[ScriptV0Segment],
    file: ScriptFile,
) -> Result<Script, ScriptError> {
    let commands = parse_normalized_script_v0_commands(segments)?.into();

    Ok(Script {
        commands,
        file,
        includes: Arc::new(HashMap::new()),
    })
}

fn parse_normalized_script_v0_commands(
    segments: &[ScriptV0Segment],
) -> Result<Vec<ScriptBlock>, ScriptError> {
    let mut commands = vec![];

    // Handle the preamble before the first command block

    let preamble_index = segments
        .iter()
        .position(|segment| segment.is_command_block())
        .unwrap_or(segments.len());
    let (preamble, mut segments) = segments.split_at(preamble_index);

    let builder = parse_script_v0_segments(preamble)?;
    if !builder.ignore.is_empty() {
        commands.push(ScriptBlock::GlobalIgnore(OutputPatterns::new(
            builder.ignore,
        )));
    }
    if !builder.reject.is_empty() {
        commands.push(ScriptBlock::GlobalReject(OutputPatterns::new(
            builder.reject,
        )));
    }

    while let Some((command, remaining)) = segments.split_first() {
        if let ScriptV0Segment::SubBlock(_, block_type, args, sub_segments) = command {
            let blocks = parse_normalized_script_v0_commands(sub_segments)?;

            if block_type == "if" {
                let condition = parse_if_condition(command.location().clone(), args)?;
                commands.push(ScriptBlock::If(condition, blocks));
            } else if block_type == "for" {
                if args.len() >= 3 && args[1] == "in" {
                    commands.push(ScriptBlock::For(
                        ForCondition::Env(args[0].to_string(), args[2..].to_vec()),
                        blocks,
                    ));
                } else {
                    return Err(ScriptError::new_with_data(
                        ScriptErrorType::InvalidBlockType,
                        command.location().clone(),
                        format!("for {args:?}"),
                    ));
                }
            } else if block_type == "background" {
                commands.push(ScriptBlock::Background(blocks));
            } else if block_type == "retry" {
                commands.push(ScriptBlock::Retry(blocks));
            } else if block_type == "defer" {
                commands.push(ScriptBlock::Defer(blocks));
            } else if block_type == "ignore" {
                // NOTE: These can exist in the preamble as well.
                let builder = parse_script_v0_segments(sub_segments)?;
                commands.push(ScriptBlock::GlobalIgnore(OutputPatterns::new(
                    builder.patterns,
                )));
            } else if block_type == "reject" {
                // NOTE: These can exist in the preamble as well.
                let builder = parse_script_v0_segments(sub_segments)?;
                commands.push(ScriptBlock::GlobalReject(OutputPatterns::new(
                    builder.patterns,
                )));
            } else if block_type == "pattern" {
            } else {
                return Err(ScriptError::new_with_data(
                    ScriptErrorType::InvalidBlockType,
                    command.location().clone(),
                    block_type.clone(),
                ));
            }

            segments = remaining;
            continue;
        }

        if let ScriptV0Segment::Semi(location, text, args) = command {
            segments = remaining;
            if text == "pattern" {
                commands.push(ScriptBlock::InternalCommand(
                    location.clone(),
                    InternalCommand::Pattern(args[0].to_string(), args[1].to_string()),
                ));
                continue;
            } else if text == "using" {
                if args.len() == 1 && args[0] == "tempdir" {
                    commands.push(ScriptBlock::InternalCommand(
                        location.clone(),
                        InternalCommand::UsingTempdir,
                    ));
                    continue;
                }
                if args.len() == 2 && args[0] == "dir" {
                    commands.push(ScriptBlock::InternalCommand(
                        location.clone(),
                        InternalCommand::UsingDir(args[1].clone(), false),
                    ));
                    continue;
                }
                if args.len() == 3 && args[0] == "new" && args[1] == "dir" {
                    commands.push(ScriptBlock::InternalCommand(
                        location.clone(),
                        InternalCommand::UsingDir(args[2].clone(), true),
                    ));
                    continue;
                }
            }
            if text == "cd" && args.len() == 1 {
                commands.push(ScriptBlock::InternalCommand(
                    location.clone(),
                    InternalCommand::ChangeDir(args[0].clone()),
                ));
                continue;
            }
            if text == "set" && args.len() == 2 {
                commands.push(ScriptBlock::InternalCommand(
                    location.clone(),
                    InternalCommand::Set(args[0].to_string(), args[1].clone()),
                ));
                continue;
            }
            if text == "exit" && args.len() == 1 && args[0] == "script" {
                commands.push(ScriptBlock::InternalCommand(
                    location.clone(),
                    InternalCommand::ExitScript,
                ));
                continue;
            }
            if text == "include" && args.len() == 1 {
                commands.push(ScriptBlock::InternalCommand(
                    location.clone(),
                    InternalCommand::Include(args[0].to_string()),
                ));
                continue;
            }
            return Err(ScriptError::new_with_data(
                ScriptErrorType::InvalidInternalCommand,
                location.clone(),
                format!("{text} {args:?}"),
            ));
        }

        let next_command = remaining
            .iter()
            .position(|segment| segment.is_command_block())
            .unwrap_or(remaining.len());
        let mut pattern;
        (pattern, segments) = remaining.split_at(next_command);

        let location = command.location().clone();
        let mut command = ScriptCommand::new(match command {
            ScriptV0Segment::Block(block) => block.block_type.clone().unwrap_command(),
            _ => unreachable!(),
        });

        if let Some(maybe_meta) = pattern.first()
            && let ScriptV0Segment::Block(block) = maybe_meta
            && block.block_type.is_meta()
        {
            pattern = pattern.split_first().unwrap().1;
            parse_script_v0_meta(block, &mut command)?;
        }

        let builder = parse_script_v0_segments(pattern)?;
        command.pattern = OutputPattern::new_sequence(location, builder.patterns);
        command.pattern.ignore = OutputPatterns::new(builder.ignore);
        command.pattern.reject = OutputPatterns::new(builder.reject);
        commands.push(ScriptBlock::Command(command));
    }
    Ok(commands)
}

fn parse_script_v0_segments(
    segments: &[ScriptV0Segment],
) -> Result<OutputPatternBuilder, ScriptError> {
    let mut builder = OutputPatternBuilder::default();
    for segment in segments {
        parse_script_v0_segment(&mut builder, segment)?;
    }
    Ok(builder)
}

fn parse_script_v0_segment(
    builder: &mut OutputPatternBuilder,
    segment: &ScriptV0Segment,
) -> Result<(), ScriptError> {
    if segment.is_command_block() {
        return Err(ScriptError::new(
            ScriptErrorType::UnsupportedCommandPosition,
            segment.location().clone(),
        ));
    }
    match segment {
        ScriptV0Segment::Block(block) => {
            let mut pattern = block.lines.as_slice();
            while let Some((line, rest)) = pattern.split_first() {
                pattern = rest;
                if line.text() == ESCAPED_MULTILINE {
                    let indent = line.text_untrimmed().find(ESCAPED_MULTILINE).unwrap();
                    while let Some((line, rest)) = pattern.split_first() {
                        pattern = rest;
                        if line.text() == ESCAPED_MULTILINE {
                            break;
                        } else {
                            builder.patterns.push(parse_pattern_line(
                                line.location.clone(),
                                &line.text_untrimmed()[indent.min(line.text_untrimmed().len())..],
                                '!',
                            )?);
                        }
                    }
                } else if line.text() == REGEX_MULTILINE {
                    let indent = line.text_untrimmed().find(REGEX_MULTILINE).unwrap();
                    while let Some((line, rest)) = pattern.split_first() {
                        pattern = rest;
                        if line.text() == REGEX_MULTILINE {
                            break;
                        } else {
                            builder.patterns.push(parse_pattern_line(
                                line.location.clone(),
                                &line.text_untrimmed()[indent.min(line.text_untrimmed().len())..],
                                '?',
                            )?);
                        }
                    }
                } else if line.text() == LITERAL_MULTILINE {
                    let indent = line.text_untrimmed().find(LITERAL_MULTILINE).unwrap();
                    while let Some((line, rest)) = pattern.split_first() {
                        pattern = rest;
                        if line.text() == LITERAL_MULTILINE {
                            break;
                        } else {
                            builder.patterns.push(parse_pattern_line(
                                line.location.clone(),
                                &line.text_untrimmed()[indent.min(line.text_untrimmed().len())..],
                                '"',
                            )?);
                        }
                    }
                } else if line.text() == "!" || line.text() == "?" {
                    builder.patterns.push(parse_pattern_line(
                        line.location.clone(),
                        "",
                        line.first_char().unwrap(),
                    )?);
                } else if line.starts_with("! ") || line.starts_with("? ") {
                    builder.patterns.push(parse_pattern_line(
                        line.location.clone(),
                        &line.text()[2..],
                        line.first_char().unwrap(),
                    )?);
                } else if line.text() == "end" {
                    builder.patterns.push(OutputPattern {
                        pattern: OutputPatternType::End,
                        ignore: Default::default(),
                        reject: Default::default(),
                        location: line.location.clone(),
                    });
                } else if line.text() == "none" {
                    builder.patterns.push(OutputPattern {
                        pattern: OutputPatternType::None,
                        ignore: Default::default(),
                        reject: Default::default(),
                        location: line.location.clone(),
                    });
                } else {
                    return Err(ScriptError::new_with_data(
                        ScriptErrorType::InvalidPattern,
                        line.location.clone(),
                        format!("{:?}", line.text()),
                    ));
                }
            }
        }
        ScriptV0Segment::SubBlock(location, text, args, segments) => {
            if text != "if" && !args.is_empty() {
                return Err(ScriptError::new_with_data(
                    ScriptErrorType::InvalidPattern,
                    location.clone(),
                    format!("{text} {args:?}"),
                ));
            }
            if text == "reject" {
                let next = parse_script_v0_segments(segments)?;
                if !next.ignore.is_empty() || !next.reject.is_empty() {
                    return Err(ScriptError::new(
                        ScriptErrorType::InvalidPattern,
                        location.clone(),
                    ));
                }
                builder.reject.extend(next.patterns);
            } else if text == "ignore" {
                let next = parse_script_v0_segments(segments)?;
                if !next.ignore.is_empty() || !next.reject.is_empty() {
                    return Err(ScriptError::new(
                        ScriptErrorType::InvalidPattern,
                        location.clone(),
                    ));
                }
                builder.ignore.extend(next.patterns);
            } else if text == "if" {
                let condition = parse_if_condition(location.clone(), args)?;
                let new_builder = parse_script_v0_segments(segments)?;
                let pattern = OutputPattern {
                    pattern: OutputPatternType::If(
                        condition,
                        Box::new(OutputPattern::new_sequence(
                            location.clone(),
                            new_builder.patterns,
                        )),
                    ),
                    ignore: OutputPatterns::new(new_builder.ignore),
                    reject: OutputPatterns::new(new_builder.reject),
                    location: location.clone(),
                };
                builder.patterns.push(pattern);
            } else {
                let factory: &dyn Fn(&ScriptLocation, Vec<OutputPattern>) -> OutputPatternType =
                    match text.as_str() {
                        "repeat" => &|location, patterns| {
                            OutputPatternType::Repeat(Box::new(OutputPattern::new_sequence(
                                location.clone(),
                                patterns,
                            )))
                        },
                        "choice" => &|_location, patterns| OutputPatternType::Choice(patterns),
                        "unordered" => {
                            &|_location, patterns| OutputPatternType::Unordered(patterns)
                        }
                        "sequence" => &|_location, patterns| OutputPatternType::Sequence(patterns),
                        "optional" => &|location, patterns| {
                            OutputPatternType::Optional(Box::new(OutputPattern::new_sequence(
                                location.clone(),
                                patterns,
                            )))
                        },
                        "not" => &|location, patterns| {
                            OutputPatternType::Not(Box::new(OutputPattern::new_sequence(
                                location.clone(),
                                patterns,
                            )))
                        },
                        "*" => &|location: &ScriptLocation, patterns| {
                            OutputPatternType::Any(Box::new(OutputPattern::new_sequence(
                                location.clone(),
                                patterns,
                            )))
                        },
                        _ => {
                            return Err(ScriptError::new_with_data(
                                ScriptErrorType::InvalidPattern,
                                location.clone(),
                                text.to_string(),
                            ));
                        }
                    };

                let new_builder = parse_script_v0_segments(segments)?;
                let pattern = OutputPattern {
                    pattern: factory(location, new_builder.patterns),
                    ignore: OutputPatterns::new(new_builder.ignore),
                    reject: OutputPatterns::new(new_builder.reject),
                    location: location.clone(),
                };
                builder.patterns.push(pattern);
            }
        }
        ScriptV0Segment::Semi(location, text, args) => {
            return Err(ScriptError::new_with_data(
                ScriptErrorType::UnsupportedCommandPosition,
                location.clone(),
                format!("{text} {args:?}"),
            ));
        }
    }
    Ok(())
}

fn parse_if_condition(
    location: ScriptLocation,
    args: &[ShellBit],
) -> Result<IfCondition, ScriptError> {
    if args.len() == 1 && args[0] == "true" {
        Ok(IfCondition::True)
    } else if args.len() == 1 && args[0] == "false" {
        Ok(IfCondition::False)
    } else if args.len() == 3 && args[1] == "==" {
        Ok(IfCondition::EnvEq(
            false,
            args[0].to_string(),
            args[2].clone(),
        ))
    } else if args.len() == 3 && args[1] == "!=" {
        Ok(IfCondition::EnvEq(
            true,
            args[0].to_string(),
            args[2].clone(),
        ))
    } else {
        Err(ScriptError::new_with_data(
            ScriptErrorType::InvalidIfCondition,
            location.clone(),
            format!("{args:?}"),
        ))
    }
}

fn parse_pattern_line(
    location: ScriptLocation,
    text: &str,
    line_start: char,
) -> Result<OutputPattern, ScriptError> {
    if text.is_empty() || line_start == '"' {
        return Ok(OutputPattern {
            pattern: OutputPatternType::Literal(text.to_string()),
            ignore: Default::default(),
            reject: Default::default(),
            location,
        });
    }

    let text = text.trim_end();
    let original = text.to_string();

    if line_start == '!' {
        if !text.contains("%") {
            return Ok(OutputPattern {
                pattern: OutputPatternType::Literal(text.to_string()),
                ignore: Default::default(),
                reject: Default::default(),
                location,
            });
        }

        let pattern = GrokPattern::compile(text, original, true).map_err(|e| {
            ScriptError::new_with_data(
                ScriptErrorType::InvalidPattern,
                location.clone(),
                e.to_string(),
            )
        })?;
        Ok(OutputPattern {
            pattern: OutputPatternType::Pattern(Arc::new(pattern)),
            ignore: Default::default(),
            reject: Default::default(),
            location,
        })
    } else if line_start == '?' {
        let text = if text.ends_with('$') {
            text.to_string()
        } else {
            format!(r#"{text}\s*"#)
        };
        let pattern = GrokPattern::compile(&text, original, false).map_err(|e| {
            ScriptError::new_with_data(
                ScriptErrorType::InvalidPattern,
                location.clone(),
                e.to_string(),
            )
        })?;
        Ok(OutputPattern {
            pattern: OutputPatternType::Pattern(Arc::new(pattern)),
            ignore: Default::default(),
            reject: Default::default(),
            location,
        })
    } else {
        unreachable!("Invalid line start: {line_start}");
    }
}

fn parse_script_v0_meta(
    meta_block: &ScriptV0Block,
    command: &mut ScriptCommand,
) -> Result<(), ScriptError> {
    for line in meta_block.lines.iter() {
        let Some(meta_text) = line.text().strip_prefix('%') else {
            continue;
        };
        let words = shell_split(meta_text).map_err(|e| {
            ScriptError::new_with_data(
                ScriptErrorType::InvalidMetaCommand,
                line.location.clone(),
                format!("{e}: {line}", line = line.text()),
            )
        })?;

        if words.is_empty() {
            return Err(ScriptError::new(
                ScriptErrorType::InvalidMetaCommand,
                line.location.clone(),
            ));
        }

        let command_string = words[0].to_string();

        match &*command_string {
            "SET" | "set" => {
                if words.len() == 2 {
                    command.set_var = Some(words[1].to_string());
                } else if words.len() == 3 {
                    command
                        .set_vars
                        .insert(words[1].to_string(), words[2].clone());
                } else {
                    return Err(ScriptError::new(
                        ScriptErrorType::InvalidSetVariable,
                        line.location.clone(),
                    ));
                }
            }
            "EXPECT_FAILURE" | "expect_failure" => {
                command.expect_failure = true;
            }
            "EXIT" | "exit" => {
                if words.len() >= 2 {
                    match &*words[1].to_string() {
                        "any" => {
                            command.exit = CommandExit::Any;
                        }
                        "fail" => {
                            command.exit = CommandExit::AnyFailure;
                        }
                        "timeout" => {
                            command.exit = CommandExit::Timeout;
                        }
                        status_str => {
                            if let Ok(status) = status_str.parse::<i32>() {
                                command.exit = CommandExit::Failure(status);
                            } else {
                                return Err(ScriptError::new(
                                    ScriptErrorType::InvalidExitStatus,
                                    line.location.clone(),
                                ));
                            }
                        }
                    }
                } else {
                    return Err(ScriptError::new(
                        ScriptErrorType::InvalidMetaCommand,
                        line.location.clone(),
                    ));
                }
            }
            "TIMEOUT" | "timeout" => {
                if words.len() >= 2 {
                    let timeout_text = words[1..]
                        .iter()
                        .map(|w| w.to_string())
                        .collect::<Vec<_>>()
                        .join(" ");
                    if let Ok(timeout) = humantime::parse_duration(&timeout_text) {
                        command.timeout = Some(timeout);
                    } else {
                        return Err(ScriptError::new(
                            ScriptErrorType::InvalidMetaCommand,
                            line.location.clone(),
                        ));
                    }
                } else {
                    return Err(ScriptError::new(
                        ScriptErrorType::InvalidMetaCommand,
                        line.location.clone(),
                    ));
                }
            }
            "EXPECT" | "expect" => {
                if words.len() != 3 {
                    return Err(ScriptError::new(
                        ScriptErrorType::InvalidMetaCommand,
                        line.location.clone(),
                    ));
                }

                let key = words[1].to_string();
                let value = words[2].clone();
                command.expect.insert(key, value);
            }
            _ => {
                return Err(ScriptError::new_with_data(
                    ScriptErrorType::InvalidMetaCommand,
                    line.location.clone(),
                    format!("{line:?}"),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::Lines;

    fn parse_pattern(pattern: &str) -> Result<OutputPattern, ScriptError> {
        let lines = ScriptLine::parse(ScriptFile::new("test.cli"), pattern);
        let segments = segment_script(true, &mut lines.as_slice()).unwrap();
        let normalized = normalize_segments(segments);
        Ok(parse_script_v0_segments(&normalized)?
            .patterns
            .first()
            .unwrap()
            .clone())
    }

    fn parse_lines(lines: &str) -> Result<Lines, ScriptError> {
        Ok(Lines::new(
            lines.lines().map(|l| l.to_string()).collect::<Vec<_>>(),
        ))
    }

    #[test]
    fn test_v0_patterns() {
        let patterns = vec![
            parse_pattern("! a\n! b\n! c\n").unwrap(),
            parse_pattern("!!!\na\nb\nc\n!!!\n").unwrap(),
        ];

        let context = ScriptRunContext::default();
        let context = OutputMatchContext::new(&context);
        let output = parse_lines("a\nb\nc\n").unwrap();

        for pattern in patterns {
            let result = pattern.matches(context.clone(), output.clone());
            assert!(result.is_ok());
        }
    }

    #[test]
    fn test_v0_block_pattern() {
        let pattern = r#"
        repeat {
            choice {
    ? pattern1 %{DATA}
    ? pattern2 %{DATA}
    ? pattern3 %{DATA}
            }
        }
        "#;
        let pattern = parse_pattern(pattern).unwrap();
        eprintln!("{pattern:?}");
    }
}
