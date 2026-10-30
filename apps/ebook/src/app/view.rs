//! The reader as a bound view: [`window`] lays the [`BooksApp`] out over the
//! library the document holds and answers [`BooksEffect`]s on it. What it
//! publishes, and what finishes after an effect returned, goes through a
//! [`Store`] the reader shares with the view.

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::{EffectAnswer, EffectCx, Effected, LiveRow, Publishes, ViewCx};
use ui::tilo_resource::{ActionDecl, SearchEntry};
use ui::widget::Element;

use super::BooksApp;
use crate::actions::BookResourceCtx;
use crate::content;
use crate::model::LibraryEntry;
use crate::resource::{BooksEffect, Ebook};

/// The [`Store`] as the reader and the view share it.
pub(crate) type Shared = Rc<RefCell<Store>>;

/// What the reader, the view's effect completions and what the view
/// publishes share.
#[derive(Default)]
pub(crate) struct Store {
    /// What the reader showed when it last looked.
    pub(crate) shown: Shown,
    /// What effect completions, jobs and a capture asked of the reader since
    /// it last looked, in the order they asked.
    pub(super) asked: Vec<Asked>,
}

/// What the reader showed when it last looked: what the view publishes.
pub(crate) struct Shown {
    /// Where the reader is in the open book.
    pub(super) reading: Option<BookResourceCtx>,
    /// The books, chapters, annotations and remote books search finds.
    pub(super) search: crate::search_provider::BookSearch,
}

impl Default for Shown {
    fn default() -> Self {
        Self {
            reading: None,
            search: crate::search_provider::BookSearch::new(&[], &[]),
        }
    }
}

/// What was asked of the reader, done when it next looks.
pub(super) enum Asked {
    /// A book an effect placed, shelved through the library's action, or
    /// why it could not be opened.
    Opened(Box<Result<content::Placed, String>>),
    /// Books found in the watched folders, whose files this device opens.
    Found(Vec<content::FoundBook>),
    /// The scenario a capture staged.
    #[cfg(feature = "samples")]
    Stage(String),
}

impl Shown {
    /// The book, chapter, annotation or location a search hit names, when
    /// the published page leaves it out.
    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.search.resolve(id).map(LiveRow::from_resource)
    }

    /// The library's edits bound to where the reader is: the page's
    /// bookmark and notes, and the selection's highlight and note.
    fn declarations(&self) -> Vec<ActionDecl<Ebook>> {
        self.reading
            .as_ref()
            .map(crate::actions::reading_declarations)
            .unwrap_or_default()
    }

    /// The books with their chapters' prose, their annotations and the
    /// library's remote books: what the reader holds, not the document.
    fn search_entries(&self) -> Vec<SearchEntry> {
        self.search.entries()
    }
}

/// The reader as the view lays it out: it shows the library the document
/// holds, answers the effects on itself, and publishes where it is and what
/// search finds in its books.
pub(crate) fn window(cx: &ViewCx<Ebook>, mut window: BooksApp) -> Element {
    window.follow(cx);
    let store = window.store();
    cx.window_effects(|app: &mut BooksApp, effect: BooksEffect, cx| app.answer_effect(effect, cx));
    #[cfg(feature = "samples")]
    {
        let store = Rc::clone(&store);
        cx.on_stage(move |scenario| {
            let scenario = scenario.unwrap_or("read").to_owned();
            store.borrow_mut().asked.push(Asked::Stage(scenario));
        });
    }
    {
        let store = Rc::clone(&store);
        cx.bind_declarations(move |_| store.borrow().shown.declarations());
    }
    let (row, entries) = (Rc::clone(&store), store);
    cx.publishing(
        Publishes::new()
            .live_row(move |id| row.borrow().shown.live_row(id))
            .search_entries(move || entries.borrow().shown.search_entries()),
        Box::new(window),
    )
}

impl BooksApp {
    /// Answer `effect` on what the reader shows now. Opening a book asks the
    /// host's chooser or reads the content store first; a share or fetch
    /// answers once its bytes are in place.
    fn answer_effect(&mut self, effect: BooksEffect, cx: &mut EffectCx<Ebook>) -> EffectAnswer {
        match effect {
            BooksEffect::OpenBook => Ok(self.choose_book(cx)),
            BooksEffect::OpenStored { content, name } => self.open_stored(content, name, cx),
            BooksEffect::ReadSettings => Ok(Effected::value(cx.live().with(Ebook::read_settings))),
            BooksEffect::ReadFont { font } => cx
                .live()
                .with(|library| library.read_font(&font))
                .map(Effected::value),
            BooksEffect::ReadScheme { scheme } => cx
                .live()
                .with(|library| library.read_scheme(&scheme))
                .map(Effected::value),
            BooksEffect::ReadBook { book } => cx
                .live()
                .with(|library| library.read_book(&book))
                .map(Effected::value),
            BooksEffect::ReadHighlight { highlight } => cx
                .live()
                .with(|library| library.read_highlight(highlight))
                .map(Effected::value),
            BooksEffect::ReadAnnotation { annotation } => cx
                .live()
                .with(|library| library.read_annotation(annotation))
                .map(Effected::value),
            effect => {
                let invocation = cx.invocation().unwrap_or_default();
                let entries: Vec<LibraryEntry> = cx
                    .live()
                    .with(|state| state.entries.values().cloned().collect());
                let answer = self.begin_effect(invocation, effect, &entries)?;
                if matches!(answer, Effected::Pending) {
                    self.await_effect(invocation, cx.reply());
                }
                Ok(answer)
            }
        }
    }

    /// Ask the person for a book with the host's chooser, then place it
    /// where the library reads it again and open it. A cancelled choice
    /// opens nothing.
    fn choose_book(&self, cx: &mut EffectCx<Ebook>) -> Effected {
        let (store, kept) = (self.store(), self.content.clone());
        cx.choose_file(
            super::book_open_request(),
            move |(): &mut (), chosen, cx| {
                let Some(file) = chosen else {
                    return Ok(Effected::done());
                };
                let name = file.name;
                Ok(cx.thread(
                    move |_| content::place_chosen_book(&file.bytes, kept.as_ref()),
                    move |(): &mut (), placed, cx| finish_opening(&store, &name, placed, cx),
                ))
            },
        )
    }

    /// Open the book whose bytes the content store holds as `hash`: parsed
    /// off the UI thread, shelved through the library's action and opened.
    /// What a launch on a book takes.
    fn open_stored(&self, hash: String, name: String, cx: &mut EffectCx<Ebook>) -> EffectAnswer {
        let kept = self
            .content
            .clone()
            .ok_or_else(|| "this device keeps no book contents".to_string())?;
        let store = self.store();
        Ok(cx.thread(
            move |_| {
                let bytes = kept
                    .read(&hash)
                    .ok_or_else(|| "its contents are not on this device".to_string())?;
                content::open_handed_book(&bytes, None).map(|mut handed| {
                    handed.content = Some(hash);
                    content::placed(handed)
                })
            },
            move |(): &mut (), placed, cx| finish_opening(&store, &name, placed, cx),
        ))
    }
}

/// Shelve a book an effect placed through the library's action and have the
/// reader open it, or say why it could not be opened.
fn finish_opening(
    store: &Shared,
    name: &str,
    placed: Result<content::Placed, String>,
    cx: &EffectCx<Ebook>,
) -> EffectAnswer {
    let placed = placed.map_err(|error| format!("Couldn’t open {name}: {error}"));
    let answer = match &placed {
        Ok(placed) => {
            cx.submit(crate::resource::shelve(&placed.entry));
            Ok(Effected::done())
        }
        Err(message) => Err(message.clone()),
    };
    store
        .borrow_mut()
        .asked
        .push(Asked::Opened(Box::new(placed)));
    answer
}

/// Look through `folders` off the UI thread, and put the books found there
/// that the library does not hold yet on it, through the library's actions
/// like an import; the reader remembers where each one's file is.
pub(super) fn scan_watched_folders(
    store: &Shared,
    jobs: &ui::framework::Jobs<Ebook>,
    folders: Vec<std::path::PathBuf>,
) {
    let store = Rc::clone(store);
    jobs.start("watched-folders", move |cx: &mut EffectCx<Ebook>| {
        Ok(cx.thread(
            move |_| content::find_watched_books(&folders),
            move |(): &mut (), found: Vec<content::FoundBook>, cx| {
                let shelved: std::collections::HashSet<String> = cx.live().with(|state| {
                    state
                        .entries
                        .values()
                        .map(|entry| entry.id.clone())
                        .collect()
                });
                for book in found
                    .iter()
                    .filter(|book| !shelved.contains(&book.entry.id))
                {
                    cx.submit(crate::resource::shelve_found(&book.entry));
                }
                store.borrow_mut().asked.push(Asked::Found(found));
                Ok(Effected::done())
            },
        ))
    });
}
