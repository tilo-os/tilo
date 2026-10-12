//! What one keystroke costs in a large document: 1, 5 and 20 MB of prose.
//!
//! A keystroke's cost must not grow with the document. This opens documents
//! of increasing size and times each typed character end to end — the
//! action, the document refresh, layout and drawing — and prints how the mean
//! and the tail move with size. It measures the editor hosted on the app
//! framework: the record, permission checks, attributed undo and the
//! incrementally refreshed state are all on the path.
//!
//! ```text
//! cargo run -p word-processor --release --example large_document
//! cargo run -p word-processor --release --example large_document -- --sizes 1,5 --chars 100
//! ```
//!
//! Release only: a debug build is slow enough to make every reading
//! meaningless.

use std::path::PathBuf;

use ui::framework::testing::HeadlessApp;
use word_processor::framework::WordProcessorApp;

const PARAGRAPH: &str = "The rehearsal ran long and the doors opened late, so the \
first half went out to a room that was still finding its seats. Nobody minded much. \
By the interval the balcony had filled and the noise off it was the good kind.";

fn document_of(megabytes: usize) -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir().join("word-processor-large-document");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("body-{megabytes}mb.md"));
    if !path.exists() {
        let target = megabytes.saturating_mul(1_000_000);
        let mut body = String::with_capacity(target.saturating_add(PARAGRAPH.len()));
        let mut n = 0usize;
        while body.len() < target {
            n = n.saturating_add(1);
            if n.is_multiple_of(40) {
                body.push_str("## Section ");
                body.push_str(&n.to_string());
                body.push_str("\n\n");
            }
            body.push_str(PARAGRAPH);
            body.push_str(" **Bold words** and *emphasis* keep marks in play.\n\n");
        }
        std::fs::write(&path, &body)?;
    }
    Ok(path)
}

fn micros(elapsed: std::time::Duration) -> u64 {
    u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX)
}

#[allow(
    clippy::cast_precision_loss,
    reason = "microsecond timings never approach 2^53"
)]
fn ms(us: u64) -> f64 {
    us as f64 / 1000.0
}

/// Something a keystroke can be typed into.
trait Editor {
    fn key(&mut self, key: &str);
    /// Where one keystroke's time goes, when the editor can tell.
    fn passes(&mut self, key: &str) -> Option<String> {
        let _ = key;
        None
    }
    /// Paragraphs, and characters in them: proof the keystrokes landed.
    fn size(&mut self) -> (usize, usize);
}

fn measure(doc: &automerge::AutoCommit) -> (usize, usize) {
    let Ok(c) = word_processor::model::open(doc) else {
        return (0, 0);
    };
    let ids = tilo_doc::model::blocks_in_order(doc, &c);
    let chars = ids
        .iter()
        .map(|id| tilo_doc::model::block_text(doc, &c, id).map_or(0, |t| t.chars().count()))
        .sum();
    (ids.len(), chars)
}

impl Editor for HeadlessApp<WordProcessorApp> {
    fn key(&mut self, key: &str) {
        self.harness().key(key);
    }
    fn passes(&mut self, key: &str) -> Option<String> {
        let harness = self.harness();
        harness.begin_profiling();
        harness.clear_profile_samples();
        let scans = tilo_doc::model::ordered_keys_scan_count();
        let started = ui::time::now();
        harness.key(key);
        let wall = micros(started.elapsed());
        let scans = tilo_doc::model::ordered_keys_scan_count().saturating_sub(scans);
        let frames = harness.take_profile_samples();
        let sum =
            |pick: fn(&ui::profiling::FrameTiming) -> u64| frames.iter().map(pick).sum::<u64>();
        Some(format!(
            "wall {:.2}  event {:.2}  sync {:.2}  build {:.2}  apply {:.2}  draw {:.2}  ({} frames, {scans} order scans)",
            ms(wall),
            ms(sum(|f| f.handle_event_us)),
            ms(sum(|f| f.sync_us)),
            ms(sum(|f| f.build_layout_us)),
            ms(sum(|f| f.apply_layout_us)),
            ms(sum(|f| f.draw_us)),
            frames.len(),
        ))
    }
    fn size(&mut self) -> (usize, usize) {
        self.read_document(measure)
    }
}

/// The framework-hosted editor on a new record, with the document pasted
/// into it through the editor's own paste action, ten thousand paragraphs a
/// time.
fn open_hosted(path: &std::path::Path) -> std::io::Result<Box<dyn Editor>> {
    use tilo_doc_codecs::FormatProvider;
    use tilo_document::ClipBlock;
    use tilo_document::action::{RegionKind, RichTextAction};
    use ui::tilo_resource::Invoke;
    use word_processor::framework::Document;

    let failed = |error: String| std::io::Error::other(error);
    let bytes = std::fs::read(path)?;
    let mut scratch = tilo_doc::Document::new();
    tilo_doc_codecs::MarkdownCodec
        .import(
            &bytes,
            &mut scratch.as_mut(),
            &mut tilo_content::ContentInputs::new(),
        )
        .map_err(|error| failed(error.to_string()))?;
    let clips: Vec<ClipBlock> = scratch
        .materialize()
        .blocks
        .into_iter()
        .map(|block| ClipBlock {
            level: match block.attrs.get("level") {
                Some(tilo_doc::AttrVal::Int(level)) => Some(*level),
                _ => None,
            },
            kind: block.kind,
            text: block.text,
            marks: block.marks,
            ordered: None,
            align: None,
            attrs: std::collections::BTreeMap::default(),
        })
        .collect();

    let mut app =
        HeadlessApp::<WordProcessorApp>::new().map_err(|error| failed(error.to_string()))?;
    for chunk in clips.chunks(10_000) {
        let (block, at) = app
            .read_document(|doc| {
                let c = word_processor::model::open(doc).ok()?;
                let last = tilo_doc::model::blocks_in_order(doc, &c).pop()?;
                let len =
                    tilo_doc::model::block_text(doc, &c, &last).map_or(0, |t| t.chars().count());
                Some((last, len))
            })
            .ok_or_else(|| failed("the record holds no paragraph".into()))?;
        let paste = RichTextAction::PasteFragment {
            region: RegionKind::Body,
            start_block: block.clone(),
            start: at,
            end_block: block,
            end: at,
            blocks: chunk.to_vec(),
            adopt_first_style: false,
        };
        let invoke = Invoke::<Document>::edit(&word_processor::model::edit(&paste))
            .map_err(|error| failed(error.to_string()))?;
        app.invoke_confirmed(invoke)
            .map_err(|error| failed(error.to_string()))?;
    }
    Ok(Box::new(app))
}

fn main() -> std::io::Result<()> {
    ui::i18n::I18nManager::init("en-US");
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i.saturating_add(1)))
            .cloned()
    };
    let sizes: Vec<usize> = arg("--sizes")
        .unwrap_or_else(|| "1,5,20".into())
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    let chars: usize = arg("--chars").and_then(|v| v.parse().ok()).unwrap_or(200);

    println!("framework-hosted editor: per keystroke, ms");
    println!("size   open(s)   keystroke mean      p50      p95      max   enter mean");
    for megabytes in sizes {
        let path = document_of(megabytes)?;
        let opened = ui::time::now();
        let mut editor = open_hosted(&path)?;
        for ch in "warmup".chars() {
            editor.key(&ch.to_string());
        }
        let open_s = opened.elapsed().as_secs_f64();
        let (paragraphs, before) = editor.size();
        if let Some(passes) = editor.passes("z") {
            println!("       one keystroke: {passes}");
        }
        if let Some(passes) = editor.passes("Enter") {
            println!("       one Enter:     {passes}");
        }
        // The keystroke probe above typed one character.
        let before = before.saturating_add(1);

        let mut per_key = Vec::with_capacity(chars);
        for ch in "the quick brown fox jumps over the lazy dog "
            .chars()
            .cycle()
            .take(chars)
        {
            let started = ui::time::now();
            editor.key(&ch.to_string());
            per_key.push(micros(started.elapsed()));
        }
        let mut enters = Vec::new();
        for _ in 0..10 {
            let started = ui::time::now();
            editor.key("Enter");
            enters.push(micros(started.elapsed()));
            editor.key("x");
        }
        let (_, after) = editor.size();
        if after < before.saturating_add(chars) {
            return Err(std::io::Error::other(format!(
                "the keystrokes did not land: {before} -> {after} characters"
            )));
        }
        let mean = |v: &[u64]| {
            v.iter()
                .sum::<u64>()
                .checked_div(u64::try_from(v.len()).unwrap_or(u64::MAX))
                .unwrap_or(0)
        };
        let mut sorted = per_key.clone();
        sorted.sort_unstable();
        let at = |p: usize| {
            sorted
                .get(sorted.len().saturating_mul(p) / 100)
                .copied()
                .unwrap_or(0)
        };
        println!(
            "{megabytes:>3} MB  {open_s:>7.1}   {:>13.2}  {:>7.2}  {:>7.2}  {:>7.2}  {:>11.2}   ({paragraphs} paragraphs)",
            ms(mean(&per_key)),
            ms(at(50)),
            ms(at(95)),
            ms(sorted.last().copied().unwrap_or(0)),
            ms(mean(&enters)),
        );
    }
    Ok(())
}
