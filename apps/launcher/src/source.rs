//! What the launcher asks of whoever answers its searches, and what they
//! answer.
//!
//! The launcher draws, the source knows. A source answers a search with the
//! rows to show (the toolkit's palette rows, each already wearing its
//! picture, facts and the ways it can be opened), and carries out what the
//! person chose: it opens the app, runs the action, asks for the action's
//! arguments one step at a time. The shell lends the launcher its source
//! over everything the desktop can find ([`LauncherService`]); a capture
//! and the app's own window use a fixed set of rows
//! ([`crate::fixture::FixtureSource`]).
//!
//! The two sides talk through a [`Link`]: the launcher puts its asks on
//! it, and the source answers each in turn on its own schedule (the
//! shell's on its next frame, where it holds what search reads), waking the
//! launcher when the answer changed.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

pub use ui::search::ActivationModifier;
pub use ui::search::system::{ResourceKind, SearchScope};
pub use ui::{ArgumentSpec, CommandEntry};

/// What the person searches for: the text typed and the kinds of result
/// they narrowed it to (none: every kind).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Search {
    pub query: String,
    pub kinds: Vec<ResourceKind>,
}

/// Something the launcher asks of its source.
#[derive(Clone, Debug, PartialEq)]
pub enum Ask {
    /// The launcher opened in `scope` (the source may search a narrower or
    /// wider one when that is all there is), showing `query`, or what the
    /// source opens it on when `None`.
    Open {
        scope: SearchScope,
        query: Option<String>,
    },
    /// The rows for `search`.
    Search(Search),
    /// The person chose row `row`, with `modifier` held: the source opens
    /// it, runs it or asks what it needs first.
    Choose {
        row: String,
        modifier: Option<ActivationModifier>,
    },
    /// The person selected row `row`: the source keeps it selected while
    /// rows change under it, and runs it when the person chooses.
    Select { row: String },
    /// The person stepped the value row `row` shows by `delta` (←/→).
    Adjust { row: String, delta: i8 },
    /// The argument step on screen was answered with `text`.
    Argument(String),
    /// The person left the step on screen (Escape): back to the previous
    /// one, or to the results.
    Back,
    /// Search one scope wider than now.
    Widen,
    /// The launcher closed without a choice.
    Dismissed,
}

/// The source's answer: everything the launcher shows that is not what
/// the person typed.
#[derive(Clone, Debug, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is one independent part of what the launcher shows"
)]
pub struct Answer {
    /// The rows, in order: entries sharing a group sit under one header,
    /// and a row's section heads its group.
    pub rows: Vec<CommandEntry>,
    /// Whether typing narrows `rows` here rather than asking the source:
    /// the values an argument step offers, a row's other actions.
    pub narrows: bool,
    /// The kinds the results hold, most hits first, with their labels:
    /// the filter chips. A kind the person chose is among them.
    pub kinds: Vec<(ResourceKind, String)>,
    /// Whether the filter band shows.
    pub band: bool,
    /// The scope searched now.
    pub scope: SearchScope,
    /// The token at the start of the field naming a narrowed scope, and
    /// the accessible name of what pressing it does (widen), if anything.
    pub scope_token: Option<String>,
    pub scope_token_action: Option<String>,
    /// What the empty field says it searches.
    pub placeholder: String,
    /// An argument step's field, by its id, in place of the query.
    pub field: Option<(String, ArgumentSpec)>,
    /// The query the source puts in the field (opening on one, coming back
    /// from a step); `None` leaves what the person typed.
    pub query: Option<String>,
    /// The row to select, by id: the person's choice as the source keeps
    /// it ([`Ask::Select`]), or its best row. `None` keeps the person's
    /// choice, or the first row.
    pub select: Option<String>,
    /// Why the selected row did not run: a title and a detail.
    pub status: Option<(String, String)>,
    /// More rows are still coming.
    pub searching: bool,
    /// The field alone while nothing is typed: no list and no band.
    pub field_only: bool,
    /// The launcher's work is done (a row ran, the source closed it).
    pub closed: bool,
}

/// Answers the launcher's asks.
pub trait LauncherSource {
    /// Answer `ask`, changing `answer` to what the launcher shows now.
    fn answer(&mut self, ask: Ask, answer: &mut Answer);
}

/// What the launcher's card makes of the room its host gives it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CardFit {
    /// The card, in the launcher's own coordinates.
    pub card: ui::vello::kurbo::Rect,
    /// The card's height less its list: the field, the band, the padding.
    pub chrome_height: f64,
    /// How many rows a full list holds: what the source finds rows for.
    pub rows: usize,
    /// Each row on screen, by id, and where it is in the launcher's own
    /// coordinates: what a press or a menu on the card is about.
    pub shown: Vec<(String, ui::vello::kurbo::Rect)>,
}

#[derive(Default)]
struct Shared {
    asks: VecDeque<Ask>,
    answer: Answer,
    generation: u64,
    open: bool,
    /// The host asked the launcher to open, in this scope.
    open_request: Option<SearchScope>,
    room: Option<(f64, f64)>,
    fit: Option<CardFit>,
    /// The modifier keys the host sees held.
    held: ui::winit::keyboard::ModifiersState,
    wake: Option<ui::WakeHandle>,
}

/// The launcher's side of the conversation and its source's, shared
/// between the two in one process.
#[derive(Clone, Default)]
pub struct Link {
    shared: Rc<RefCell<Shared>>,
    /// Bumped with every ask, so a host can tell there is work without
    /// borrowing.
    pending: Rc<Cell<u64>>,
}

impl Link {
    pub fn new() -> Self {
        Self::default()
    }

    /// Put `ask` to the source.
    pub fn ask(&self, ask: Ask) {
        let mut shared = self.shared.borrow_mut();
        match &ask {
            Ask::Open { .. } => shared.open = true,
            Ask::Dismissed => shared.open = false,
            _ => {}
        }
        shared.asks.push_back(ask);
        self.pending.set(self.pending.get().wrapping_add(1));
    }

    /// Whether an ask waits for its answer.
    pub fn has_asks(&self) -> bool {
        !self.shared.borrow().asks.is_empty()
    }

    /// Answer every ask waiting, in order, with `source`, and wake the
    /// launcher when the answer changed.
    pub fn serve(&self, source: &mut dyn LauncherSource) {
        loop {
            let Some(ask) = self.shared.borrow_mut().asks.pop_front() else {
                break;
            };
            let mut answer = self.shared.borrow().answer.clone();
            answer.query = None;
            answer.select = None;
            answer.closed = false;
            source.answer(ask, &mut answer);
            self.publish(answer);
        }
    }

    /// Show `answer`, as the source's own change (a provider's late rows,
    /// a switch flipped somewhere else), and wake the launcher.
    pub fn publish(&self, answer: Answer) {
        let wake = {
            let mut shared = self.shared.borrow_mut();
            if answer.closed {
                shared.open = false;
            }
            shared.answer = answer;
            shared.generation = shared.generation.wrapping_add(1);
            shared.wake.clone()
        };
        if let Some(wake) = wake {
            wake.wake();
        }
    }

    /// The answer showing now, and its generation.
    pub fn answer(&self) -> (u64, Answer) {
        let shared = self.shared.borrow();
        (shared.generation, shared.answer.clone())
    }

    /// The generation of the answer showing now.
    pub fn generation(&self) -> u64 {
        self.shared.borrow().generation
    }

    /// Whether the launcher is open: opened and neither dismissed nor
    /// closed by its source since.
    pub fn is_open(&self) -> bool {
        self.shared.borrow().open
    }

    /// Ask the launcher to open in `scope`, as the host's own shortcut
    /// does: the launcher opens before it handles anything else, so the
    /// first key typed after the shortcut is never lost to a closed card.
    /// Agents and scripts open it through its `open` action instead.
    pub fn request_open(&self, scope: SearchScope) {
        let wake = {
            let mut shared = self.shared.borrow_mut();
            shared.open_request = Some(scope);
            shared.wake.clone()
        };
        if let Some(wake) = wake {
            wake.wake();
        }
    }

    /// The scope the host asked the launcher to open in, once.
    pub fn take_open_request(&self) -> Option<SearchScope> {
        self.shared.borrow_mut().open_request.take()
    }

    /// Give the launcher's card `width` and let its list grow to
    /// `list_height`: the room the host shows it in.
    pub fn set_room(&self, width: f64, list_height: f64) {
        let wake = {
            let mut shared = self.shared.borrow_mut();
            if shared.room == Some((width, list_height)) {
                return;
            }
            shared.room = Some((width, list_height));
            shared.wake.clone()
        };
        if let Some(wake) = wake {
            wake.wake();
        }
    }

    /// Tell the host what the card makes of its room.
    pub fn set_fit(&self, fit: CardFit) {
        let mut shared = self.shared.borrow_mut();
        if shared.fit.as_ref() != Some(&fit) {
            shared.fit = Some(fit);
        }
    }

    /// What the card makes of its room, once the launcher said.
    pub fn fit(&self) -> Option<CardFit> {
        self.shared.borrow().fit.clone()
    }

    /// The host sees `held` held, with no key for the launcher to read it
    /// from: the selected row previews what choosing it with them does.
    /// Whether that changed.
    pub fn hold_modifiers(&self, held: ui::winit::keyboard::ModifiersState) -> bool {
        let wake = {
            let mut shared = self.shared.borrow_mut();
            if shared.held == held {
                return false;
            }
            shared.held = held;
            shared.wake.clone()
        };
        if let Some(wake) = wake {
            wake.wake();
        }
        true
    }

    /// The modifier keys the host sees held.
    pub fn held_modifiers(&self) -> ui::winit::keyboard::ModifiersState {
        self.shared.borrow().held
    }

    /// The room the host gives the card, if it said.
    pub fn room(&self) -> Option<(f64, f64)> {
        self.shared.borrow().room
    }

    /// Wake the launcher with `wake` when an answer arrives.
    pub fn wake_with(&self, wake: ui::WakeHandle) {
        self.shared.borrow_mut().wake = Some(wake);
    }
}

/// The source a host lends the launcher it mounts
/// ([`ui::application::HostContext::lend`]): the host keeps a clone of the
/// link and answers its asks.
#[derive(Clone)]
pub struct LauncherService {
    pub link: Link,
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    impl LauncherSource for Echo {
        fn answer(&mut self, ask: Ask, answer: &mut Answer) {
            match ask {
                Ask::Search(search) => {
                    answer.rows = vec![CommandEntry::new("echo", search.query)];
                }
                Ask::Choose { .. } => answer.closed = true,
                _ => {}
            }
        }
    }

    #[test]
    fn asks_are_answered_in_order_and_each_answer_is_a_new_generation() {
        let link = Link::new();
        link.ask(Ask::Open {
            scope: SearchScope::Workspace,
            query: None,
        });
        link.ask(Ask::Search(Search {
            query: "notes".into(),
            kinds: Vec::new(),
        }));
        assert!(link.is_open());
        assert!(link.has_asks());
        link.serve(&mut Echo);
        assert!(!link.has_asks());
        let (generation, answer) = link.answer();
        assert_eq!(generation, 2);
        assert_eq!(answer.rows[0].label, "notes");

        link.ask(Ask::Choose {
            row: "echo".into(),
            modifier: None,
        });
        link.serve(&mut Echo);
        assert!(!link.is_open(), "a row that ran closes the launcher");
        assert_eq!(
            link.answer().1.rows[0].label,
            "notes",
            "what an answer does not change stays"
        );
    }
}
