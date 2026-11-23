//! One message's view, shown alone in the window: what a program that keeps a conversation
//! of its own on screen (a native app with Tilo's page inside it) mounts in a message's place.
//!
//! The window is told the message ([`crate::effects::ChatEffect::ShowView`], the view `view`
//! of the message's item). From then on it draws that message's view and nothing of the
//! conversation around it: the view itself, live, then what the view could not read and what
//! it asked to do that waits for the person, each with the shared controls. Showing a
//! conversation or a message again ends it.
//!
//! # Whose code, and whose eyes
//!
//! The view's author is whoever the workspace attributes the message to ([`code_author`]): the
//! software that wrote it where software did, else its author, both as Chat pinned them from
//! whoever invoked the verb that posted the view. Nothing the view's text says, and nothing
//! whoever asks for the view to be shown says, changes that. What Chat pinned is a field of the
//! message all the same, and nothing yet binds it to the device that signed the change which
//! wrote the message: a member's device can write another member's name there. A view is
//! trusted to act with no question where its author is its reader, so a view whose message
//! names its reader as the author is not handed over as theirs ([`Stands::Unbound`]): until the
//! author is read from the signer, no view is trusted on the field. The view is shown as the person
//! at this window: it reads what they may read, of the apps the message lists and no other,
//! and what it asks to do in someone else's name waits here until this person allows that one
//! thing.
//!
//! The window says how tall what it shows is ([`ChatApp::alone_size`]), so whoever holds the
//! window gives it that height.

use std::rc::Rc;

use ui::Widget;
use ui::framework::page_part::{Asked, Placed, asking};
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_actions::Actor;
use ui::tilo_resource::{Acting, MemberId};
use ui::tokens::TextRole;
use ui::widget::{AppModel, Button, ButtonRole, Column, Element, ModelPath, Row, Text};
use ui::written_view::{self, Suggestion, ViewState, WrittenView};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::WindowEvent;

use crate::app::ChatApp;
use crate::i18n::{t, t_args};

/// Whose code the view of a message by `author` is: the software that wrote the message
/// where software did (`acting`), else its author. So what an agent wrote for a person is
/// never that person's own code.
pub(crate) fn code_author(author: &MemberId, acting: Option<&Acting>) -> Actor {
    match acting {
        Some(acting) => acting.by.clone(),
        None => Actor::Member(author.clone()),
    }
}

/// What the person answers to something the view asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    Allow(u64),
    Dismiss(u64),
}

/// Where the view stands in the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stands {
    /// The message has not reached this device yet.
    Waiting,
    /// The message carries no view.
    Gone,
    /// The code that shows views is on its way.
    Arriving,
    /// This program cannot show a view, or nobody is at this window to show it to.
    Unavailable,
    /// The message names its reader as the view's author. A view is trusted where its author
    /// is its reader, and nothing yet binds a message's author to the device that signed the
    /// message: such a view is not shown as its reader's own.
    Unbound,
    Shown,
}

/// The one message whose view the window shows, and what is placed of it.
pub(crate) struct Alone {
    message: String,
    stands: Stands,
    /// The version of the view and the generation of the code that shows views it was placed
    /// at: it is placed again when either moved.
    placed: Option<(u32, u64)>,
    view: Option<Placed<Column>>,
    state: Option<Rc<dyn ViewState>>,
    /// The revision of `state` the window last drew.
    revision: u64,
    /// What is said under the view: what it could not read, and what it asked.
    under: Placed<Column>,
    /// What `under` says, as text: it is built again when this moved.
    under_key: String,
    /// What the person answered under the view, until the window takes it.
    answers: Asked<Answer>,
    /// How tall everything shown is, as last drawn.
    height: f64,
    /// The session client the view asks through. The window hands on what it asked and takes
    /// the answers in ([`Self::take_requests`], [`Self::answered`]), so a holder that serves
    /// its windows through their own asks serves the view too.
    api: ui::RemoteActions,
}

impl Alone {
    fn of(message: &str) -> Self {
        Self {
            api: ui::RemoteActions::current(),
            message: message.to_owned(),
            stands: Stands::Waiting,
            placed: None,
            view: None,
            state: None,
            revision: 0,
            under: Placed::new(Column::new().with_no_padding()),
            under_key: String::new(),
            answers: Rc::default(),
            height: 0.0,
        }
    }
}

impl Alone {
    /// What the view asked of the session since the last call.
    pub(crate) fn take_requests(&mut self) -> Vec<ui::collaboration::CollabRequest> {
        ui::widget::Collaborative::take_collab_requests(&mut self.api)
    }

    /// Hand the view's client `event`, which may answer something it asked.
    pub(crate) fn answered(&mut self, event: &ui::collaboration::CollabEvent) {
        ui::widget::Collaborative::handle_collab_event(&mut self.api, event);
    }
}

/// Everything said under the view. What it reads as is the key of the part that says it.
#[derive(Debug)]
struct Said {
    stands: Stands,
    refused: Vec<String>,
    unread: Vec<String>,
    suggestions: Vec<Suggestion>,
    /// Who wrote the view, as the conversation names them.
    by: String,
}

fn caption(words: &str) -> Element {
    Text::new(words).with_role(TextRole::Caption).into_widget()
}

impl Said {
    fn widget(&self, asked: &Asked<Answer>, gap: f32) -> Element {
        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(gap);
        match self.stands {
            Stands::Waiting => column = column.push(caption(&t("chat-view-waiting"))),
            Stands::Gone => column = column.push(caption(&t("chat-view-gone"))),
            Stands::Arriving => column = column.push(caption(&t("chat-view-arriving"))),
            Stands::Unavailable => column = column.push(caption(&t("chat-view-unavailable"))),
            Stands::Unbound => column = column.push(caption(&t("chat-view-unbound"))),
            Stands::Shown => {}
        }
        if !self.refused.is_empty() {
            let apps = self.refused.join(", ");
            column = column.push(caption(&t_args(
                "chat-view-refused",
                &[("apps", apps.into())],
            )));
        }
        if !self.unread.is_empty() {
            let apps = self.unread.join(", ");
            column = column.push(caption(&t_args(
                "chat-view-unread",
                &[("apps", apps.into())],
            )));
        }
        for suggestion in &self.suggestions {
            let mut card = Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(gap);
            card = card.push(caption(&t_args(
                "chat-view-asks",
                &[
                    ("who", self.by.clone().into()),
                    ("app", suggestion.app.clone().into()),
                ],
            )));
            card = card.push(Text::new(&suggestion.action).strong().into_widget());
            // Every argument, whole: nothing is sent that is not said here.
            for (name, value) in &suggestion.arguments {
                card = card.push(caption(&t_args(
                    "chat-view-asks-given",
                    &[
                        ("name", name.clone().into()),
                        ("value", value.clone().into()),
                    ],
                )));
            }
            let allow = Button::new(
                &format!("chat.view.allow.{}", suggestion.id),
                &t("chat-view-allow"),
            )
            .role(if suggestion.destructive {
                ButtonRole::Destructive
            } else {
                ButtonRole::Primary
            })
            .on_click(asking(asked, Answer::Allow(suggestion.id)));
            let dismiss = Button::new(
                &format!("chat.view.dismiss.{}", suggestion.id),
                &t("chat-view-dismiss"),
            )
            .on_open(asking(asked, Answer::Dismiss(suggestion.id)));
            card = card.push(Box::new(
                Row::new()
                    .with_gap(gap)
                    .push(Box::new(allow))
                    .push(Box::new(dismiss)),
            ));
            column = column.push(Box::new(card));
        }
        Box::new(column)
    }
}

impl ChatApp {
    /// Show the view the message `message` carries, alone.
    pub(crate) fn show_view_alone(&mut self, message: &str) -> Result<(), String> {
        let held = self
            .search_messages
            .iter()
            .find(|item| item.id == message)
            .ok_or("message no longer exists")?;
        if held.view.is_none() {
            return Err("the message carries no view".into());
        }
        if self
            .alone
            .as_ref()
            .is_none_or(|alone| alone.message != message)
        {
            self.alone = Some(Alone::of(message));
        }
        self.canonical_dirty = true;
        ui::widget::note_layout_moved();
        Ok(())
    }

    /// Bring what is shown alone up to date with its message, and let the view take in what
    /// it read. Whether what the window shows moved.
    pub(crate) fn update_alone(&mut self, doc: &AppModel, path: &ModelPath) -> bool {
        let Some(alone) = self.alone.as_mut() else {
            return false;
        };
        let before = alone.stands;
        let message = self
            .search_messages
            .iter()
            .find(|item| item.id == alone.message);
        match message.and_then(|message| message.view.as_ref().map(|view| (message, view))) {
            None => {
                alone.stands = if message.is_some() {
                    Stands::Gone
                } else {
                    Stands::Waiting
                };
                alone.view = None;
                alone.state = None;
                alone.placed = None;
            }
            Some((message, view)) => {
                let wanted = (view.version, written_view::code_generation());
                // A view whose code was on its way is placed again: that is what asks for the
                // code again when its fetch failed.
                if alone.placed != Some(wanted) || alone.stands == Stands::Arriving {
                    alone.placed = Some(wanted);
                    alone.view = None;
                    alone.state = None;
                    let author = code_author(&message.author, message.acting.as_ref());
                    alone.stands = match self.me.id.as_person() {
                        None => Stands::Unavailable,
                        Some(viewer)
                            if author.member_id().and_then(MemberId::as_person) == Some(viewer) =>
                        {
                            Stands::Unbound
                        }
                        Some(viewer) => {
                            let written = WrittenView {
                                source: view.source.clone(),
                                author,
                                viewer: viewer.clone(),
                                reads: view.reads.clone(),
                            };
                            match written_view::place(&written, &alone.api) {
                                written_view::Placed::Shown(shown) => {
                                    alone.view = Some(Placed::new(
                                        Column::new()
                                            .with_no_padding()
                                            .with_intrinsic_height()
                                            .push(shown.widget),
                                    ));
                                    alone.state = Some(shown.state);
                                    Stands::Shown
                                }
                                written_view::Placed::Arriving => Stands::Arriving,
                                written_view::Placed::Unavailable => Stands::Unavailable,
                            }
                        }
                    };
                }
            }
        }
        let mut moved = alone.stands != before;
        if let Some(view) = alone.view.as_mut() {
            moved |= view.widget_mut().update(doc, path);
        }
        let revision = alone.state.as_ref().map_or(0, |state| state.revision());
        if alone.revision != revision {
            alone.revision = revision;
            moved = true;
        }
        if moved {
            self.canonical_dirty = true;
            ui::widget::note_layout_moved();
        }
        moved
    }

    /// Draw the view and what is said under it, from the top of the window.
    pub(crate) fn draw_alone(
        &mut self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let window = self.rect;
        let by = self.alone_author();
        let Some(alone) = self.alone.as_mut() else {
            return;
        };
        let pad = f64::from(theme.tokens.spacing.s3);
        let gap = theme.tokens.spacing.s2;
        let left = window.x0 + pad;
        let span = (left, (window.x1 - pad).max(left));
        let mut ctx = PaintCtx::new(scene, tf, tr, theme);
        let mut y = window.y0 + pad;
        if let Some(view) = alone.view.as_mut() {
            view.unplace();
            y = view.place(&mut ctx, span, y, window) + f64::from(gap);
        }
        let state = alone.state.as_ref();
        let said = Said {
            stands: alone.stands,
            refused: state.map(|state| state.refused()).unwrap_or_default(),
            unread: state.map(|state| state.unread()).unwrap_or_default(),
            suggestions: state.map(|state| state.suggestions()).unwrap_or_default(),
            by,
        };
        let key = format!("{said:?} {}", theme.text_scale);
        alone.under.unplace();
        if key != alone.under_key {
            alone.under_key = key;
            alone.under.replace(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .push(said.widget(&alone.answers, gap)),
            );
        }
        y = alone.under.place(&mut ctx, span, y, window);
        let height = (y - window.y0 + pad).ceil();
        if (height - alone.height).abs() >= 1.0 {
            alone.height = height;
        }
    }

    /// Who wrote the view shown alone, as the conversation names them.
    fn alone_author(&self) -> String {
        self.alone
            .as_ref()
            .and_then(|alone| {
                self.search_messages
                    .iter()
                    .find(|item| item.id == alone.message)
            })
            .map(|message| self.message_author_label(message))
            .unwrap_or_default()
    }

    /// Hand one event to the view and to what stands under it, and do what the person
    /// answered there.
    pub(crate) fn alone_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let Some(alone) = self.alone.as_mut() else {
            return;
        };
        if let Some(view) = alone.view.as_mut() {
            view.handle_event(event, ctx);
        }
        if !*ctx.handled {
            alone.under.handle_event(event, ctx);
        }
        let answers = std::mem::take(&mut *alone.answers.borrow_mut());
        if let Some(state) = alone.state.as_ref() {
            for answer in &answers {
                match answer {
                    Answer::Allow(id) => state.allow(*id),
                    Answer::Dismiss(id) => state.dismiss(*id),
                }
            }
        }
        if *ctx.handled || !answers.is_empty() {
            self.canonical_dirty = true;
        }
    }

    /// The size what is shown alone asks for: the window's width, and the height of the view
    /// with everything said under it.
    pub(crate) fn alone_size(&self) -> Option<(f64, f64)> {
        let alone = self.alone.as_ref()?;
        (alone.height > 0.0).then(|| (self.rect.width(), alone.height))
    }

    pub(crate) fn alone_children(&self) -> Vec<&dyn Widget> {
        let Some(alone) = self.alone.as_ref() else {
            return Vec::new();
        };
        alone
            .view
            .as_ref()
            .and_then(|view| view.visible())
            .map(|column| column as &dyn Widget)
            .into_iter()
            .chain(alone.under.visible().map(|column| column as &dyn Widget))
            .collect()
    }

    pub(crate) fn alone_children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children: Vec<&mut (dyn Widget + '_)> = Vec::new();
        let Some(alone) = self.alone.as_mut() else {
            return children;
        };
        if let Some(column) = alone.view.as_mut().and_then(|view| view.visible_mut()) {
            children.push(column);
        }
        if let Some(column) = alone.under.visible_mut() {
            children.push(column);
        }
        children
    }

    /// Whether the view shown alone moves by itself now.
    pub(crate) fn alone_needs_frame(&self) -> bool {
        self.alone
            .as_ref()
            .and_then(|alone| alone.view.as_ref())
            .is_some_and(|view| view.widget().needs_animation_frame())
    }

    /// Whether the view shown alone owes the window a sync: its instance has work that no frame
    /// has taken in yet. The column that holds the view answers for none of its children, so
    /// the view's own widget is asked.
    pub(crate) fn alone_owes_sync(&self) -> bool {
        let mut owes = false;
        if let Some(view) = self.alone.as_ref().and_then(|alone| alone.view.as_ref()) {
            view.widget()
                .for_each_child(&mut |child| owes |= child.needs_idle_sync());
        }
        owes
    }

    /// When the view shown alone next wants the window woken.
    pub(crate) fn alone_wake(&self) -> Option<ui::time::Instant> {
        self.alone
            .as_ref()
            .and_then(|alone| alone.view.as_ref())
            .and_then(|view| view.widget().next_wake_deadline())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Once;

    use automerge::ROOT;
    use ui::Widget;
    use ui::tilo_actions::Actor;
    use ui::tilo_resource::{Acting, Id, MemberId, NonBlank};
    use ui::widget::{DrawChild as _, Text};
    use ui::written_view::{
        MakeWrittenView, Shown, Suggestion, ViewState, WRITTEN_VIEWS, WrittenView,
    };

    use super::code_author;
    use crate::ChatApp;
    use crate::effects::ChatEffect;
    use crate::identity::LocalIdentity;
    use crate::messages::chat_messages;
    use crate::model::ME;
    use crate::parts::ChatMessages;
    use crate::testing::Workspace;

    const GENERAL: &str = "c_general";
    const ARIA: &str = "u_aria";
    const AREAS: &str =
        "view areas() {\n    for a in tasks.tracker.areas {\n        text(a.name)\n    }\n}";
    const NAMES: &str =
        "view names() {\n    for a in tasks.tracker.areas {\n        row(a.name)\n    }\n}";

    thread_local! {
        /// Every view this thread's window handed over to be shown, as it named it.
        static PLACED: RefCell<Vec<WrittenView>> = const { RefCell::new(Vec::new()) };
    }

    struct Nothing;

    impl ViewState for Nothing {
        fn suggestions(&self) -> Vec<Suggestion> {
            Vec::new()
        }
        fn allow(&self, _: u64) {}
        fn dismiss(&self, _: u64) {}
        fn refused(&self) -> Vec<String> {
            Vec::new()
        }
        fn unread(&self) -> Vec<String> {
            Vec::new()
        }
        fn revision(&self) -> u64 {
            0
        }
    }

    thread_local! {
        /// Whether the widget this thread's views are shown by owes its window a sync.
        static OWES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// What a view is shown by here: a line of text that owes its window a sync while [`OWES`]
    /// says so, as a written view does while its instance owes a frame.
    struct Owing(ui::widget::Element);

    impl ui::widget::Decorator for Owing {
        type Inner = dyn Widget;

        fn inner(&self) -> &(dyn Widget + 'static) {
            &*self.0
        }

        fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
            &mut *self.0
        }

        fn needs_idle_sync(&self) -> bool {
            OWES.with(std::cell::Cell::get)
        }
    }

    fn make(view: &WrittenView, _: &ui::RemoteActions) -> Shown {
        PLACED.with(|placed| placed.borrow_mut().push(view.clone()));
        Shown {
            widget: Box::new(Owing(Text::new("shown").into_widget())),
            state: Rc::new(Nothing),
        }
    }

    /// The code that shows views is in, for every test of this process: one that only notes
    /// what it was handed.
    fn views_are_shown() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| WRITTEN_VIEWS.install(MakeWrittenView(make)));
        crate::i18n::ensure();
    }

    fn placed() -> Vec<WrittenView> {
        PLACED.with(|placed| placed.borrow().clone())
    }

    fn window(workspace: &Workspace, identity: LocalIdentity) -> ChatApp {
        let me = identity.id.clone();
        let mut app = ChatApp::new(
            identity,
            crate::app::ChatRuntime::Unavailable,
            tilo_content::ContentStore::memory(),
        );
        app.attach_framework(ui::framework::Dispatcher::detached("so.tilo.chat", &me));
        app.attach_workspace(workspace);
        app.sync(&workspace.chat, &ROOT);
        app
    }

    fn aria() -> LocalIdentity {
        LocalIdentity {
            id: MemberId::person(ARIA),
            name: "Aria".to_owned(),
            initials: "A".to_owned(),
        }
    }

    /// The person [`ME`] posts a view over Tasks; the id of the message that carries it.
    fn posted(workspace: &mut Workspace) -> String {
        crate::testing::apply_part_as(
            workspace,
            ME,
            ChatMessages::POST_VIEW.with(chat_messages::PostView {
                channel: Id::from_raw(GENERAL),
                text: NonBlank::parse("The areas, as they stand").expect("words"),
                source: AREAS.to_owned(),
                reads: vec!["tasks".to_owned()],
                parent: None,
            }),
        )
        .expect("a person posts a view");
        crate::testing::messages(workspace, GENERAL)
            .into_iter()
            .rev()
            .find(|message| message.view.is_some())
            .expect("the message that carries the view")
            .id
    }

    /// A written view owes its window a sync while its instance has work that no frame has taken
    /// in yet (a change no sync has sent the window, an action handed on, work under way off the
    /// frame), and says so. The toolkit's frame asks the window and never the view inside it:
    /// the window of a view shown alone hands the view's word on. Where it does not, that work
    /// waits for whatever else next makes a frame.
    #[test]
    fn the_window_owes_a_sync_while_the_view_it_shows_alone_owes_one() {
        views_are_shown();
        let mut workspace = crate::testing::demo_doc();
        let message = posted(&mut workspace);
        let mut app = window(&workspace, aria());
        app.answer_effect(ChatEffect::ShowView { message })
            .expect("the view is shown alone");
        Widget::update(&mut app, &workspace.chat, &ROOT);
        let window = crate::ChatWindow::new(Rc::new(RefCell::new(app)));
        assert!(
            !Widget::needs_idle_sync(&window),
            "a view at rest owes its window nothing"
        );
        OWES.with(|owes| owes.set(true));
        let owed = Widget::needs_idle_sync(&window);
        OWES.with(|owes| owes.set(false));
        assert!(
            owed,
            "the view shown alone owes a sync and its window does not say so"
        );
    }

    #[test]
    fn the_code_of_a_view_is_its_messages_author_or_the_software_that_wrote_for_them() {
        let ada = MemberId::person("ada");
        assert_eq!(code_author(&ada, None), Actor::Member(ada.clone()));
        // What an agent wrote for Ada is the agent's code, never Ada's own.
        let helper = Acting::agent("helper").acting_for(Acting::person("ada"));
        let author = code_author(&ada, Some(&helper));
        assert_ne!(author, Actor::Member(ada));
        assert_eq!(author, helper.by);
        let agent = MemberId::agent("tide");
        assert_eq!(code_author(&agent, None), Actor::Member(agent));
    }

    #[test]
    fn a_message_written_in_its_readers_name_is_not_shown_as_the_readers_own_view() {
        views_are_shown();
        let mut workspace = crate::testing::demo_doc();
        // Below Chat's verbs a member's device writes what it likes into a change it signs:
        // here the change a post makes, naming the reader as whoever invoked it. The message
        // keeps nothing else of who wrote it, so this is all the reader's device is shown.
        let message = posted(&mut workspace);

        // A view is trusted where its author is its reader, and the name is only a field: the
        // reader's window does not hand the view over as the reader's own.
        let mut app = window(&workspace, LocalIdentity::demo());
        app.answer_effect(ChatEffect::ShowView { message })
            .expect("the window is told the message");
        Widget::update(&mut app, &workspace.chat, &ROOT);
        assert!(placed().is_empty(), "nothing is placed: {:?}", placed());
        assert_eq!(
            app.alone.as_ref().map(|alone| alone.stands),
            Some(super::Stands::Unbound)
        );
        // Looked at again, it still is not.
        Widget::update(&mut app, &workspace.chat, &ROOT);
        assert!(placed().is_empty(), "nothing is placed: {:?}", placed());
    }

    #[test]
    fn a_messages_view_is_shown_alone_as_its_reader_and_as_its_authors_code() {
        views_are_shown();
        let mut workspace = crate::testing::demo_doc();
        let message = posted(&mut workspace);

        // Another member reads it: the code is its author's, the eyes are the reader's.
        let mut app = window(&workspace, aria());
        app.answer_effect(ChatEffect::ShowView {
            message: message.clone(),
        })
        .expect("the view is shown alone");
        Widget::update(&mut app, &workspace.chat, &ROOT);
        let shown = placed();
        assert_eq!(shown.len(), 1, "placed once: {shown:?}");
        assert_eq!(shown[0].source, AREAS);
        assert_eq!(shown[0].reads, ["tasks"]);
        assert_eq!(shown[0].author, Actor::Member(MemberId::person(ME)));
        assert_eq!(Some(&shown[0].viewer), MemberId::person(ARIA).as_person());
        // Nothing moved: it is not placed a second time.
        Widget::update(&mut app, &workspace.chat, &ROOT);
        assert_eq!(placed().len(), 1);

        // Its author writes it again: the same window shows the new version.
        crate::testing::apply_part_as(
            &mut workspace,
            ME,
            ChatMessages::REWRITE_VIEW.with(chat_messages::RewriteView {
                message: Id::from_raw(message),
                source: NAMES.to_owned(),
                reads: vec!["tasks".to_owned()],
            }),
        )
        .expect("its author writes the view again");
        app.attach_workspace(&workspace);
        app.sync(&workspace.chat, &ROOT);
        Widget::update(&mut app, &workspace.chat, &ROOT);
        let shown = placed();
        assert_eq!(shown.len(), 2, "the new version is placed: {shown:?}");
        assert_eq!(shown[1].source, NAMES);
    }

    #[test]
    fn another_reader_is_shown_the_view_as_themselves_and_as_its_authors_code() {
        views_are_shown();
        let mut workspace = crate::testing::demo_doc();
        let message = posted(&mut workspace);
        let mut app = window(&workspace, aria());
        app.answer_effect(ChatEffect::ShowView { message })
            .expect("the view is shown alone");
        Widget::update(&mut app, &workspace.chat, &ROOT);
        let shown = placed();
        assert_eq!(shown.len(), 1, "placed once: {shown:?}");
        // Whoever looks at it, the code stays its author's, and the eyes are the reader's.
        assert_eq!(shown[0].author, Actor::Member(MemberId::person(ME)));
        assert_eq!(Some(&shown[0].viewer), MemberId::person(ARIA).as_person());
    }

    #[test]
    fn a_message_without_a_view_and_a_message_that_is_not_there_are_refused() {
        views_are_shown();
        let workspace = crate::testing::demo_doc();
        let plain = crate::testing::messages(&workspace, GENERAL)
            .into_iter()
            .find(|message| message.view.is_none())
            .expect("a seeded message")
            .id;
        let mut app = window(&workspace, LocalIdentity::demo());
        let refused = app
            .answer_effect(ChatEffect::ShowView { message: plain })
            .expect_err("a message without a view shows none");
        assert!(
            format!("{refused:?}").contains("carries no view"),
            "{refused:?}"
        );
        let missing = app
            .answer_effect(ChatEffect::ShowView {
                message: "m_nowhere".to_owned(),
            })
            .expect_err("a message that is not there shows nothing");
        assert!(
            format!("{missing:?}").contains("no longer exists"),
            "{missing:?}"
        );
        Widget::update(&mut app, &workspace.chat, &ROOT);
        assert!(placed().is_empty(), "nothing was placed");
        assert!(app.alone_size().is_none());
    }

    /// Every name the widgets from `widget` down say for assistive use, as a walk of a window's
    /// widgets finds them.
    fn said(widget: &dyn Widget, out: &mut Vec<String>) {
        if let Some(name) = widget.accessibility().and_then(|node| node.name) {
            out.push(name);
        }
        widget.for_each_child(&mut |child| said(child, out));
    }

    /// A page asks the window it mounts, not the chat inside it: how tall what it shows asks to
    /// be, and what it says for assistive use. The window answers both from the view it shows
    /// alone.
    #[test]
    fn the_window_says_the_height_and_the_words_of_the_view_it_shows_alone() {
        views_are_shown();
        let mut workspace = crate::testing::demo_doc();
        let message = posted(&mut workspace);
        let mut app = window(&workspace, aria());
        app.answer_effect(ChatEffect::ShowView { message })
            .expect("the view is shown alone");
        Widget::update(&mut app, &workspace.chat, &ROOT);
        app.rect = vello::kurbo::Rect::new(0.0, 0.0, 620.0, 400.0);
        let mut window = crate::ChatWindow::new(Rc::new(RefCell::new(app)));
        let theme = ui::theme::Theme::dark();
        let mut text = ui::text_render::TextRenderer::new();
        let mut scene = vello::Scene::new();
        window.draw_child(
            &mut scene,
            vello::kurbo::Affine::IDENTITY,
            &mut text,
            &theme,
            &ui::presence::PresenceCtx::empty(),
        );

        let (width, height) = ui::widget::preferred_window_size(&window)
            .expect("the window says the size of the view it shows alone");
        assert!(
            (width - 620.0).abs() < 1.0,
            "the window's own width: {width}"
        );
        assert!(
            height > 0.0 && height < 400.0,
            "the view's own height, not the window's: {height}"
        );
        let mut names = Vec::new();
        said(&window, &mut names);
        assert!(
            names.iter().any(|name| name == "shown"),
            "what the view draws is said through the window: {names:?}"
        );
    }

    /// A view shown alone is all its window shows. A program that mounts Chat in a message's
    /// place for its view (a person's app, in the conversation it keeps itself) gets the view
    /// and nothing of Chat's window around it: Chat declares no parts for the window's bar and
    /// asks its shell for the one thing alone, so no sidebar stands beside the view. A
    /// conversation shown again brings both back.
    #[test]
    fn a_view_shown_alone_has_nothing_of_chats_window_around_it() {
        views_are_shown();
        let mut workspace = crate::testing::demo_doc();
        let message = posted(&mut workspace);
        let shell = ui::widget::NavShell::new().with_window_toolbar();
        let bar = shell.window_bar_link();
        let mut app = window(&workspace, aria()).in_shell(&ui::widget::NavFeed::new(), bar.clone());
        Widget::sync(&mut app, &workspace.chat, &ROOT);
        assert!(
            ui::window_toolbar::declared().is_some(),
            "a conversation's window declares its parts for the bar"
        );
        assert!(!bar.shows_content_alone());

        app.answer_effect(ChatEffect::ShowView { message })
            .expect("the view is shown alone");
        Widget::sync(&mut app, &workspace.chat, &ROOT);
        assert!(
            ui::window_toolbar::declared().is_none(),
            "nothing of Chat stands in the bar over a view shown alone"
        );
        assert!(
            bar.shows_content_alone(),
            "the shell is asked for the view alone: no sidebar beside it"
        );

        app.answer_effect(ChatEffect::OpenChannel {
            channel: GENERAL.to_owned(),
        })
        .expect("the conversation is shown again");
        Widget::sync(&mut app, &workspace.chat, &ROOT);
        assert!(
            ui::window_toolbar::declared().is_some(),
            "the conversation's parts stand in the bar again"
        );
        assert!(!bar.shows_content_alone());
    }
}
