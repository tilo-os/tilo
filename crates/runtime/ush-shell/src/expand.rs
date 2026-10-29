//! Word expansion, in POSIX's order: brace expansion (bash's), then tilde,
//! parameter, command and arithmetic expansion, field splitting, pathname
//! expansion and quote removal.
//!
//! An expanded word is a list of fields, each a run of segments that remember
//! whether they were quoted and whether they came out of an expansion: only an
//! unquoted expansion's text is split, and only unquoted text globs.

use brush_parser::word::{
    self, Parameter, ParameterExpr, ParameterTestType, ParameterTransformOp, SpecialParameter,
    SubstringMatchKind, TildeExpr, WordPiece, WordPieceWithSource,
};

use crate::pattern::{PatChar, Pattern};
use crate::shell::{Flow, Shell};
use crate::vars::Value;

/// A run of text in a field.
#[derive(Debug, Clone)]
struct Seg {
    text: String,
    quoted: bool,
    /// From an unquoted expansion: field splitting applies.
    split: bool,
}

/// One field being built.
#[derive(Debug, Clone, Default)]
struct Field {
    segs: Vec<Seg>,
    /// A quoted piece was here, so the field stays even when empty (`""`).
    kept: bool,
}

impl Field {
    fn text(&self) -> String {
        self.segs.iter().map(|s| s.text.as_str()).collect()
    }

    fn pattern(&self) -> Vec<PatChar> {
        self.segs
            .iter()
            .flat_map(|s| s.text.chars().map(move |c| (c, s.quoted)))
            .collect()
    }

    fn has_glob(&self) -> bool {
        self.segs
            .iter()
            .any(|s| !s.quoted && s.text.contains(['*', '?', '[']))
    }
}

/// Fields being built: what a word expands to.
#[derive(Debug, Default)]
struct Builder {
    done: Vec<Field>,
    current: Field,
}

impl Builder {
    fn push(&mut self, text: impl Into<String>, quoted: bool, split: bool) {
        let text = text.into();
        if quoted {
            self.current.kept = true;
        }
        // An empty quoted run still holds its place: `""$A""` keeps the
        // fields at either end.
        if !text.is_empty() || quoted {
            self.current.segs.push(Seg {
                text,
                quoted,
                split,
            });
        }
    }

    /// Several words that stay separate (`"$@"`): the first joins what came
    /// before, the last what comes after.
    fn push_words(&mut self, words: Vec<String>, quoted: bool, split: bool) {
        let count = words.len();
        for (i, w) in words.into_iter().enumerate() {
            if i > 0 {
                self.break_field();
            }
            self.push(w, quoted, split);
            if quoted && i.saturating_add(1) < count {
                self.current.kept = true;
            }
        }
    }

    fn break_field(&mut self) {
        let field = std::mem::take(&mut self.current);
        self.done.push(field);
    }

    fn finish(mut self) -> Vec<Field> {
        self.break_field();
        self.done
    }
}

/// What a parameter holds, before an operator works on it.
struct ParamValue {
    values: Vec<String>,
    /// `$@` or `${a[@]}`: separate words even when quoted.
    at: bool,
    /// `$*` or `${a[*]}`: one word joined by IFS's first character when
    /// quoted.
    star: bool,
    set: bool,
}

impl ParamValue {
    fn single(value: Option<String>) -> Self {
        Self {
            set: value.is_some(),
            values: value.into_iter().collect(),
            at: false,
            star: false,
        }
    }

    fn joined(&self, sep: &str) -> String {
        self.values.join(sep)
    }

    /// Null for `:-`: unset or empty. `$@` is null only with no
    /// parameters, or one that is empty.
    fn is_null(&self) -> bool {
        if self.at || self.star {
            return match self.values.as_slice() {
                [] => true,
                [one] => one.is_empty(),
                _ => false,
            };
        }
        !self.set || self.values.iter().all(String::is_empty)
    }
}

impl Shell {
    /// Expand words into the fields a command gets: split, globbed and
    /// unquoted.
    pub(crate) fn expand_words(
        &mut self,
        words: &[brush_parser::ast::Word],
    ) -> Result<Vec<String>, Flow> {
        let mut out = Vec::new();
        for w in words {
            for text in brace_expand(&w.value) {
                let fields = self.expand_fields(&text)?;
                for field in self.split_fields(fields) {
                    self.glob_field(&field, &mut out)?;
                }
            }
        }
        Ok(out)
    }

    /// Expand one word into one string: no splitting, no globbing (an
    /// assignment's value, a `case` word, a redirection's target).
    pub(crate) fn expand_word(&mut self, text: &str) -> Result<String, Flow> {
        let fields = self.expand_fields(text)?;
        let ifs0 = self
            .ifs()
            .chars()
            .next()
            .map(String::from)
            .unwrap_or_default();
        Ok(fields
            .iter()
            .map(Field::text)
            .collect::<Vec<_>>()
            .join(&ifs0))
    }

    /// Expand text that is already inside double quotes, or a here
    /// document's body: expansions happen, quotes are just characters.
    pub(crate) fn expand_string(&mut self, text: &str) -> Result<String, Flow> {
        let pieces = word::parse_heredoc(text, &Self::parser_options()).map_err(|e| {
            self.error(format!("bad substitution: {e}"));
            Flow::Fatal(1)
        })?;
        let mut b = Builder::default();
        self.pieces(&pieces, true, &mut b)?;
        Ok(b.finish()
            .iter()
            .map(Field::text)
            .collect::<Vec<_>>()
            .join(" "))
    }

    /// Expand a word into a pattern: what `case`, `[[ == ]]` and `${x#pat}`
    /// match with. Quoted characters are literal.
    pub(crate) fn expand_pattern(&mut self, text: &str) -> Result<Pattern, Flow> {
        let fields = self.expand_fields(text)?;
        let mut chars = Vec::new();
        for (i, f) in fields.iter().enumerate() {
            if i > 0 {
                chars.push((' ', true));
            }
            chars.extend(f.pattern());
        }
        Ok(Pattern::new(&chars))
    }

    /// Expand words as a pattern-free list that is split but not globbed:
    /// what `for` and an array literal see under `set -f`, and assignment
    /// values of array elements.
    fn expand_fields(&mut self, text: &str) -> Result<Vec<Field>, Flow> {
        let pieces = self.parse_word(text, false)?;
        let mut b = Builder::default();
        self.pieces(&pieces, false, &mut b)?;
        Ok(b.finish())
    }

    /// The pieces of a word, with any text between them that the parser
    /// passed over (white space inside an operand) kept as text.
    fn parse_word(
        &self,
        text: &str,
        tilde_after_colon: bool,
    ) -> Result<Vec<WordPieceWithSource>, Flow> {
        let options = brush_parser::ParserOptions {
            tilde_expansion_after_colon: tilde_after_colon,
            ..Self::parser_options()
        };
        let pieces = word::parse(text, &options).map_err(|e| {
            self.error(format!("{text}: bad substitution ({e})"));
            Flow::Fatal(1)
        })?;
        let mut out = Vec::with_capacity(pieces.len());
        let mut at = 0_usize;
        for p in pieces {
            if p.start_index > at
                && let Some(gap) = text.get(at..p.start_index)
            {
                out.push(WordPieceWithSource {
                    piece: WordPiece::Text(gap.to_string()),
                    start_index: at,
                    end_index: p.start_index,
                });
            }
            at = p.end_index.max(at);
            out.push(p);
        }
        if at < text.len()
            && let Some(gap) = text.get(at..)
        {
            out.push(WordPieceWithSource {
                piece: WordPiece::Text(gap.to_string()),
                start_index: at,
                end_index: text.len(),
            });
        }
        Ok(out)
    }

    /// An assignment's value: one string, with `~` expanded after `:` too
    /// (`PATH=~/bin:~/.local/bin`).
    pub(crate) fn expand_assignment_value(&mut self, text: &str) -> Result<String, Flow> {
        let pieces = self.parse_word(text, true)?;
        let mut b = Builder::default();
        self.pieces(&pieces, false, &mut b)?;
        let fields = b.finish();
        Ok(fields.iter().map(Field::text).collect::<Vec<_>>().join(" "))
    }

    fn pieces(
        &mut self,
        pieces: &[WordPieceWithSource],
        quoted: bool,
        b: &mut Builder,
    ) -> Result<(), Flow> {
        for p in pieces {
            self.piece(&p.piece, quoted, b)?;
        }
        Ok(())
    }

    fn piece(&mut self, piece: &WordPiece, quoted: bool, b: &mut Builder) -> Result<(), Flow> {
        match piece {
            WordPiece::Text(text) => {
                // The parser leaves an expansion it could not read as text:
                // `${a[0][0]}`. Unescaped and unquoted, `${` is always one.
                if text.contains("${") {
                    self.error(format!("{text}: bad substitution"));
                    return Err(Flow::Error(1));
                }
                b.push(text.clone(), quoted, false);
            }
            WordPiece::SingleQuotedText(text) => b.push(text.clone(), true, false),
            WordPiece::AnsiCQuotedText(text) => b.push(ansi_c(text), true, false),
            WordPiece::DoubleQuotedSequence(inner)
            | WordPiece::GettextDoubleQuotedSequence(inner) => {
                // `""` is a field; `"$@"` with no parameters is none.
                if inner.is_empty() {
                    b.push("", true, false);
                }
                self.pieces(inner, true, b)?;
            }
            WordPiece::TildeExpansion(tilde) => {
                let text = self.tilde(tilde);
                b.push(text, true, false);
            }
            WordPiece::ParameterExpansion(expr) => self.parameter_expansion(expr, quoted, b)?,
            WordPiece::CommandSubstitution(text) => {
                let out = self.substitute(text)?;
                b.push(out, quoted, !quoted);
            }
            WordPiece::BackquotedCommandSubstitution(text) => {
                let out = self.substitute(&unbackquote(text, quoted))?;
                b.push(out, quoted, !quoted);
            }
            WordPiece::EscapeSequence(text) => {
                let escaped: String = text.chars().skip(1).collect();
                if escaped == "\n" {
                    return Ok(());
                }
                b.push(escaped, true, false);
            }
            WordPiece::ArithmeticExpression(expr) => {
                let value = self.arith_text(&expr.value)?;
                b.push(value.to_string(), quoted, !quoted);
            }
        }
        Ok(())
    }

    fn tilde(&self, tilde: &TildeExpr) -> String {
        match tilde {
            TildeExpr::Home => self.vars.scalar("HOME").unwrap_or_default(),
            TildeExpr::WorkingDir => self.vars.scalar("PWD").unwrap_or_default(),
            TildeExpr::OldWorkingDir => self.vars.scalar("OLDPWD").unwrap_or_default(),
            TildeExpr::UserHome(user) => self
                .host
                .home_of(user)
                .map_or_else(|| format!("~{user}"), |p| p.to_string_lossy().into_owned()),
            TildeExpr::NthDirFromTopOfDirStack { n, plus_used } => {
                format!("~{}{n}", if *plus_used { "+" } else { "" })
            }
            TildeExpr::NthDirFromBottomOfDirStack { n } => format!("~-{n}"),
        }
    }

    /// Run `text` in a subshell and answer what it wrote, without its
    /// trailing newlines.
    pub(crate) fn substitute(&mut self, text: &str) -> Result<String, Flow> {
        let (out, captured) = crate::io::Output::buffer();
        let mut sub = self.clone();
        sub.fds.insert(1, crate::shell::Fd::Out(out));
        let status = match sub.eval_text(text) {
            Ok(status) | Err(Flow::Exit(status) | Flow::Return(status) | Flow::Error(status)) => {
                status
            }
            Err(Flow::Fatal(status)) => {
                if self.interactive {
                    status
                } else {
                    return Err(Flow::Fatal(status));
                }
            }
            Err(Flow::Break(_) | Flow::Continue(_)) => sub.last_status,
        };
        drop(sub);
        self.last_substitution = Some(status);
        self.last_status = status;
        let mut text = String::from_utf8_lossy(&captured.take()).into_owned();
        while text.ends_with('\n') {
            text.pop();
        }
        Ok(text)
    }

    pub(crate) fn ifs(&self) -> String {
        self.vars.scalar("IFS").unwrap_or_else(|| " \t\n".into())
    }

    /// Field splitting: each unquoted expansion's text is cut at IFS
    /// characters. A field made only of such text that came out empty is
    /// gone.
    fn split_fields(&self, fields: Vec<Field>) -> Vec<Field> {
        let ifs = self.ifs();
        let mut out = Vec::new();
        for field in fields {
            if !field.segs.iter().any(|s| s.split) {
                if field.kept || !field.segs.is_empty() {
                    out.push(field);
                }
                continue;
            }
            let mut current = Field {
                segs: Vec::new(),
                kept: field.kept,
            };
            // Whether `current` holds anything that makes it a field.
            let mut started = false;
            for seg in field.segs {
                if !seg.split || ifs.is_empty() {
                    started = true;
                    current.segs.push(seg);
                    continue;
                }
                let mut text = String::new();
                let mut chars = seg.text.chars().peekable();
                while let Some(c) = chars.next() {
                    if !ifs.contains(c) {
                        text.push(c);
                        continue;
                    }
                    let white = c.is_whitespace();
                    if !text.is_empty() {
                        current.segs.push(Seg {
                            text: std::mem::take(&mut text),
                            quoted: false,
                            split: false,
                        });
                        started = true;
                    }
                    if white {
                        // Runs of IFS white space are one separator, and a
                        // non-white IFS character beside them belongs to it.
                        while chars
                            .peek()
                            .is_some_and(|n| ifs.contains(*n) && n.is_whitespace())
                        {
                            chars.next();
                        }
                        if let Some(n) = chars.peek().copied()
                            && ifs.contains(n)
                            && !n.is_whitespace()
                        {
                            chars.next();
                            out.push(std::mem::take(&mut current));
                            started = false;
                            while chars
                                .peek()
                                .is_some_and(|n| ifs.contains(*n) && n.is_whitespace())
                            {
                                chars.next();
                            }
                            continue;
                        }
                        if started {
                            out.push(std::mem::take(&mut current));
                            started = false;
                        }
                    } else {
                        out.push(std::mem::take(&mut current));
                        started = false;
                        while chars
                            .peek()
                            .is_some_and(|n| ifs.contains(*n) && n.is_whitespace())
                        {
                            chars.next();
                        }
                    }
                }
                if !text.is_empty() {
                    current.segs.push(Seg {
                        text,
                        quoted: false,
                        split: false,
                    });
                    started = true;
                }
            }
            if started {
                out.push(current);
            }
        }
        out
    }

    /// Pathname expansion of one field, then quote removal.
    fn glob_field(&self, field: &Field, out: &mut Vec<String>) -> Result<(), Flow> {
        if self.options.noglob || !field.has_glob() {
            out.push(field.text());
            return Ok(());
        }
        let matches = crate::glob::glob(self, &field.pattern());
        if matches.is_empty() {
            if self.options.failglob {
                self.error(format!("no match: {}", field.text()));
                return Err(Flow::Fatal(1));
            }
            if !self.options.nullglob {
                out.push(field.text());
            }
        } else {
            out.extend(matches);
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one arm per kind of parameter expansion the parser gives"
    )]
    fn parameter_expansion(
        &mut self,
        expr: &ParameterExpr,
        quoted: bool,
        b: &mut Builder,
    ) -> Result<(), Flow> {
        let split = !quoted;
        match expr {
            ParameterExpr::Parameter {
                parameter,
                indirect,
            } => {
                let value = self.param(parameter, *indirect)?;
                self.check_unset(parameter, &value)?;
                self.emit(value, quoted, b);
            }
            ParameterExpr::UseDefaultValues {
                parameter,
                indirect,
                test_type,
                default_value,
            } => {
                let value = self.param(parameter, *indirect)?;
                if self.absent(&value, test_type, quoted) {
                    let text = default_value.clone().unwrap_or_default();
                    self.expand_operand(&text, quoted, b)?;
                } else {
                    self.emit(value, quoted, b);
                }
            }
            ParameterExpr::AssignDefaultValues {
                parameter,
                indirect,
                test_type,
                default_value,
            } => {
                let value = self.param(parameter, *indirect)?;
                if self.absent(&value, test_type, quoted) {
                    let text = default_value.clone().unwrap_or_default();
                    let new = self.expand_word(&text)?;
                    self.assign_parameter(parameter, &new)?;
                    b.push(new, quoted, split);
                } else {
                    self.emit(value, quoted, b);
                }
            }
            ParameterExpr::IndicateErrorIfNullOrUnset {
                parameter,
                indirect,
                test_type,
                error_message,
            } => {
                let value = self.param(parameter, *indirect)?;
                if self.absent(&value, test_type, quoted) {
                    let message = match error_message {
                        Some(text) if !text.is_empty() => self.expand_word(text)?,
                        _ => "parameter null or not set".into(),
                    };
                    self.error(format!("{}: {message}", param_name(parameter)));
                    return Err(if self.interactive {
                        Flow::Fatal(1)
                    } else {
                        Flow::Exit(1)
                    });
                }
                self.emit(value, quoted, b);
            }
            ParameterExpr::UseAlternativeValue {
                parameter,
                indirect,
                test_type,
                alternative_value,
            } => {
                let value = self.param(parameter, *indirect)?;
                if !self.absent(&value, test_type, quoted) {
                    let text = alternative_value.clone().unwrap_or_default();
                    self.expand_operand(&text, quoted, b)?;
                } else if quoted {
                    b.push("", true, false);
                }
            }
            ParameterExpr::ParameterLength {
                parameter,
                indirect,
            } => {
                let value = self.param(parameter, *indirect)?;
                self.check_unset(parameter, &value)?;
                let n = if value.at || value.star {
                    value.values.len()
                } else {
                    value.joined("").chars().count()
                };
                b.push(n.to_string(), quoted, split);
            }
            ParameterExpr::RemoveSmallestSuffixPattern {
                parameter,
                indirect,
                pattern,
            }
            | ParameterExpr::RemoveLargestSuffixPattern {
                parameter,
                indirect,
                pattern,
            }
            | ParameterExpr::RemoveSmallestPrefixPattern {
                parameter,
                indirect,
                pattern,
            }
            | ParameterExpr::RemoveLargestPrefixPattern {
                parameter,
                indirect,
                pattern,
            } => {
                let mut value = self.param(parameter, *indirect)?;
                self.check_unset(parameter, &value)?;
                let pat = self.expand_pattern(pattern.as_deref().unwrap_or(""))?;
                let (suffix, largest) = match expr {
                    ParameterExpr::RemoveSmallestSuffixPattern { .. } => (true, false),
                    ParameterExpr::RemoveLargestSuffixPattern { .. } => (true, true),
                    ParameterExpr::RemoveSmallestPrefixPattern { .. } => (false, false),
                    _ => (false, true),
                };
                for v in &mut value.values {
                    *v = remove_affix(v, &pat, suffix, largest);
                }
                self.emit(value, quoted, b);
            }
            ParameterExpr::Substring {
                parameter,
                indirect,
                offset,
                length,
            } => {
                let value = self.substring(parameter, *indirect, offset, length.as_ref())?;
                self.emit(value, quoted, b);
            }
            ParameterExpr::Transform {
                parameter,
                indirect,
                op,
            } => {
                let mut value = self.param(parameter, *indirect)?;
                let name = param_name(parameter);
                for v in &mut value.values {
                    *v = match op {
                        ParameterTransformOp::ToUpperCase => v.to_uppercase(),
                        ParameterTransformOp::ToLowerCase => v.to_lowercase(),
                        ParameterTransformOp::CapitalizeInitial => capitalize(v),
                        ParameterTransformOp::Quoted
                        | ParameterTransformOp::PossiblyQuoteWithArraysExpanded { .. } => quote(v),
                        ParameterTransformOp::ExpandEscapeSequences => ansi_c(v),
                        ParameterTransformOp::ToAssignmentLogic => format!("{name}={}", quote(v)),
                        ParameterTransformOp::PromptExpand => self.prompt(v),
                        ParameterTransformOp::ToAttributeFlags => self.attribute_flags(&name),
                    };
                }
                self.emit(value, quoted, b);
            }
            ParameterExpr::UppercaseFirstChar {
                parameter,
                indirect,
                ..
            }
            | ParameterExpr::UppercasePattern {
                parameter,
                indirect,
                ..
            }
            | ParameterExpr::LowercaseFirstChar {
                parameter,
                indirect,
                ..
            }
            | ParameterExpr::LowercasePattern {
                parameter,
                indirect,
                ..
            } => {
                let mut value = self.param(parameter, *indirect)?;
                for v in &mut value.values {
                    *v = match expr {
                        ParameterExpr::UppercaseFirstChar { .. } => change_first(v, true),
                        ParameterExpr::UppercasePattern { .. } => v.to_uppercase(),
                        ParameterExpr::LowercaseFirstChar { .. } => change_first(v, false),
                        _ => v.to_lowercase(),
                    };
                }
                self.emit(value, quoted, b);
            }
            ParameterExpr::ReplaceSubstring {
                parameter,
                indirect,
                pattern,
                replacement,
                match_kind,
            } => {
                let mut value = self.param(parameter, *indirect)?;
                self.check_unset(parameter, &value)?;
                let pat = self.expand_pattern(pattern)?;
                let with = match replacement {
                    Some(r) => self.expand_word(r)?,
                    None => String::new(),
                };
                for v in &mut value.values {
                    *v = replace(v, &pat, &with, match_kind);
                }
                self.emit(value, quoted, b);
            }
            ParameterExpr::VariableNames {
                prefix,
                concatenate,
            } => {
                let names: Vec<String> = self
                    .vars
                    .all()
                    .into_iter()
                    .filter(|(n, v)| n.starts_with(prefix.as_str()) && v.value.is_some())
                    .map(|(n, _)| n.clone())
                    .collect();
                let value = ParamValue {
                    set: true,
                    values: names,
                    at: !concatenate,
                    star: *concatenate,
                };
                self.emit(value, quoted, b);
            }
            ParameterExpr::MemberKeys {
                variable_name,
                concatenate,
            } => {
                let keys = self
                    .vars
                    .get(variable_name)
                    .and_then(|v| v.value.as_ref())
                    .map(Value::keys)
                    .unwrap_or_default();
                let value = ParamValue {
                    set: true,
                    values: keys,
                    at: !concatenate,
                    star: *concatenate,
                };
                self.emit(value, quoted, b);
            }
        }
        Ok(())
    }

    /// `${x:offset:length}`: a slice of a string, of the positional
    /// parameters or of an array.
    fn substring(
        &mut self,
        parameter: &Parameter,
        indirect: bool,
        offset: &brush_parser::ast::UnexpandedArithmeticExpr,
        length: Option<&brush_parser::ast::UnexpandedArithmeticExpr>,
    ) -> Result<ParamValue, Flow> {
        let mut value = self.param(parameter, indirect)?;
        self.check_unset(parameter, &value)?;
        let offset = self.arith_text(&offset.value)?;
        let length = match length {
            Some(l) => Some(self.arith_text(&l.value)?),
            None => None,
        };
        let sparse = match parameter {
            Parameter::NamedWithAllIndices { name, .. } => {
                match self.vars.get(name).and_then(|v| v.value.clone()) {
                    Some(Value::Indexed(map)) => Some(map),
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(map) = sparse {
            // An array's slice counts by index, not by position:
            // `${a[@]:15:2}` is the two elements from index 15 on.
            let last = map.keys().next_back().copied().unwrap_or(-1);
            let start = if offset < 0 {
                last.saturating_add(1).saturating_add(offset)
            } else {
                offset
            };
            let taken = map.range(start.max(0)..).map(|(_, v)| v.clone());
            value.values = match length {
                Some(l) if l >= 0 => taken.take(usize::try_from(l).unwrap_or(0)).collect(),
                Some(_) => {
                    self.error("substring expression < 0");
                    return Err(Flow::Fatal(1));
                }
                None => taken.collect(),
            };
        } else if value.at || value.star {
            // A slice of the positional parameters or an array's
            // elements: `${@:2}` counts `$0` as offset 0.
            let mut items = value.values.clone();
            if matches!(
                parameter,
                Parameter::Special(SpecialParameter::AllPositionalParameters { .. })
            ) {
                items.insert(0, self.arg0.clone());
            }
            value.values = slice(&items, offset, length).to_vec();
        } else {
            for v in &mut value.values {
                let chars: Vec<char> = v.chars().collect();
                *v = slice(&chars, offset, length).iter().collect();
            }
        }
        Ok(value)
    }

    /// Expand an operator's word (`${x:-word}`) in the quoting the whole
    /// expansion is in.
    fn expand_operand(&mut self, text: &str, quoted: bool, b: &mut Builder) -> Result<(), Flow> {
        let pieces = self.parse_word(text, false)?;
        if quoted {
            // Inside double quotes the operand is double-quoted text: its own
            // double quotes nest, its single quotes are characters (what they
            // hold is still expanded), `~` is a character, and a backslash
            // only escapes `$`, `` ` ``, `"` and `\`.
            b.push("", true, false);
            for p in &pieces {
                match &p.piece {
                    WordPiece::SingleQuotedText(t) => {
                        let inner = self.expand_string(t)?;
                        b.push(format!("'{inner}'"), true, false);
                    }
                    WordPiece::Text(t) => b.push(t.clone(), true, false),
                    WordPiece::TildeExpansion(_) => {
                        b.push(
                            text.get(p.start_index..p.end_index).unwrap_or("~"),
                            true,
                            false,
                        );
                    }
                    WordPiece::EscapeSequence(e) => {
                        let c = e.chars().nth(1).unwrap_or('\\');
                        if matches!(c, '$' | '`' | '"' | '\\') {
                            b.push(c.to_string(), true, false);
                        } else {
                            b.push(e.clone(), true, false);
                        }
                    }
                    other => self.piece(other, true, b)?,
                }
            }
            return Ok(());
        }
        for p in &pieces {
            match &p.piece {
                // Literal text of an unquoted operand is split too.
                WordPiece::Text(t) => b.push(t.clone(), false, true),
                other => self.piece(other, false, b)?,
            }
        }
        Ok(())
    }

    fn emit(&self, value: ParamValue, quoted: bool, b: &mut Builder) {
        if value.at && quoted {
            b.push_words(value.values, true, false);
        } else if value.star && quoted {
            let sep = self
                .ifs()
                .chars()
                .next()
                .map(String::from)
                .unwrap_or_default();
            b.push(value.joined(&sep), true, false);
        } else if value.at || value.star {
            // Unquoted, each element is its own word, and split further.
            for (i, v) in value.values.into_iter().enumerate() {
                if i > 0 {
                    b.break_field();
                }
                b.push(v, false, true);
            }
        } else {
            b.push(value.joined(""), quoted, !quoted);
        }
    }

    fn check_unset(&self, parameter: &Parameter, value: &ParamValue) -> Result<(), Flow> {
        if self.options.nounset && !value.set && !value.at && !value.star {
            self.error(format!("{}: unbound variable", param_name(parameter)));
            return Err(if self.interactive {
                Flow::Fatal(1)
            } else {
                Flow::Exit(1)
            });
        }
        Ok(())
    }

    fn param(&mut self, parameter: &Parameter, indirect: bool) -> Result<ParamValue, Flow> {
        let value = self.param_direct(parameter)?;
        if !indirect {
            return Ok(value);
        }
        let name = value.joined("");
        if name.is_empty() {
            return Ok(ParamValue::single(None));
        }
        let target = word::parse_parameter(&name, &Self::parser_options()).map_err(|_| {
            self.error(format!("{name}: invalid indirect expansion"));
            Flow::Fatal(1)
        })?;
        self.param_direct(&target)
    }

    fn param_direct(&mut self, parameter: &Parameter) -> Result<ParamValue, Flow> {
        Ok(match parameter {
            Parameter::Positional(0) => ParamValue::single(Some(self.arg0.clone())),
            Parameter::Positional(n) => {
                let i = usize::try_from(*n).unwrap_or(usize::MAX).saturating_sub(1);
                ParamValue::single(self.positional.get(i).cloned())
            }
            Parameter::Special(special) => match special {
                SpecialParameter::AllPositionalParameters { concatenate } => ParamValue {
                    values: self.positional.clone(),
                    at: !concatenate,
                    star: *concatenate,
                    set: !self.positional.is_empty(),
                },
                SpecialParameter::PositionalParameterCount => {
                    ParamValue::single(Some(self.positional.len().to_string()))
                }
                SpecialParameter::LastExitStatus => {
                    ParamValue::single(Some(self.last_status.to_string()))
                }
                SpecialParameter::CurrentOptionFlags => {
                    ParamValue::single(Some(self.option_flags()))
                }
                SpecialParameter::ProcessId => {
                    ParamValue::single(Some(self.host.pid().to_string()))
                }
                SpecialParameter::LastBackgroundProcessId => {
                    ParamValue::single(self.last_background.map(|pid| pid.to_string()))
                }
                SpecialParameter::ShellName => ParamValue::single(Some(self.arg0.clone())),
            },
            Parameter::Named(name) => ParamValue::single(self.named(name)),
            Parameter::NamedWithIndex { name, index } => {
                let value = self.vars.get(name).and_then(|v| v.value.clone());
                let element = match value {
                    Some(Value::Assoc(map)) => {
                        let key = self.expand_word(index)?;
                        map.get(&key).cloned()
                    }
                    Some(Value::Indexed(map)) => {
                        let i = self.arith_text(index)?;
                        map.get(&self.index_from_end(name, i)).cloned()
                    }
                    Some(Value::Scalar(s)) => {
                        let i = self.arith_text(index)?;
                        (i == 0 || i == -1).then_some(s)
                    }
                    None => None,
                };
                ParamValue::single(element)
            }
            Parameter::NamedWithAllIndices { name, concatenate } => {
                let value = self.vars.get(name).and_then(|v| v.value.as_ref());
                ParamValue {
                    set: value.is_some(),
                    values: value.map(Value::elements).unwrap_or_default(),
                    at: !concatenate,
                    star: *concatenate,
                }
            }
        })
    }

    /// The value of the variable `name`, with the shell's own dynamic ones.
    pub(crate) fn named(&mut self, name: &str) -> Option<String> {
        match name {
            "RANDOM" => Some(self.next_random().to_string()),
            "SECONDS" if !self.vars.is_set("SECONDS") => Some("0".into()),
            _ => self.vars.scalar(name),
        }
    }

    fn assign_parameter(&mut self, parameter: &Parameter, value: &str) -> Result<(), Flow> {
        let result = match parameter {
            Parameter::Named(name) => self.vars.set(name, value),
            Parameter::NamedWithIndex { name, index } => {
                if matches!(
                    self.vars.get(name).and_then(|v| v.value.as_ref()),
                    Some(Value::Assoc(_))
                ) {
                    let key = self.expand_word(index)?;
                    self.vars.set_key(name, key, value.to_string())
                } else {
                    let i = self.arith_text(index)?;
                    let i = self.index_from_end(name, i);
                    self.vars.set_index(name, i, value.to_string())
                }
            }
            other => {
                self.error(format!("{}: cannot assign in this way", param_name(other)));
                return Err(Flow::Fatal(1));
            }
        };
        result.map_err(|e| {
            self.error(e);
            Flow::Fatal(1)
        })
    }

    pub(crate) fn option_flags(&self) -> String {
        let mut flags = String::new();
        for (on, c) in [
            (self.options.errexit, 'e'),
            (self.options.noglob, 'f'),
            (self.options.nounset, 'u'),
            (self.options.xtrace, 'x'),
            (self.options.noclobber, 'C'),
            (self.interactive, 'i'),
        ] {
            if on {
                flags.push(c);
            }
        }
        flags
    }

    fn attribute_flags(&self, name: &str) -> String {
        let Some(var) = self.vars.get(name) else {
            return String::new();
        };
        let mut flags = String::new();
        match var.value {
            Some(Value::Indexed(_)) => flags.push('a'),
            Some(Value::Assoc(_)) => flags.push('A'),
            _ => {}
        }
        if var.integer {
            flags.push('i');
        }
        if var.readonly {
            flags.push('r');
        }
        if var.exported {
            flags.push('x');
        }
        flags
    }
}

impl Shell {
    /// Whether an operator's test (`-` or `:-`) finds the parameter absent.
    /// A quoted `"${*:-x}"` asks about `$*` joined, as it would expand.
    fn absent(&self, value: &ParamValue, test: &ParameterTestType, quoted: bool) -> bool {
        match test {
            ParameterTestType::Unset => !value.set,
            ParameterTestType::UnsetOrNull if value.star && quoted => {
                let sep = self
                    .ifs()
                    .chars()
                    .next()
                    .map(String::from)
                    .unwrap_or_default();
                value.joined(&sep).is_empty()
            }
            ParameterTestType::UnsetOrNull => value.is_null(),
        }
    }
}

fn param_name(parameter: &Parameter) -> String {
    match parameter {
        Parameter::Positional(n) => n.to_string(),
        Parameter::Special(s) => s.to_string(),
        Parameter::Named(name)
        | Parameter::NamedWithIndex { name, .. }
        | Parameter::NamedWithAllIndices { name, .. } => name.clone(),
    }
}

fn brace_expand(text: &str) -> Vec<String> {
    if !text.contains('{') {
        return vec![text.to_string()];
    }
    crate::brace::expand(text)
}

fn remove_affix(value: &str, pat: &Pattern, suffix: bool, largest: bool) -> String {
    let chars: Vec<char> = value.chars().collect();
    let n = chars.len();
    let lengths: Vec<usize> = if largest {
        (0..=n).rev().collect()
    } else {
        (0..=n).collect()
    };
    for len in lengths {
        let (affix, rest) = if suffix {
            let start = n.saturating_sub(len);
            (chars.get(start..), chars.get(..start))
        } else {
            (chars.get(..len), chars.get(len..))
        };
        if let (Some(affix), Some(rest)) = (affix, rest)
            && pat.matches_chars(affix)
        {
            return rest.iter().collect();
        }
    }
    value.to_string()
}

fn replace(value: &str, pat: &Pattern, with: &str, kind: &SubstringMatchKind) -> String {
    let chars: Vec<char> = value.chars().collect();
    let n = chars.len();
    match kind {
        SubstringMatchKind::Prefix => {
            for len in (0..=n).rev() {
                if chars.get(..len).is_some_and(|p| pat.matches_chars(p)) {
                    let rest: String = chars.get(len..).unwrap_or(&[]).iter().collect();
                    return format!("{with}{rest}");
                }
            }
            value.to_string()
        }
        SubstringMatchKind::Suffix => {
            for start in 0..=n {
                if chars.get(start..).is_some_and(|p| pat.matches_chars(p)) {
                    let head: String = chars.get(..start).unwrap_or(&[]).iter().collect();
                    return format!("{head}{with}");
                }
            }
            value.to_string()
        }
        SubstringMatchKind::FirstOccurrence | SubstringMatchKind::Anywhere => {
            let all = matches!(kind, SubstringMatchKind::Anywhere);
            let mut out = String::new();
            let mut i = 0_usize;
            let mut replaced = false;
            while i < n {
                let found = if replaced && !all {
                    None
                } else {
                    (i.saturating_add(1)..=n)
                        .rev()
                        .find(|&end| chars.get(i..end).is_some_and(|p| pat.matches_chars(p)))
                };
                if let Some(end) = found {
                    out.push_str(with);
                    i = end;
                    replaced = true;
                } else {
                    if let Some(c) = chars.get(i) {
                        out.push(*c);
                    }
                    i = i.saturating_add(1);
                }
            }
            out
        }
    }
}

fn slice<T>(items: &[T], offset: i64, length: Option<i64>) -> &[T] {
    let n = i64::try_from(items.len()).unwrap_or(i64::MAX);
    let start = if offset < 0 {
        n.saturating_add(offset)
    } else {
        offset
    };
    if start < 0 || start > n {
        return &[];
    }
    let end = match length {
        None => n,
        Some(l) if l < 0 => n.saturating_add(l),
        Some(l) => start.saturating_add(l).min(n),
    };
    if end < start {
        return &[];
    }
    let start = usize::try_from(start).unwrap_or(0);
    let end = usize::try_from(end).unwrap_or(0);
    items.get(start..end).unwrap_or(&[])
}

fn capitalize(value: &str) -> String {
    let mut out = String::new();
    let mut start = true;
    for c in value.chars() {
        if start && c.is_alphanumeric() {
            out.extend(c.to_uppercase());
            start = false;
        } else {
            out.extend(c.to_lowercase());
            if !c.is_alphanumeric() {
                start = true;
            }
        }
    }
    out
}

fn change_first(value: &str, upper: bool) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(c) if upper => c.to_uppercase().chain(chars).collect(),
        Some(c) => c.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// `value` quoted so the shell reads it back as itself.
pub(crate) fn quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./=:,+@%".contains(c))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Inside backquotes, `\$`, `` \` `` and `\\` stand for the character, and
/// `\"` too when the backquotes are inside double quotes.
fn unbackquote(text: &str, in_quotes: bool) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\'
            && let Some(&next) = chars.peek()
            && (matches!(next, '$' | '`' | '\\') || (in_quotes && next == '"'))
        {
            out.push(next);
            chars.next();
            continue;
        }
        out.push(c);
    }
    out
}

/// The escapes of `$'…'` and `echo -e`, as text.
pub(crate) fn ansi_c(text: &str) -> String {
    String::from_utf8_lossy(&ansi_c_bytes(text)).into_owned()
}

/// The escapes of `$'…'`, `echo -e` and `printf`, as bytes: `\xHH` and
/// octal name a byte, `\u` a character.
pub(crate) fn ansi_c_bytes(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let put = |out: &mut Vec<u8>, c: char| {
        let mut buf = [0_u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    };
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            put(&mut out, c);
            continue;
        }
        let Some(e) = chars.next() else {
            out.push(b'\\');
            break;
        };
        match e {
            'n' => out.push(b'\n'),
            't' => out.push(b'\t'),
            'r' => out.push(b'\r'),
            'a' => out.push(0x07),
            'b' => out.push(0x08),
            'e' | 'E' => out.push(0x1b),
            'f' => out.push(0x0c),
            'v' => out.push(0x0b),
            '\\' => out.push(b'\\'),
            '\'' => out.push(b'\''),
            '"' => out.push(b'"'),
            '?' => out.push(b'?'),
            'x' => match digits(&mut chars, 16, 2) {
                Some(n) => out.push(u8::try_from(n & 0xff).unwrap_or(0)),
                None => out.extend_from_slice(b"\\x"),
            },
            'u' | 'U' => {
                let max = if e == 'u' { 4 } else { 8 };
                if let Some(ch) = digits(&mut chars, 16, max).and_then(char::from_u32) {
                    put(&mut out, ch);
                } else {
                    out.push(b'\\');
                    put(&mut out, e);
                }
            }
            '0'..='7' => {
                let mut n = e.to_digit(8).unwrap_or(0);
                for _ in 0..2 {
                    match chars.peek().and_then(|d| d.to_digit(8)) {
                        Some(d) => {
                            n = n.wrapping_mul(8).wrapping_add(d);
                            chars.next();
                        }
                        None => break,
                    }
                }
                out.push(u8::try_from(n & 0xff).unwrap_or(0));
            }
            'c' => {
                if let Some(x) = chars.next() {
                    out.push(u8::try_from(u32::from(x) & 0x1f).unwrap_or(0));
                }
            }
            other => {
                out.push(b'\\');
                put(&mut out, other);
            }
        }
    }
    out
}

fn digits(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    radix: u32,
    max: usize,
) -> Option<u32> {
    let mut text = String::new();
    while text.len() < max && chars.peek().is_some_and(|d| d.is_digit(radix)) {
        text.extend(chars.next());
    }
    if text.is_empty() {
        return None;
    }
    u32::from_str_radix(&text, radix).ok()
}
