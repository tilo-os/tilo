//! A view a message carries: a small piece of interface written in the app
//! language, over what the workspace's apps keep.
//!
//! Whoever answers with something that changes (how many issues are open,
//! who has replied, what a sheet's total is) can answer with a view instead
//! of words that are stale the moment they are read. The view is state of
//! the message ([`View`]): its definition as written, the apps it reads, and
//! which writing of it this is. Where the message is read by a program that
//! shows views, the view is drawn there, live, as the person reading it: it
//! shows what they may read and is drawn again when what it reads changes.
//! Every other reader shows the message's words, which are always there, and
//! one line that says a view is attached ([`line`]).
//!
//! # Whose code it is
//!
//! A view is code, and it carries no author. Its author is its message's,
//! with what wrote the message for a person where software did: Chat pins
//! both from who invokes the verb, so nothing a writer fills in says whose
//! code a view is. Only the author writes it again (`rewrite-view`).
//!
//! # What it reads
//!
//! [`View::reads`] names the apps the view reads, by the names a view calls
//! them (`tasks`, `mail`). It is the writer's declaration, held from both
//! sides: a name the definition does not name is refused here
//! ([`View::written`]), and where the view is shown it is given exactly the
//! apps listed, so a reader that cannot show the view still says truthfully
//! what it is a view of.

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Model, Refusal};

use crate::i18n::{t, t_args};

/// The longest definition a view has, in bytes.
pub const MAX_SOURCE_BYTES: usize = 32 * 1024;
/// The most apps one view reads.
pub const MAX_READS: usize = 16;
/// The longest name a view calls an app by, in bytes.
pub const MAX_NAME_BYTES: usize = 64;
/// The most earlier forms a view keeps; the oldest goes first.
pub const MAX_EARLIER: usize = 8;

/// What a model is told where it is offered a tool that answers with a view: when a view is
/// the right answer, and how one is written.
pub const GUIDE: &str = "Answer with a view: a small piece of interface you write in Tilo's app \
language over what the workspace's apps keep. It is shown live in your reply to everyone who \
reads the conversation, each as themselves: it shows what that reader may read, and it is drawn \
again whenever what it reads changes, whoever changed it. Use it when what was asked for keeps \
changing or is easier seen than read: a count, a list, a status, things someone wants to keep \
an eye on. Answer in words alone when the answer is one fact that will not move. `source` is \
one `view` declaration and the functions it calls, and nothing else: no types, no actions, no \
`use`. It names an app by the last part of its id (`tasks` for `so.tilo.tasks`) and reads what \
that app keeps for the workspace, by the names reading the app shows: `view projects() { for a \
in tasks.tracker.areas { text(a.name) } }`. One document among several an app keeps (a text of \
the word processor) is named by its own id, the last part of the link the tool that wrote it \
answered with: `text(@word_processor.Document:uuid:0f2b1c00-0000-4000-8000-000000000001\
.body.text)`. A control invokes one of the app's own actions, \
named as the app names it with `_` for `-`: `button(\"Delete\", on: \
tasks.delete_area(tasks.tracker, a.id))`; in your view a press only suggests, and the reader \
allows that one thing or not. Numbers that are compared are a chart, given readings and what \
they are of and never a colour or a size: `ui.BarChart([.bars(values: [3.0, 5.0], label: \
\"Closed\")], categories: [\"Mon\", \"Tue\"])` for amounts side by side (`layout: .stacked` \
stacks several `.bars`), `ui.PieChart([.slice(label: \"Open\", value: 3.0)])` \
for the parts of one whole, `ui.Chart([.series(values: [1.0, 2.0], label: \"Opened\")])` for \
a history; build the lists from what the app keeps (`areas.map(a -> a.people.len().to_float())`) \
and the chart follows it. A view reads a field by its type: a text, a number, a flag, a date \
or a time as it is; an option with `??` or `if let v = x {}`; a list, a set or a collection \
with `for i in xs {}`, `.len()` or `.map()`; a thing inside a thing by `.field`; a plain case \
by `== .name`. A field that cannot be read fails alone and says so in the view's place. \
`reads` lists every app the view reads, each as the view calls \
it (`[\"tasks\"]`); the view is given those apps and no other. Calling it again in the same \
turn writes the view again. After it, say in a sentence or two what the view shows: those words \
are all that search, notifications and a reader that cannot show a view have.";

/// The view a message carries.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct View {
    /// The view as written in the app language: one `view` declaration and
    /// the functions it calls.
    pub source: String,
    /// The apps it reads, by the names a view calls them: what it is given
    /// where it is shown, and nothing else.
    pub reads: Vec<String>,
    /// Which writing of it this is: 1, then one more each time its author
    /// writes it again.
    pub version: u32,
    /// When this version was written, in seconds since the epoch.
    pub at: i64,
    /// The forms before this one, oldest first.
    pub earlier: Vec<Earlier>,
}

/// A form a view had before its author wrote it again.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Earlier {
    pub version: u32,
    pub source: String,
    pub reads: Vec<String>,
    pub at: i64,
}

impl View {
    /// The view `source` reading `reads`, written at `at`: its first version.
    ///
    /// # Errors
    /// A definition that is empty or over [`MAX_SOURCE_BYTES`], more than
    /// [`MAX_READS`] apps, or an app the definition does not name.
    pub fn written(source: String, reads: Vec<String>, at: i64) -> Result<Self, Refusal> {
        let reads = checked(&source, reads)?;
        Ok(Self {
            source,
            reads,
            version: 1,
            at,
            earlier: Vec::new(),
        })
    }

    /// Its author wrote it again as `source` reading `reads`, at `at`: the
    /// next version, the form before kept in [`Self::earlier`]. The same
    /// definition and apps again change nothing.
    ///
    /// # Errors
    /// As [`Self::written`].
    pub fn written_again(
        &mut self,
        source: String,
        reads: Vec<String>,
        at: i64,
    ) -> Result<(), Refusal> {
        let reads = checked(&source, reads)?;
        if self.source == source && self.reads == reads {
            return Ok(());
        }
        self.earlier.push(Earlier {
            version: self.version,
            source: std::mem::replace(&mut self.source, source),
            reads: std::mem::replace(&mut self.reads, reads),
            at: self.at,
        });
        if self.earlier.len() > MAX_EARLIER {
            let over = self.earlier.len() - MAX_EARLIER;
            self.earlier.drain(..over);
        }
        self.version = self.version.saturating_add(1);
        self.at = at;
        Ok(())
    }
}

/// `reads` as a view keeps it, each app once in the order given, checked
/// against the definition `source`.
fn checked(source: &str, reads: Vec<String>) -> Result<Vec<String>, Refusal> {
    if source.trim().is_empty() {
        return Err(Refusal::new(
            "a view shows something: its definition is empty",
        ));
    }
    if source.len() > MAX_SOURCE_BYTES {
        return Err(Refusal::new("the view's definition is too long"));
    }
    let named = named(source);
    let mut kept: Vec<String> = Vec::new();
    for app in reads {
        if app.is_empty() || app.len() > MAX_NAME_BYTES {
            return Err(Refusal::new(
                "an app a view reads is named as the view calls it, like tasks",
            ));
        }
        if !named.contains(&app) {
            return Err(Refusal::new(format!(
                "the view does not name {app}: list the apps it reads, each as the view calls it before a dot"
            )));
        }
        if !kept.contains(&app) {
            kept.push(app);
        }
    }
    if kept.len() > MAX_READS {
        return Err(Refusal::new("a view reads at most 16 apps"));
    }
    Ok(kept)
}

/// Every name that stands before a dot in `source`, each once, in the order
/// they first stand: how a view names an app (`tasks.tracker`), and so every
/// app it can reach. The rule is the one the code that shows views finds a
/// view's apps by.
#[must_use]
pub fn named(source: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut word = String::new();
    for c in source.chars() {
        if c.is_alphanumeric() || c == '_' {
            word.push(c);
            continue;
        }
        if c == '.' && !word.is_empty() && !names.contains(&word) {
            names.push(std::mem::take(&mut word));
        }
        word.clear();
    }
    names
}

/// What a reader that does not show views says of the view a message
/// carries, under the message's words: that one is attached, and of which
/// apps.
#[must_use]
pub fn line(view: &View) -> String {
    if view.reads.is_empty() {
        return t("chat-view-attached");
    }
    let apps = view
        .reads
        .iter()
        .map(|app| spoken(app))
        .collect::<Vec<_>>()
        .join(", ");
    t_args("chat-view-attached-of", &[("apps", apps.into())])
}

/// The name a view calls an app by, as a line names it: `tasks` is "Tasks".
fn spoken(app: &str) -> String {
    ui::format::capitalize_first(&app.replace('_', " "))
}
