//! A task's checklist of obligations: what its request asks for, as the
//! model listed it on the first plan, kept by the host rather than the
//! model so a re-plan for news, a wait or a repair cannot drop an item.

use crate::model::OBLIGATIONS_INTRO;
use crate::planner::{MOST_OBLIGATIONS, Obligations};

/// The most times a task that would finish with obligations open is
/// planned again for them; then it finishes, reporting them undone.
pub(super) const MOST_REMINDERS: u32 = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
enum State {
    Open,
    Done,
    SetAside(String),
}

#[derive(Debug)]
struct Item {
    text: String,
    state: State,
}

#[derive(Debug, Default)]
pub(super) struct Checklist {
    items: Vec<Item>,
    reminders: u32,
    /// How many were open when the task was last reminded: a reminder that
    /// closed none is the last, as another would be answered the same way.
    open_when_reminded: Option<usize>,
}

impl Checklist {
    /// Take up what a plan declared: its items when the task has none yet
    /// (a later list would renumber what the steps were tagged with), and,
    /// when the plan judged the finished work (`judged`: a reminder or a
    /// check), the open ones it set aside. Models set aside, mid-task, what
    /// a wait or a later step was still to do ("once the sessions are
    /// in"), and the task then finished without it: those stay open.
    pub(super) fn adopt(&mut self, declared: Obligations, judged: bool) {
        if self.items.is_empty() {
            self.items = declared
                .items
                .into_iter()
                .take(MOST_OBLIGATIONS)
                .map(|text| Item {
                    text,
                    state: State::Open,
                })
                .collect();
        }
        if !judged {
            if !declared.set_aside.is_empty() {
                tracing::debug!(set_aside = ?declared.set_aside, "agent: obligations set aside mid-task stay open");
            }
            return;
        }
        for (number, reason) in declared.set_aside {
            if let Some(item) = self.open_item(number) {
                tracing::info!(
                    item = number,
                    obligation = %item.text,
                    %reason,
                    "agent: an obligation is set aside"
                );
                item.state = State::SetAside(reason);
            }
        }
    }

    /// Obligation `number` (from 1) is fulfilled, though it was set aside.
    pub(super) fn tick(&mut self, number: usize) {
        if let Some(item) = number.checked_sub(1).and_then(|at| self.items.get_mut(at)) {
            item.state = State::Done;
        }
    }

    fn open_item(&mut self, number: usize) -> Option<&mut Item> {
        self.items
            .get_mut(number.checked_sub(1)?)
            .filter(|item| item.state == State::Open)
    }

    fn numbered(&self) -> impl Iterator<Item = (usize, &Item)> {
        self.items
            .iter()
            .enumerate()
            .map(|(at, item)| (at.saturating_add(1), item))
    }

    fn open(&self) -> Vec<String> {
        self.numbered()
            .filter(|(_, item)| item.state == State::Open)
            .map(|(number, item)| format!("{number}. {}", item.text))
            .collect()
    }

    /// The list as a plan of the rest of the task is shown it, each item
    /// with whether it is done, then what is still to do. Empty for none.
    pub(super) fn shown(&self) -> String {
        if self.items.is_empty() {
            return String::new();
        }
        let listed = self
            .numbered()
            .map(|(number, item)| match &item.state {
                State::Open => format!("{number}. {}", item.text),
                State::Done => format!("{number}. {} (done)", item.text),
                State::SetAside(reason) => {
                    format!("{number}. {} (set aside: {reason})", item.text)
                }
            })
            .collect::<Vec<_>>()
            .join("; ");
        let open = self.open();
        let still = if open.is_empty() {
            "Nothing on it is still to do.".to_owned()
        } else {
            format!(
                "Still to do: {}. Plan these too, each step with its number in `for`, unless one \
                 is already done or cannot be: then set it aside in `set_aside`, saying why.",
                open.join("; ")
            )
        };
        format!("{OBLIGATIONS_INTRO}, as you listed them: {listed}.\n{still}")
    }

    /// Whether the task, about to finish with obligations open, is planned
    /// once more for them: at most [`MOST_REMINDERS`] times, and not after
    /// a reminder that closed none.
    pub(super) fn remind(&mut self) -> bool {
        let open = self.open().len();
        if open == 0
            || self.reminders >= MOST_REMINDERS
            || self.open_when_reminded.is_some_and(|before| open >= before)
        {
            return false;
        }
        self.reminders = self.reminders.saturating_add(1);
        self.open_when_reminded = Some(open);
        true
    }

    /// What the task's result adds: the obligations left undone and those
    /// set aside, with why.
    pub(super) fn outcome(&self) -> String {
        let open = self.open();
        let set_aside: Vec<String> = self
            .numbered()
            .filter_map(|(number, item)| match &item.state {
                State::SetAside(reason) => Some(format!("{number}. {} ({reason})", item.text)),
                State::Open | State::Done => None,
            })
            .collect();
        let mut outcome = String::new();
        if !open.is_empty() {
            outcome = format!(" Left undone: {}.", open.join("; "));
        }
        if !set_aside.is_empty() {
            outcome = format!("{outcome} Set aside: {}.", set_aside.join("; "));
        }
        outcome
    }

    pub(super) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The list for the plan log: each item, its state and why one was set
    /// aside.
    pub(super) fn logged(&self) -> serde_json::Value {
        self.numbered()
            .map(|(number, item)| {
                let (state, reason) = match &item.state {
                    State::Open => ("open", None),
                    State::Done => ("done", None),
                    State::SetAside(reason) => ("set aside", Some(reason)),
                };
                serde_json::json!({
                    "item": number,
                    "text": item.text,
                    "state": state,
                    "reason": reason,
                })
            })
            .collect()
    }
}

/// What a task about to finish with obligations open is planned again
/// with; the list itself follows the request ([`Checklist::shown`]), and
/// the steps that ran follow this.
pub(super) const OBLIGATIONS_LEFT: &str = "You planned nothing more, but obligations of the \
     request are still open (listed below). Plan the steps that do them, each with its number in \
     `for`. Set one aside in `set_aside` with a short reason when it is already done (by a step \
     that ran, or as a record shows) or cannot be done. Never plan again a step that ran: what it \
     did is there. The steps that ran: ";
