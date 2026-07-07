use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, Mutex, OnceLock},
};

use grok::Grok;
use serde::Serialize;

use crate::failure::OutputPatternMatchFailure;
use crate::{
    failure::{OutputMatchTraceNode, PatternTraceNote},
    script::{IfCondition, ScriptLocation, ScriptRunContext},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub number: usize,
    pub text: String,
}

#[derive(Clone)]
pub struct Lines {
    lines: Arc<Vec<String>>,
    current_line: usize,
    ignored_patterns: OutputPatterns,
    negative_disabled: bool,
    rejected_patterns: OutputPatterns,
}

impl std::fmt::Debug for Lines {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Lines {{ ignored: {} pattern(s), rejected: {} pattern(s) }}",
            self.ignored_patterns.len(),
            self.rejected_patterns.len()
        )
    }
}

impl<'s> IntoIterator for &'s Lines {
    type Item = &'s String;
    type IntoIter = std::slice::Iter<'s, String>;

    fn into_iter(self) -> Self::IntoIter {
        self.lines.iter()
    }
}

impl std::fmt::Display for Lines {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.lines[self.current_line..].join("\n"))
    }
}

impl Lines {
    pub fn new(lines: Vec<String>) -> Self {
        Self {
            lines: Arc::new(lines),
            current_line: 0,
            ignored_patterns: Default::default(),
            negative_disabled: false,
            rejected_patterns: Default::default(),
        }
    }

    pub fn is_exhausted(&self) -> bool {
        self.current_line >= self.lines.len()
    }

    pub fn next_line(&self) -> Option<Line> {
        if self.current_line < self.lines.len() {
            Some(Line {
                number: self.current_line,
                text: self.lines[self.current_line].clone(),
            })
        } else {
            None
        }
    }

    pub fn next(
        &self,
        context: OutputMatchContext,
    ) -> Result<(Option<Line>, Lines), OutputPatternMatchFailure> {
        let mut next = self.clone();
        'outer: while next.current_line < next.lines.len() {
            if !self.negative_disabled {
                let ignore_check = next.without_negatives();
                for ignored_pattern in &*next.ignored_patterns {
                    if let Ok(next_next) =
                        ignored_pattern.matches(context.ignore(), ignore_check.clone())
                    {
                        next = next_next.with_negatives();
                        continue 'outer;
                    }
                }
                for rejected_pattern in &*next.rejected_patterns {
                    if rejected_pattern
                        .matches(context.ignore(), ignore_check.clone())
                        .is_ok()
                    {
                        return Err(OutputPatternMatchFailure::new_reject(
                            &rejected_pattern.location,
                            next.next_line(),
                        ));
                    }
                }
            }
            let line = Line {
                number: next.current_line,
                text: next.lines[next.current_line].clone(),
            };
            next.current_line += 1;
            return Ok((Some(line), next));
        }
        Ok((None, next))
    }

    pub fn with_ignore(&self, ignore: &OutputPatterns) -> Self {
        let mut ignored_patterns = self.ignored_patterns.clone();
        ignored_patterns.extend(ignore);
        Self {
            ignored_patterns,
            ..self.clone()
        }
    }

    pub fn with_reject(&self, reject: &OutputPatterns) -> Self {
        let mut rejected_patterns = self.rejected_patterns.clone();
        rejected_patterns.extend(reject);
        Self {
            rejected_patterns,
            ..self.clone()
        }
    }

    fn without_negatives(&self) -> Self {
        Self {
            negative_disabled: true,
            ..self.clone()
        }
    }

    fn with_negatives(&self) -> Self {
        Self {
            negative_disabled: false,
            ..self.clone()
        }
    }

    pub fn into_inner(self) -> Vec<String> {
        Arc::unwrap_or_clone(self.lines).split_off(self.current_line)
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

#[derive(Clone, Default, Debug, Serialize)]

pub struct OutputPatterns {
    patterns: Arc<Vec<OutputPattern>>,
}

impl OutputPatterns {
    pub fn new(patterns: Vec<OutputPattern>) -> Self {
        Self {
            patterns: Arc::new(patterns),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    pub fn len(&self) -> usize {
        self.patterns.len()
    }

    pub fn extend(&mut self, patterns: &OutputPatterns) {
        if self.is_empty() {
            self.patterns = patterns.patterns.clone();
            return;
        }
        let new_patterns = std::mem::take(&mut self.patterns);
        let mut new_patterns = Arc::unwrap_or_clone(new_patterns);
        new_patterns.extend(patterns.patterns.iter().cloned());
        self.patterns = Arc::new(new_patterns);
    }
}

impl std::ops::Deref for OutputPatterns {
    type Target = Vec<OutputPattern>;
    fn deref(&self) -> &Self::Target {
        &self.patterns
    }
}

#[derive(Clone)]
pub struct OutputPattern {
    pub location: ScriptLocation,
    pub pattern: OutputPatternType,
    pub ignore: OutputPatterns,
    pub reject: OutputPatterns,
}

impl Serialize for OutputPattern {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.pattern.serialize(serializer)
    }
}

impl std::fmt::Debug for OutputPattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.pattern)
    }
}

impl OutputPattern {
    pub fn new_sequence(location: ScriptLocation, mut patterns: Vec<OutputPattern>) -> Self {
        if patterns.len() == 1 {
            patterns.remove(0)
        } else {
            Self {
                pattern: OutputPatternType::Sequence(patterns),
                ignore: Default::default(),
                reject: Default::default(),
                location: location.clone(),
            }
        }
    }

    pub fn prepare(&self, grok: &Grok) -> Result<(), OutputPatternPrepareError> {
        for pattern in &*self.ignore.patterns {
            pattern.prepare(grok)?
        }
        for pattern in &*self.reject.patterns {
            pattern.prepare(grok)?
        }
        match &self.pattern {
            OutputPatternType::Pattern(pattern) => {
                pattern
                    .prepare(grok)
                    .map_err(|e| OutputPatternPrepareError {
                        location: self.location.clone(),
                        pattern: pattern.pattern.clone(),
                        error: e,
                    })?
            }
            OutputPatternType::Sequence(patterns) => {
                for pattern in patterns {
                    pattern.prepare(grok)?;
                }
            }
            OutputPatternType::Unordered(patterns) => {
                for pattern in patterns {
                    pattern.prepare(grok)?;
                }
            }
            OutputPatternType::Choice(patterns) => {
                for pattern in patterns {
                    pattern.prepare(grok)?;
                }
            }
            OutputPatternType::If(_, pattern) => pattern.prepare(grok)?,
            OutputPatternType::Not(pattern) => pattern.prepare(grok)?,
            OutputPatternType::Any(pattern) => pattern.prepare(grok)?,
            OutputPatternType::Repeat(pattern) => pattern.prepare(grok)?,
            OutputPatternType::Optional(pattern) => pattern.prepare(grok)?,
            OutputPatternType::Literal(_) => {}
            OutputPatternType::End | OutputPatternType::None => {}
        }
        Ok(())
    }

    pub fn matches(
        &self,
        context: OutputMatchContext,
        output: Lines,
    ) -> Result<Lines, OutputPatternMatchFailure> {
        if self.ignore.is_empty() && self.reject.is_empty() {
            self.pattern.matches(&self.location, context, output)
        } else {
            let output = output.with_ignore(&self.ignore).with_reject(&self.reject);
            self.pattern.matches(&self.location, context, output)
        }
    }

    /// The minimum number of lines this pattern will match.
    pub fn min_matches(&self) -> usize {
        self.pattern.min_matches()
    }

    /// The maximum number of lines this pattern will match (or usize::MAX if unbounded).
    pub fn max_matches(&self) -> usize {
        self.pattern.max_matches()
    }
}

#[derive(thiserror::Error, Debug)]
#[error("pattern {pattern} at line {location} failed to compile: {error}")]
pub struct OutputPatternPrepareError {
    pub location: ScriptLocation,
    pub pattern: String,
    pub error: grok::Error,
}

#[derive(Clone)]
pub enum OutputPatternType {
    /// The end of the output
    End,
    /// Matches no lines of output, always succeeds
    None,
    /// Any lines, followed by a pattern.
    Any(Box<OutputPattern>),
    /// A literal string
    Literal(String),
    /// A grok pattern
    Pattern(Arc<GrokPattern>),
    /// A pattern that matches one or more of the given pattern
    Repeat(Box<OutputPattern>),
    /// A pattern that matches zero or one of the given pattern
    Optional(Box<OutputPattern>),
    /// A pattern that all of its subpatterns, but in any order
    Unordered(Vec<OutputPattern>),
    /// A pattern that matches one of the given patterns
    Choice(Vec<OutputPattern>),
    /// A pattern that matches a sequence of patterns
    Sequence(Vec<OutputPattern>),
    /// A negative look-ahead pattern
    Not(Box<OutputPattern>),
    /// A pattern that matches a condition
    If(IfCondition, Box<OutputPattern>),
}

impl Serialize for OutputPatternType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            OutputPatternType::Literal(literal) => {
                serializer.serialize_str(&format!("! {literal}"))
            }
            OutputPatternType::Pattern(pattern) => {
                serializer.serialize_str(&pattern.display_source())
            }
            OutputPatternType::Repeat(pattern) => {
                HashMap::from([("repeat", &pattern)]).serialize(serializer)
            }
            OutputPatternType::Optional(pattern) => {
                HashMap::from([("optional", &pattern)]).serialize(serializer)
            }
            OutputPatternType::Unordered(patterns) => {
                HashMap::from([("unordered", &patterns)]).serialize(serializer)
            }
            OutputPatternType::Choice(patterns) => {
                HashMap::from([("choice", &patterns)]).serialize(serializer)
            }
            OutputPatternType::Sequence(patterns) => {
                HashMap::from([("sequence", &patterns)]).serialize(serializer)
            }
            OutputPatternType::Not(pattern) => {
                HashMap::from([("not", &pattern)]).serialize(serializer)
            }
            OutputPatternType::Any(pattern) => {
                HashMap::from([("any", &pattern)]).serialize(serializer)
            }
            OutputPatternType::If(condition, pattern) => {
                #[derive(Serialize)]
                struct If<'a> {
                    condition: &'a IfCondition,
                    pattern: &'a OutputPattern,
                }
                If { condition, pattern }.serialize(serializer)
            }
            OutputPatternType::End => serializer.serialize_str("end"),
            OutputPatternType::None => serializer.serialize_str("none"),
        }
    }
}

impl OutputPatternType {
    /// The minimum number of lines this pattern will match.
    pub fn min_matches(&self) -> usize {
        match self {
            OutputPatternType::None => 0,
            OutputPatternType::Literal(_) => 1,
            OutputPatternType::Pattern(_) => 1,
            OutputPatternType::Repeat(pattern) => pattern.min_matches(),
            OutputPatternType::Optional(_) => 0,
            OutputPatternType::Unordered(patterns) => {
                patterns.iter().map(|p| p.min_matches()).sum()
            }
            OutputPatternType::Choice(patterns) => {
                patterns.iter().map(|p| p.min_matches()).min().unwrap_or(0)
            }
            OutputPatternType::Sequence(patterns) => patterns.iter().map(|p| p.min_matches()).sum(),
            OutputPatternType::Not(_) => 0,
            OutputPatternType::Any(pattern) => pattern.min_matches(),
            OutputPatternType::If(_, _) => 0,
            OutputPatternType::End => 0,
        }
    }

    /// The maximum number of lines this pattern will match (or usize::MAX if unbounded).
    pub fn max_matches(&self) -> usize {
        fn saturating_iter_sum<I>(iter: I) -> usize
        where
            I: IntoIterator<Item = usize>,
        {
            iter.into_iter()
                .reduce(|n, i| n.saturating_add(i))
                .unwrap_or(0)
        }

        match self {
            OutputPatternType::None => 0,
            OutputPatternType::Literal(_) => 1,
            OutputPatternType::Pattern(_) => 1,
            OutputPatternType::Repeat(pattern) => {
                if pattern.max_matches() == 0 {
                    0
                } else {
                    usize::MAX
                }
            }
            OutputPatternType::Optional(pattern) => pattern.max_matches(),
            OutputPatternType::Unordered(patterns) => {
                saturating_iter_sum(patterns.iter().map(|p| p.max_matches()))
            }
            OutputPatternType::Choice(patterns) => {
                patterns.iter().map(|p| p.max_matches()).max().unwrap_or(0)
            }
            OutputPatternType::Sequence(patterns) => {
                saturating_iter_sum(patterns.iter().map(|p| p.max_matches()))
            }
            OutputPatternType::Not(_) => 0,
            OutputPatternType::Any(_) => usize::MAX,
            OutputPatternType::If(_, pattern) => pattern.max_matches(),
            OutputPatternType::End => 0,
        }
    }

    pub fn keyword(&self) -> &'static str {
        match self {
            OutputPatternType::Literal(_) => "\"...\"",
            OutputPatternType::Pattern(_) => "? ...",
            OutputPatternType::Repeat(_) => "repeat",
            OutputPatternType::Optional(_) => "optional",
            OutputPatternType::Unordered(_) => "unordered",
            OutputPatternType::Choice(_) => "choice",
            OutputPatternType::Sequence(_) => "sequence",
            OutputPatternType::Not(_) => "not",
            OutputPatternType::Any(_) => "*",
            OutputPatternType::If(_, _) => "if",
            OutputPatternType::End => "end",
            OutputPatternType::None => "none",
        }
    }

    pub fn is_container(&self) -> bool {
        match self {
            OutputPatternType::Literal(_) => false,
            OutputPatternType::Pattern(_) => false,
            OutputPatternType::Repeat(_) => true,
            OutputPatternType::Optional(_) => true,
            OutputPatternType::Unordered(_) => true,
            OutputPatternType::Choice(_) => true,
            OutputPatternType::Sequence(_) => true,
            OutputPatternType::Not(_) => true,
            OutputPatternType::Any(_) => false,
            OutputPatternType::If(_, _) => true,
            OutputPatternType::End => false,
            OutputPatternType::None => false,
        }
    }

    pub fn trace_string(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        _ = match self {
            OutputPatternType::Any(pattern) => {
                if let OutputPatternType::End = pattern.pattern {
                    write!(out, "*")
                } else {
                    write!(out, "* ... {}", pattern.pattern.trace_string())
                }
            }
            OutputPatternType::End => {
                write!(out, "<eof>")
            }
            OutputPatternType::Pattern(pattern) => {
                write!(out, "pattern {:?}", pattern.display_source())
            }
            OutputPatternType::Literal(literal) => {
                write!(out, "{literal:?}")
            }
            _ if self.is_container() => {
                write!(out, "{} {{ ... }}", self.keyword())
            }
            _ => {
                write!(out, "{:?}", self)
            }
        };
        out
    }
}

impl Default for OutputPatternType {
    fn default() -> Self {
        Self::Sequence(vec![])
    }
}

impl std::fmt::Debug for OutputPatternType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutputPatternType::Literal(literal) => write!(f, "{literal:?}"),
            OutputPatternType::Pattern(pattern) => write!(f, "Pattern({pattern:?})"),
            OutputPatternType::Repeat(pattern) => write!(f, "Repeat({pattern:?})"),
            OutputPatternType::Optional(pattern) => write!(f, "Optional({pattern:?})"),
            OutputPatternType::Unordered(patterns) => write!(f, "Unordered({patterns:?})"),
            OutputPatternType::Choice(patterns) => write!(f, "Choice({patterns:?})"),
            OutputPatternType::Sequence(patterns) => write!(f, "Sequence({patterns:?})"),
            OutputPatternType::Not(pattern) => write!(f, "Not({pattern:?})"),
            OutputPatternType::Any(until) => write!(f, "Any({until:?})"),
            OutputPatternType::If(condition, pattern) => {
                write!(f, "If({condition:?}, {pattern:?})")
            }
            OutputPatternType::End => write!(f, "End"),
            OutputPatternType::None => write!(f, "None"),
        }
    }
}

#[derive(Serialize, derive_more::Debug)]
#[debug("/{pattern:?}/")]
pub struct GrokPattern {
    original: String,
    pattern: String,
    aliases: Vec<String>,
    #[serde(skip)]
    grok: OnceLock<grok::Pattern>,
}

impl GrokPattern {
    pub fn display_source(&self) -> &str {
        &self.original
    }

    pub fn compile(line: &str, original: String, escape_non_grok: bool) -> Result<Self, String> {
        use grok::parser::GrokPatternError;
        let mut test_pattern = String::new();
        let mut final_pattern = String::new();
        let mut aliases = vec![];
        for bit in grok::parser::grok_split(line) {
            match bit {
                grok::parser::GrokComponent::RegularExpression { string, .. } => {
                    if escape_non_grok {
                        for char in string.chars() {
                            if char.is_ascii() && !char.is_alphanumeric() {
                                test_pattern.push('\\');
                                test_pattern.push(char);
                                final_pattern.push('\\');
                                final_pattern.push(char);
                            } else {
                                test_pattern.push(char);
                                final_pattern.push(char);
                            }
                        }
                    } else {
                        test_pattern.push_str(string);
                        final_pattern.push_str(string);
                    }
                }
                grok::parser::GrokComponent::GrokPattern { pattern, alias, .. } => {
                    test_pattern.push('.');
                    final_pattern.push_str(pattern);
                    if !alias.is_empty() {
                        aliases.push(alias.to_string());
                    }
                }
                grok::parser::GrokComponent::PatternError(GrokPatternError::InvalidCharacter(
                    c,
                )) => {
                    return Err(format!("Invalid character in pattern: {c:?}"));
                }
                grok::parser::GrokComponent::PatternError(GrokPatternError::InvalidPattern) => {
                    return Err("Invalid grok pattern".to_string());
                }
                grok::parser::GrokComponent::PatternError(
                    GrokPatternError::InvalidPatternDefinition,
                ) => {
                    return Err("Invalid grok pattern definition".to_string());
                }
            }
        }

        test_pattern.push('$');
        final_pattern.push('$');

        _ = Grok::empty()
            .compile(&test_pattern, false)
            .map_err(|e| e.to_string())?;

        Ok(Self {
            original,
            pattern: final_pattern,
            aliases,
            grok: OnceLock::new(),
        })
    }

    pub fn prepare(&self, grok: &Grok) -> Result<(), grok::Error> {
        // This could technically suffer from multiple init, but they should
        // always initialize the same way.
        if self.grok.get().is_none() {
            let pattern = grok.compile(&self.pattern, false)?;
            self.grok.get_or_init(move || pattern);
        }
        Ok(())
    }

    pub fn matches<'a>(&'a self, text: &'a str) -> Option<grok::Matches<'a>> {
        let pattern_ref = self.grok.get().expect("grok pattern not compiled");
        pattern_ref.match_against(text)
    }
}

#[derive(Debug, Default)]
struct OutputMatchTraceCollector {
    root: Vec<OutputMatchTraceNode>,
    /// Path of indices from [`Self::root`] down to the [`OutputMatchTraceNode`] whose
    /// [`OutputMatchTraceNode::children`] receives nested pattern nodes.
    path: Vec<usize>,
}

fn resolve_trace_node_mut<'a>(
    root: &'a mut Vec<OutputMatchTraceNode>,
    path: &[usize],
    idx: usize,
) -> &'a mut OutputMatchTraceNode {
    let mut cur = root;
    for &p in path {
        cur = &mut cur[p].children;
    }
    &mut cur[idx]
}

impl OutputMatchTraceCollector {
    fn navigate_mut<'a>(&'a mut self) -> &'a mut Vec<OutputMatchTraceNode> {
        let mut cur = &mut self.root;
        for &idx in &self.path {
            cur = &mut cur[idx].children;
        }
        cur
    }

    fn composite_pattern_begin(&mut self, pattern: OutputPatternType, ignore: bool) {
        let list = self.navigate_mut();
        let idx = list.len();
        list.push(OutputMatchTraceNode {
            ignore,
            pattern,
            succeeded: false,
            output_line: None,
            note: None,
            children: Vec::new(),
        });
        self.path.push(idx);
    }

    fn composite_pattern_end(
        &mut self,
        succeeded: bool,
        output_line: Option<Line>,
        note: Option<PatternTraceNote>,
    ) {
        let idx = self
            .path
            .pop()
            .expect("composite_pattern_end without composite_pattern_begin");
        let node = resolve_trace_node_mut(&mut self.root, &self.path, idx);
        node.succeeded = succeeded;
        node.output_line = output_line;
        node.note = note;
    }

    fn pop_traces_before_last(&mut self, count: usize) {
        let trace = self.navigate_mut().pop().expect("no trace to pop");
        for _ in 0..count {
            self.navigate_mut().pop().expect("no trace to pop");
        }
        self.navigate_mut().push(trace);
    }

    fn leaf_pattern(&mut self, node: OutputMatchTraceNode) {
        let list = self.navigate_mut();
        list.push(node);
    }

    fn take_root(&mut self) -> Vec<OutputMatchTraceNode> {
        self.path.clear();
        std::mem::take(&mut self.root)
    }
}

#[derive(Debug, Clone)]
pub struct OutputMatchContext<'s> {
    trace: Arc<Mutex<OutputMatchTraceCollector>>,
    ignore: bool,
    expectations: Arc<Mutex<HashMap<String, String>>>,
    script_context: &'s ScriptRunContext,
}

/// Successful internal match before trace decoration.
struct RawPatternOk {
    lines: Lines,
    matched_line_if_ok: Option<Line>,
    /// Used by composites such as [`OutputPatternType::If`] (branch annotation).
    note: Option<PatternTraceNote>,
}

/// Failed internal match before trace decoration.
struct RawPatternErr {
    failure: OutputPatternMatchFailure,
    note: Option<PatternTraceNote>,
}

impl From<OutputPatternMatchFailure> for RawPatternErr {
    fn from(failure: OutputPatternMatchFailure) -> Self {
        Self {
            failure,
            note: None,
        }
    }
}

/// Internal [`OutputPatternType::raw_matches`] result before public [`Result`] mapping.
type RawPatternMatch = Result<RawPatternOk, RawPatternErr>;

fn raw_ok(lines: Lines, matched_line_if_ok: Option<Line>) -> RawPatternMatch {
    Ok(RawPatternOk {
        lines,
        matched_line_if_ok,
        note: None,
    })
}

fn raw_err(failure: OutputPatternMatchFailure, note: Option<PatternTraceNote>) -> RawPatternMatch {
    Err(RawPatternErr { failure, note })
}

fn raw_into_public(raw: RawPatternMatch) -> Result<Lines, OutputPatternMatchFailure> {
    raw.map(|ok| ok.lines).map_err(|e| e.failure)
}

fn record_leaf_pattern(
    context: &OutputMatchContext<'_>,
    pattern: OutputPatternType,
    raw: &RawPatternMatch,
) {
    let node = match raw {
        Ok(ok) => OutputMatchTraceNode {
            ignore: context.ignore,
            pattern,
            succeeded: true,
            output_line: ok.matched_line_if_ok.clone(),
            note: ok.note.clone(),
            children: Vec::new(),
        },
        Err(err) => OutputMatchTraceNode {
            ignore: context.ignore,
            pattern,
            succeeded: false,
            output_line: err.failure.output_line.clone(),
            note: err.note.clone(),
            children: Vec::new(),
        },
    };
    context.trace.lock().unwrap().leaf_pattern(node);
}

fn finish_composite_pattern(context: &OutputMatchContext<'_>, raw: &RawPatternMatch) {
    let (succeeded, output_line, note) = match raw {
        Ok(ok) => (true, ok.matched_line_if_ok.clone(), ok.note.clone()),
        Err(err) => (false, err.failure.output_line.clone(), err.note.clone()),
    };
    context
        .trace
        .lock()
        .unwrap()
        .composite_pattern_end(succeeded, output_line, note);
}

impl<'s> OutputMatchContext<'s> {
    pub fn new(script_context: &'s ScriptRunContext) -> Self {
        Self {
            trace: Default::default(),
            ignore: false,
            script_context,
            expectations: Default::default(),
        }
    }

    /// Cheap clone passed into nested [`OutputPattern::matches`] calls.
    pub fn descend(&self) -> Self {
        Self {
            trace: self.trace.clone(),
            ignore: self.ignore,
            script_context: self.script_context,
            expectations: self.expectations.clone(),
        }
    }

    pub fn composite_pattern_begin(&self, pattern: OutputPatternType) {
        self.trace
            .lock()
            .unwrap()
            .composite_pattern_begin(pattern, self.ignore);
    }

    pub fn ignore(&self) -> Self {
        Self {
            trace: self.trace.clone(),
            ignore: true,
            script_context: self.script_context,
            expectations: self.expectations.clone(),
        }
    }

    pub fn traces(&self) -> Vec<OutputMatchTraceNode> {
        self.trace.lock().unwrap().take_root()
    }

    pub fn expect(&self, key: &str, value: String) {
        self.expectations
            .lock()
            .unwrap()
            .insert(key.to_string(), value);
    }

    pub fn expects(&self) -> HashMap<String, String> {
        self.expectations.lock().unwrap().clone()
    }
}

impl OutputPatternType {
    pub fn matches(
        &self,
        location: &ScriptLocation,
        context: OutputMatchContext,
        output: Lines,
    ) -> Result<Lines, OutputPatternMatchFailure> {
        match self {
            OutputPatternType::None
            | OutputPatternType::Literal(_)
            | OutputPatternType::Pattern(_)
            | OutputPatternType::End => {
                let raw = self.raw_matches(location, &context, output);
                record_leaf_pattern(&context, self.clone(), &raw);
                raw_into_public(raw)
            }
            _ => {
                context.composite_pattern_begin(self.clone());
                let raw = self.raw_matches(location, &context, output);
                finish_composite_pattern(&context, &raw);
                raw_into_public(raw)
            }
        }
    }

    fn raw_matches(
        &self,
        location: &ScriptLocation,
        context: &OutputMatchContext<'_>,
        mut output: Lines,
    ) -> RawPatternMatch {
        match self {
            OutputPatternType::None => raw_ok(output, None),
            OutputPatternType::Literal(literal) => {
                let (line, next) = output.next(context.clone()).map_err(RawPatternErr::from)?;
                let Some(line) = line else {
                    return raw_err(OutputPatternMatchFailure::new(location, None, self), None);
                };
                let text = line.text.trim_end();
                if text == literal
                    || (line.text.contains('\x1b')
                        && fast_strip_ansi::strip_ansi_string(&line.text).as_ref() == literal)
                {
                    raw_ok(next, Some(line.clone()))
                } else {
                    raw_err(
                        OutputPatternMatchFailure::new(location, Some(line), self),
                        None,
                    )
                }
            }
            OutputPatternType::Pattern(pattern) => {
                let (line, next) = output.next(context.clone()).map_err(RawPatternErr::from)?;
                let Some(line) = line else {
                    return raw_err(OutputPatternMatchFailure::new(location, None, self), None);
                };
                let mut text = line.text.clone();
                let mut res = pattern.matches(&text);
                if res.is_none() {
                    // Give it a second chance with the ANSI-stripped text IF we detect escape sequences
                    if text.contains('\x1b') {
                        text = fast_strip_ansi::strip_ansi_string(&text).into_owned();
                        res = pattern.matches(&text);
                    }
                }
                match res {
                    None => raw_err(
                        OutputPatternMatchFailure::new(location, Some(line), self),
                        None,
                    ),
                    Some(matches) => {
                        for alias in &pattern.aliases {
                            if let Some(value) = matches.get(alias) {
                                let existing = context
                                    .expectations
                                    .lock()
                                    .unwrap()
                                    .insert(alias.clone(), value.to_string());
                                if let Some(existing) = existing
                                    && existing != value
                                {
                                    return raw_err(
                                        OutputPatternMatchFailure::new(location, Some(line), self),
                                        Some(PatternTraceNote::AliasMismatch(
                                            existing,
                                            value.to_string(),
                                        )),
                                    );
                                }
                            }
                        }
                        raw_ok(next, Some(line.clone()))
                    }
                }
            }
            OutputPatternType::Sequence(patterns) => {
                for pattern in patterns {
                    output = pattern
                        .matches(context.descend(), output)
                        .map_err(RawPatternErr::from)?;
                }
                raw_ok(output, None)
            }
            OutputPatternType::Repeat(pattern) => {
                let mut output = pattern
                    .matches(context.descend(), output)
                    .map_err(RawPatternErr::from)?;
                loop {
                    match pattern.matches(context.descend(), output.clone()) {
                        Ok(new_rest) => output = new_rest,
                        Err(_) => break,
                    }
                }
                raw_ok(output, None)
            }
            OutputPatternType::Optional(pattern) => {
                let lines = match pattern.matches(context.descend(), output.clone()) {
                    Ok(v) => v,
                    Err(_) => output,
                };
                raw_ok(lines, None)
            }
            OutputPatternType::Unordered(patterns) => {
                let mut not_found = (0..patterns.len()).collect::<BTreeSet<_>>();
                'outer: while !not_found.is_empty() {
                    let mut cleanup = 0;
                    for idx in &not_found {
                        let idx = *idx;
                        match patterns[idx].matches(context.descend(), output.clone()) {
                            Ok(v) => {
                                not_found.remove(&idx);
                                output = v;
                                context
                                    .trace
                                    .lock()
                                    .unwrap()
                                    .pop_traces_before_last(cleanup);
                                continue 'outer;
                            }
                            Err(_) => {
                                cleanup += 1;
                            }
                        }
                    }
                    return raw_err(
                        OutputPatternMatchFailure::new(location, output.next_line(), self),
                        None,
                    );
                }
                raw_ok(output, None)
            }
            OutputPatternType::Choice(patterns) => {
                for pattern in patterns {
                    if let Ok(v) = pattern.matches(context.descend(), output.clone()) {
                        return Ok(RawPatternOk {
                            lines: v,
                            matched_line_if_ok: None,
                            note: None,
                        });
                    }
                }
                raw_err(
                    OutputPatternMatchFailure::new(location, output.next_line(), self),
                    None,
                )
            }
            OutputPatternType::Not(pattern) => {
                if pattern.matches(context.descend(), output.clone()).is_err() {
                    raw_ok(output, None)
                } else {
                    raw_err(
                        OutputPatternMatchFailure::new(location, output.next_line(), self),
                        None,
                    )
                }
            }
            OutputPatternType::Any(until) => loop {
                match until.matches(context.descend(), output.clone()) {
                    Ok(v) => {
                        output = v;
                        break raw_ok(output, None);
                    }
                    Err(e) => match output.next(context.clone()) {
                        Err(failure) => break Err(failure.into()),
                        Ok((Some(_), next)) => output = next,
                        Ok((None, _)) => break Err(e.into()),
                    },
                }
            },
            OutputPatternType::If(condition, pattern) => {
                let branch_met = condition.matches(context.script_context);
                let branch_note = if branch_met {
                    PatternTraceNote::IfConditionMet(condition.clone())
                } else {
                    PatternTraceNote::IfConditionSkipped(condition.clone())
                };
                let inner = if branch_met {
                    pattern.matches(context.clone(), output.clone())
                } else {
                    Ok(output)
                };
                inner
                    .map(|lines| RawPatternOk {
                        lines,
                        matched_line_if_ok: None,
                        note: Some(branch_note.clone()),
                    })
                    .map_err(|failure| RawPatternErr {
                        failure,
                        note: Some(branch_note),
                    })
            }
            OutputPatternType::End => {
                let (line, next) = output.next(context.clone()).map_err(RawPatternErr::from)?;
                if let Some(line) = line {
                    raw_err(
                        OutputPatternMatchFailure::new(location, Some(line), self),
                        None,
                    )
                } else {
                    raw_ok(next, None)
                }
            }
        }
    }
}

#[derive(Debug)]
pub enum PatternResult {
    Matches,
    MatchesFailure,
    ExpectedFailure,
    Mismatch(OutputPatternMatchFailure, String),
}
