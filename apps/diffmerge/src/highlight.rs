//! Syntax colouring for the diff and merge panels. Tokenizing is the
//! toolkit's grammar-backed highlighter
//! ([`ui::widget::code_editor::SyntaxHighlighter`]), chosen from the file name
//! a panel shows; colours come from the theme's shared syntax palette, so code
//! reads exactly as it does in every other code surface. What stays here is
//! specific to diffing: each side of a comparison is tokenized as one
//! continuous document (so a block comment or a multi-line string colours
//! correctly across aligned rows), and a line paints as coloured runs at a
//! panel's own origin and size.

use std::path::Path;
use std::sync::Arc;

use ui::paint::PaintCtx;
use ui::tokens::TextRole;
use ui::widget::code_editor::{Span, SyntaxHighlighter};
use vello::kurbo::Point;

/// A panel label as the file it names and the revision it shows: the label
/// `rate_limiter.rs (working tree)` is `("rate_limiter.rs", Some("working tree"))`.
pub fn split_label(label: &str) -> (&str, Option<&str>) {
    let label = label.trim();
    match label.rsplit_once(" (") {
        Some((name, rest)) if rest.ends_with(')') => {
            (name.trim_end(), rest.strip_suffix(')').map(str::trim))
        }
        _ => (label, None),
    }
}

/// The file a panel label names: the label without its revision.
pub fn file_name(label: &str) -> &str {
    split_label(label).0
}

/// The grammar for a panel label such as `"rate_limiter.rs (working tree)"`
/// or a path: its file extension, else its whole file name (`Dockerfile`),
/// else plain text.
pub fn grammar_for_label(label: &str) -> SyntaxHighlighter {
    let path = Path::new(file_name(label));
    path.extension()
        .and_then(|ext| ext.to_str())
        .and_then(SyntaxHighlighter::for_token)
        .or_else(|| {
            path.file_name()
                .and_then(|file| file.to_str())
                .and_then(SyntaxHighlighter::for_token)
        })
        .unwrap_or_else(SyntaxHighlighter::plain_text)
}

/// Spans for each line of one continuous document, in order.
pub fn document<'a>(
    grammar: &SyntaxHighlighter,
    lines: impl IntoIterator<Item = &'a str>,
) -> Vec<Vec<Span>> {
    shared_document(grammar, lines)
        .into_iter()
        .map(|spans| spans.to_vec())
        .collect()
}

/// Continuous tokenization with a bounded cache for repeated source lines.
/// Reuse requires the exact incoming parser state as well as the line, so
/// identical text inside a comment or multiline string never gets code colours.
/// Shared immutable spans avoid allocating the same token runs for each copy.
pub fn shared_document<'a>(
    grammar: &SyntaxHighlighter,
    lines: impl IntoIterator<Item = &'a str>,
) -> Vec<Arc<[Span]>> {
    let mut state = grammar.start();
    let mut previous: Option<(
        &str,
        ui::widget::code_editor::SyntaxState,
        ui::widget::code_editor::SyntaxState,
        Arc<[Span]>,
    )> = None;
    lines
        .into_iter()
        .map(|line| {
            if let Some((old_line, incoming, outgoing, spans)) = &previous
                && *old_line == line
                && *incoming == state
            {
                state = outgoing.clone();
                return Arc::clone(spans);
            }
            let incoming = state.clone();
            let spans: Arc<[Span]> = state.highlight_line(line).into();
            previous = Some((line, incoming, state.clone(), Arc::clone(&spans)));
            spans
        })
        .collect()
}

/// A large source's continuous syntax pass, streamed through the host's
/// background executor. Replacing a comparison drops the receiver and stops
/// further chunks; no stale results can colour the replacement.
pub struct BackgroundDocument {
    receiver: std::sync::mpsc::Receiver<SyntaxChunk>,
    background: ui::rt::background::Background,
    wake: ui::WakeHandle,
}

struct SyntaxChunk {
    source: Arc<str>,
    state: ui::widget::code_editor::SyntaxState,
    offset: usize,
    spans: Vec<Arc<[Span]>>,
}

impl SyntaxChunk {
    fn parse(mut self) -> Self {
        // One queued unit is bounded by eight complete source lines. Keeping
        // the exact parser state makes comments/strings continuous even in a
        // single-threaded browser's background executor.
        self.spans.clear();
        for line in self
            .source
            .get(self.offset..)
            .unwrap_or_default()
            .split_inclusive('\n')
            .take(8)
        {
            self.offset = self.offset.saturating_add(line.len());
            let line = line.strip_suffix('\n').unwrap_or(line);
            let line = line.strip_suffix('\r').unwrap_or(line);
            self.spans.push(Arc::from(self.state.highlight_line(line)));
        }
        self
    }
}

impl BackgroundDocument {
    pub fn start(label: &str, text: &str, wake: ui::WakeHandle) -> Option<Self> {
        let chunk = SyntaxChunk {
            source: Arc::from(text),
            state: grammar_for_label(label).start(),
            offset: 0,
            spans: Vec::new(),
        };
        let background = ui::rt::background::Background::current();
        let receiver = Self::queue(&background, &wake, chunk)?;
        Some(Self {
            receiver,
            background,
            wake,
        })
    }

    fn queue(
        background: &ui::rt::background::Background,
        wake: &ui::WakeHandle,
        chunk: SyntaxChunk,
    ) -> Option<std::sync::mpsc::Receiver<SyntaxChunk>> {
        let (sender, receiver) = std::sync::mpsc::channel();
        let wake = wake.clone();
        background
            .spawn("diffmerge-syntax", move || {
                if sender.send(chunk.parse()).is_ok() {
                    wake.wake();
                }
            })
            .ok()?;
        Some(receiver)
    }

    /// Admit one completed chunk and queue the next. Nothing waits on the
    /// UI thread; dropping the job prevents scheduling any further chunks.
    pub fn collect(&mut self, spans: &mut Vec<Arc<[Span]>>) -> bool {
        match self.receiver.try_recv() {
            Ok(mut chunk) => {
                spans.append(&mut chunk.spans);
                if chunk.offset >= chunk.source.len() {
                    return false;
                }
                let Some(receiver) = Self::queue(&self.background, &self.wake, chunk) else {
                    return false;
                };
                self.receiver = receiver;
                true
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => false,
        }
    }
}

/// Paint `text` at `origin` (the top of its line box) in the `Code` role as
/// runs coloured by `spans`. A line with no spans (its side was never
/// tokenized) paints as body text.
pub fn draw_line(ctx: &mut PaintCtx<'_>, origin: Point, text: &str, spans: &[Span]) {
    let syntax = &ctx.theme.tokens.syntax;
    let style = ctx.theme.text(TextRole::Code);
    if spans.is_empty() {
        if !text.is_empty() {
            ctx.text(text, &style, syntax.text, origin, None);
        }
        return;
    }
    let mut at = origin;
    for span in spans {
        let Some(run) = text.get(span.range.clone()) else {
            continue;
        };
        if run.is_empty() {
            continue;
        }
        ctx.text(run, &style, syntax.color(span.kind), at, None);
        let (w, _) = ctx.text.measure_style(run, None, &style);
        at.x += f64::from(w);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::widget::code_editor::TokenKind;

    fn kinds(line: &str, spans: &[Span], kind: TokenKind) -> Vec<String> {
        spans
            .iter()
            .filter(|span| span.kind == kind)
            .filter_map(|span| line.get(span.range.clone()))
            .map(str::to_owned)
            .collect()
    }

    /// Syntax highlighting: the grammar follows the file a label names, token kinds
    /// cover a line, and a side is highlighted as one document. Each phase is a
    /// former test with its own inputs.
    #[test]
    fn highlighting_picks_grammar_by_label_and_spans_whole_documents() {
        the_grammar_follows_the_file_a_label_names();
        keywords_strings_and_comments_take_their_kinds();
        a_side_is_one_document_so_block_comments_span_rows();
    }

    #[test]
    fn repeated_source_reuses_spans_only_in_the_same_parser_context() {
        let grammar = grammar_for_label("file.rs");
        let lines = [
            "let repeated = 1;",
            "let repeated = 1;",
            "/*",
            "let repeated = 1;",
            "*/",
            "let repeated = 1;",
        ];
        let actual = shared_document(&grammar, lines);
        let mut state = grammar.start();
        for (line, spans) in lines.into_iter().zip(&actual) {
            assert_eq!(spans.as_ref(), state.highlight_line(line));
        }
        let steady = shared_document(&grammar, ["// same comment"; 20]);
        assert!(
            steady
                .windows(2)
                .skip(1)
                .all(|pair| Arc::ptr_eq(&pair[0], &pair[1]))
        );
        assert!(!Arc::ptr_eq(&actual[1], &actual[3]));
    }

    #[test]
    fn background_syntax_is_bounded_continuous_and_replacement_drops_old_work() {
        let pumped = ui::rt::background::Pumped::new();
        let background = pumped.background();
        let _scope = background.enter();
        let lines = std::iter::once("/*")
            .chain(std::iter::repeat_n("let inside = 1;", 30))
            .chain(["*/", "let outside = 2;"])
            .collect::<Vec<_>>();
        let source = lines.join("\n");
        let mut job =
            BackgroundDocument::start("example.rs", &source, ui::WakeHandle::new()).unwrap();
        let mut actual = Vec::new();
        assert_eq!(
            pumped.run_pending(),
            1,
            "one queued chunk never drains the whole source"
        );
        assert!(job.collect(&mut actual));
        assert_eq!(actual.len(), 8, "the first admitted batch is bounded");
        while {
            pumped.run_pending();
            job.collect(&mut actual)
        } {}
        let expected = document(&grammar_for_label("example.rs"), lines.iter().copied());
        assert_eq!(
            actual.iter().map(AsRef::as_ref).collect::<Vec<_>>(),
            expected.iter().map(Vec::as_slice).collect::<Vec<_>>()
        );
        let old = BackgroundDocument::start("old.rs", &source, ui::WakeHandle::new()).unwrap();
        drop(old);
        assert_eq!(pumped.run_pending(), 1);
        assert!(
            pumped.is_idle(),
            "a replaced document queues no continuation"
        );
    }

    fn the_grammar_follows_the_file_a_label_names() {
        assert_eq!(
            grammar_for_label("rate_limiter.rs (working tree)").name(),
            "Rust"
        );
        assert_eq!(grammar_for_label("config.toml (ours)").name(), "TOML");
        assert_eq!(
            file_name("rate_limiter.rs (working tree)"),
            "rate_limiter.rs"
        );
        assert_eq!(file_name("plain.txt"), "plain.txt");
        assert_eq!(
            grammar_for_label("/srv/app/Dockerfile").name(),
            "Dockerfile"
        );
        assert_eq!(grammar_for_label("notes (draft)").name(), "Plain Text");
        assert_eq!(grammar_for_label("").name(), "Plain Text");
    }

    fn keywords_strings_and_comments_take_their_kinds() {
        let grammar = grammar_for_label("a.rs");
        let line = "pub fn f() { let s = \"hi\"; } // done";
        let spans = &document(&grammar, [line])[0];
        assert!(kinds(line, spans, TokenKind::Keyword).contains(&"pub".to_owned()));
        assert_eq!(kinds(line, spans, TokenKind::StringLit), ["\"hi\""]);
        assert_eq!(kinds(line, spans, TokenKind::Comment), ["// done"]);
        let covered: usize = spans.iter().map(|span| span.range.len()).sum();
        assert_eq!(covered, line.len(), "spans cover the whole line");
    }

    fn a_side_is_one_document_so_block_comments_span_rows() {
        let grammar = grammar_for_label("a.rs");
        let lines = ["/* opened", "still inside", "*/ let x = 1;"];
        let spans = document(&grammar, lines);
        assert_eq!(
            kinds(lines[1], &spans[1], TokenKind::Comment),
            ["still inside"],
            "the middle row continues the comment"
        );
        assert!(kinds(lines[2], &spans[2], TokenKind::Keyword).contains(&"let".to_owned()));
    }
}
