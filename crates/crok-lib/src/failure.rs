use std::borrow::Cow;

use crate::{
    output::{Line, OutputPatternType},
    script::{IfCondition, ScriptLocation},
};

#[derive(Clone, Debug, thiserror::Error, derive_more::Display, PartialEq, Eq)]
#[display("{pattern_type} at line {location} {verb} {line}", verb = self.verb(), line = self.line())]
pub struct OutputPatternMatchFailure {
    location: ScriptLocation,
    pattern_type: Cow<'static, str>,
    pub output_line: Option<Line>,
    pattern_label: Option<String>,
}

impl OutputPatternMatchFailure {
    pub fn new(
        location: &ScriptLocation,
        line: Option<Line>,
        pattern_type: &OutputPatternType,
    ) -> Self {
        Self {
            location: location.clone(),
            pattern_type: Cow::Owned(pattern_type.trace_string()),
            output_line: line,
            pattern_label: None,
        }
    }

    pub fn new_reject(location: &ScriptLocation, line: Option<Line>) -> Self {
        Self {
            location: location.clone(),
            pattern_type: Cow::Borrowed("reject"),
            output_line: line,
            pattern_label: None,
        }
    }

    fn verb(&self) -> &'static str {
        if self.pattern_type == "reject" {
            "rejected"
        } else {
            "does not match"
        }
    }

    fn line(&self) -> String {
        self.output_line
            .as_ref()
            .map(|l| format!("output line {:?}", l.text))
            .unwrap_or("output".to_string())
    }
}

fn trace_shows_output_line(pattern: &OutputPatternType, success: bool) -> bool {
    match (pattern, success) {
        (OutputPatternType::Pattern(_), _)
        | (OutputPatternType::Literal(_), false)
        | (OutputPatternType::End, false) => true,
        _ => false,
    }
}

#[derive(Debug, Clone)]
pub enum PatternTraceNote {
    AliasMismatch(String, String),
    IfConditionMet(IfCondition),
    IfConditionSkipped(IfCondition),
}

/// One pattern invocation in the output-match trace: a single tree node with nested child patterns.
#[derive(Debug, Clone)]
pub struct OutputMatchTraceNode {
    pub ignore: bool,
    pub pattern: OutputPatternType,
    pub succeeded: bool,
    pub output_line: Option<Line>,
    pub note: Option<PatternTraceNote>,
    pub children: Vec<OutputMatchTraceNode>,
}

impl OutputMatchTraceNode {
    fn fmt_lines(&self, depth: usize, out: &mut String) {
        use std::fmt::Write;

        let indent = depth * 2;
        _ = write!(out, "{:indent$}", "", indent = indent);

        if self.succeeded {
            _ = write!(out, "[OK]");
        } else {
            _ = write!(out, "[XX]");
        }

        if self.ignore {
            _ = write!(out, "-");
        }

        if self.succeeded {
            _ = write!(out, " matched ");
        } else {
            _ = write!(out, " missed ");
        }
        _ = write!(out, "{}", self.pattern.trace_string());

        let show_line = trace_shows_output_line(&self.pattern, self.succeeded);
        if show_line {
            if self.succeeded {
                _ = write!(out, " = ");
            } else {
                _ = write!(out, " != ");
            }

            if let Some(line) = &self.output_line {
                _ = write!(out, "{:?}", line.text);
            } else {
                _ = write!(out, "<eof>");
            }
        }

        match &self.note {
            Some(PatternTraceNote::AliasMismatch(a, b)) => {
                _ = write!(out, " (alias conflict: {a:?} != {b:?})");
            }
            Some(PatternTraceNote::IfConditionMet(c)) => _ = write!(out, " (if {c:?})"),
            Some(PatternTraceNote::IfConditionSkipped(c)) => _ = write!(out, " (if skipped {c:?})"),
            None => (),
        };

        if self.ignore {
            _ = write!(out, " (ignore)");
        }
        _ = writeln!(out);
        for child in &self.children {
            child.fmt_lines(depth + 1, out);
        }
    }
}

/// Pre-order walk with two spaces of indentation per tree level.
pub fn format_match_trace_tree(nodes: &[OutputMatchTraceNode]) -> String {
    let mut out = String::new();
    for node in nodes {
        node.fmt_lines(0, &mut out);
    }
    out
}
