//! What entering a notebook the size of a company's costs, in process: the
//! work a page does on the main thread when a workspace's notebook arrives,
//! timed on this machine.
//!
//! The notebook is written with Notes' own `import-markdown-files` action:
//! `NOTES` long notes (headings, paragraphs, lists, code and tables, 6–14
//! sections each), `BATCH` notes per change. It prints, in milliseconds:
//!
//! * **load**: loading the saved notebook as a peer's bytes are loaded, every
//!   change rebuilt and hashed (what the page's record worker now does);
//! * **load with hashes**: loading it with the change hashes that load
//!   computed (what is left on the page's main thread);
//! * **note dates**: the open note's created and edited time;
//! * **history**: the first read of the notebook's newest 16 changes, as the
//!   page's activity reads what its person did.
//!
//! ```text
//! cargo run --release -p notes --example notes-scale -- 100 1     # 1×: a change per note
//! cargo run --release -p notes --example notes-scale -- 1000 50   # 10×: imported 50 at a time
//! ```

#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::expect_used,
    reason = "a benchmark that writes its own ASCII notebook and stops at the first failure"
)]

use notes::notebook::{self, MarkdownFile, Notebook};
use std::time::Instant;
use ui::tilo_resource::Replica;

/// A small deterministic generator, so every run writes the same notebook.
struct Words(u64);

const WORDS: &[&str] = &[
    "relay",
    "workspace",
    "record",
    "sync",
    "release",
    "alpha",
    "onboarding",
    "agent",
    "browser",
    "page",
    "desk",
    "tasks",
    "notes",
    "index",
    "search",
    "history",
    "peer",
    "device",
    "identity",
    "invite",
    "member",
    "billing",
    "storage",
    "folder",
    "upload",
    "policy",
    "grant",
    "action",
    "view",
    "window",
];

impl Words {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn between(&mut self, low: u64, high: u64) -> u64 {
        low + self.next() % (high - low + 1)
    }

    fn sentence(&mut self, words: u64) -> String {
        let mut out: Vec<String> = (0..words)
            .map(|_| {
                WORDS[usize::try_from(self.next() % WORDS.len() as u64).unwrap_or(0)].to_owned()
            })
            .collect();
        if let Some(first) = out.first_mut() {
            *first = first[..1].to_uppercase() + &first[1..];
        }
        out.join(" ") + "."
    }

    fn paragraph(&mut self, sentences: u64) -> String {
        (0..sentences)
            .map(|_| {
                let words = self.between(8, 18);
                self.sentence(words)
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn note(&mut self, at: usize) -> String {
        let mut out = Vec::new();
        for section in 0..self.between(6, 14) {
            let heading = self.sentence(4);
            let sentences = self.between(3, 7);
            let paragraph = self.paragraph(sentences);
            let items = [self.sentence(6), self.sentence(7), self.sentence(5)];
            out.push(format!(
                "## {}\n\n{paragraph}\n\n- {}\n- {}\n- {}\n",
                heading.trim_end_matches('.'),
                items[0],
                items[1],
                items[2]
            ));
            if section % 3 == 0 {
                out.push(format!("```sh\ntilo action run {at} {section}\n```\n"));
            }
            if section % 4 == 1 {
                out.push("| a | b | c |\n|---|---|---|\n| 1 | 2 | 3 |\n| x | y | z |\n".to_owned());
            }
        }
        out.join("\n")
    }
}

fn ms(began: Instant) -> f64 {
    began.elapsed().as_secs_f64() * 1000.0
}

fn main() {
    ui::i18n::I18nManager::init("en-US");
    let mut args = std::env::args().skip(1);
    let notes: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(100);
    let batch: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(1).max(1);
    let mut words = Words(7);
    let mut notebook = Replica::<Notebook>::new();
    let mut written = 0;
    while written < notes {
        let files: Vec<MarkdownFile> = (written..(written + batch).min(notes))
            .map(|at| MarkdownFile {
                path: format!("wiki/{at}.md"),
                title: Some(format!("Note {at}")),
                markdown: words.note(at),
                origin: None,
            })
            .collect();
        written += files.len();
        notebook
            .apply(
                Notebook::IMPORT_MARKDOWN_FILES
                    .with(notebook::notebook::ImportMarkdownFiles { files }),
            )
            .expect("the notes import");
    }
    let bytes = notebook.save();
    let doc = notebook.doc().clone();
    let history = doc.committed_document();
    println!(
        "{notes} notes, {batch} a change: {} bytes, {} changes, {} ops",
        bytes.len(),
        history.change_hashes().len(),
        automerge::ReadDoc::stats(history).num_ops
    );

    let began = Instant::now();
    let loaded = automerge::Automerge::load(&bytes).expect("it loads");
    println!("load              {:8.1}", ms(began));
    let hashes = loaded.change_hashes().to_vec();
    let began = Instant::now();
    automerge::Automerge::load_with_options(
        &bytes,
        automerge::LoadOptions::new().known_change_hashes(&hashes),
    )
    .expect("it loads");
    println!("load with hashes  {:8.1}", ms(began));

    let page = notes::read::list_pages(&doc)[notes / 2].id;
    let began = Instant::now();
    let mut dates = notes::metadata::NoteDateIndex::default();
    dates.sync(&doc, Some(page));
    println!("note dates        {:8.1}", ms(began));

    let mut copy = doc.clone();
    let began = Instant::now();
    let mut log = ui::history_source::HistoryLog::default();
    ui::HistorySource::new()
        .without_record_genesis()
        .with_max_entries(16)
        .entries_since(&mut copy, &mut log);
    println!("history           {:8.1}", ms(began));
}
