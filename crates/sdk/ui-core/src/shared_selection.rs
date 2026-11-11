//! What the person has selected in a window, for sending it somewhere.
//!
//! A person who selects a sentence, a range of cells or a row and sends it to
//! an agent or a person means that selection. The toolkit derives it once,
//! from the focused widget, so every app gets it without code of its own: the
//! place in the resource's terms (an item and character offsets, the form an
//! action's place argument takes), the resource it is in, and a short
//! excerpt. A widget that knows its selection better (a multi-block editor)
//! says so through [`PublishesPresence::shared_selection`].
//!
//! Every context menu takes typing ([`crate::widget::ContextMenu`]): its send
//! row carries a sentence its entries do not answer into the send-to box that
//! takes the menu's place ([`crate::widget::SendToBox`]), where the person
//! names whom to send it to. The box queues the request ([`send_to`]), and the
//! app runner hands it to the session with a typed reference
//! ([`resolve_selection`]), which the session acts on as the person: it makes
//! a new agent when one is a recipient, opens or makes the one conversation,
//! and posts the message there. The reference names a place; it grants
//! nothing, so an agent reaches it only through the app's published actions,
//! as every task does.
//!
//! [`PublishesPresence::shared_selection`]: crate::presence::capability::PublishesPresence::shared_selection

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};
use tilo_actions::{ActionResource, SelectionAnchor, SelectionPlace};

use crate::collaboration::PresencePayload;
use crate::floating_surface::Recipient;
use crate::presence::PresenceKey;
use crate::presence::capability::PublishesPresence;
use crate::widget::Widget;
use crate::widget::capability::capability;

/// A person's selection in one window: the resource it is in, when known,
/// and where in it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedSelection {
    /// The published resource the selection is in. The app runner fills it
    /// from its publication when the widget does not name one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<ActionResource>,
    /// The place, the application's own interaction anchor and the excerpt.
    pub anchor: SelectionAnchor,
    /// The kind of the resource the place's item belongs to, which picks the
    /// published resource when [`Self::resource`] is absent. Local only.
    #[serde(skip)]
    pub resource_kind: Option<String>,
    /// Where the selection is drawn, in the window's logical coordinates:
    /// the selected text's lines, else the text block holding it. The shell
    /// keeps its agent composer clear of it. Local only: geometry is no part
    /// of what an agent is told.
    #[serde(skip)]
    pub bounds: Option<vello::kurbo::Rect>,
}

impl SharedSelection {
    /// Selected `text`, with no place.
    #[must_use]
    pub fn of_text(text: &str) -> Self {
        Self {
            anchor: SelectionAnchor::of_text(text),
            ..Self::default()
        }
    }

    /// Several whole items selected, each `(id, label)` in the order shown
    /// (files in a folder, rows of a list): every item named as a place and
    /// the excerpt listing their labels.
    #[must_use]
    pub fn of_items<I, L>(items: impl IntoIterator<Item = (I, L)>) -> Self
    where
        I: Into<String>,
        L: AsRef<str>,
    {
        Self {
            anchor: SelectionAnchor::of_items(items),
            ..Self::default()
        }
    }

    /// This selection at `place`.
    #[must_use]
    pub fn at(mut self, place: SelectionPlace) -> Self {
        self.anchor = self.anchor.at(place);
        self
    }

    /// This selection in `resource`.
    #[must_use]
    pub fn in_resource(mut self, resource: ActionResource) -> Self {
        self.resource = Some(resource);
        self
    }

    /// This selection in a resource of `kind`, resolved against the app's
    /// publication.
    #[must_use]
    pub fn in_kind(mut self, kind: impl Into<String>) -> Self {
        self.resource_kind = Some(kind.into());
        self
    }

    /// This selection drawn within `bounds`, in window coordinates.
    #[must_use]
    pub fn within(mut self, bounds: vello::kurbo::Rect) -> Self {
        self.bounds = Some(bounds);
        self
    }

    /// Where the selection is drawn, as the shell protocol carries it:
    /// `None` when unknown or degenerate.
    #[must_use]
    pub fn protocol_bounds(&self) -> Option<[f64; 4]> {
        self.bounds
            .filter(|rect| {
                [rect.x0, rect.y0, rect.x1, rect.y1]
                    .iter()
                    .all(|value| value.is_finite())
                    && rect.width() > 0.0
                    && rect.height() > 0.0
            })
            .map(crate::floating_surface::protocol_rect_array)
    }

    /// This selection as a thing to send: the record it is in, with the item
    /// or the run of characters it names, or — when the app publishes no
    /// resource to go with the words — the words quoted.
    ///
    /// `app` names the app that publishes that record and `record` its
    /// workspace record (`None`: the app's own); `app` is `None` when nobody
    /// says which app's record it is, and what travels then is the words
    /// alone: a reference to no app addresses nothing.
    /// A selection of several items travels as those items; a range inside
    /// one travels as that item and its character span, in the app's own
    /// unit, so the app can address the run again when the chip is opened.
    /// `None` when the person selected nothing worth sending.
    #[must_use]
    pub fn to_attached(
        &self,
        app: Option<&str>,
        record: Option<&str>,
    ) -> Option<tilo_reference::Attached> {
        use tilo_reference::{Anchor, Attached, ResourceAt, ResourceRef, Span};

        let excerpt = self
            .anchor
            .excerpt
            .as_deref()
            .map(str::trim)
            .filter(|excerpt| !excerpt.is_empty());
        let place = self.anchor.place.as_ref();
        let ids: Vec<String> = self
            .anchor
            .items
            .iter()
            .map(|item| item.item.clone())
            .collect();
        let (Some(resource), Some(app)) = (self.resource.as_ref(), app) else {
            // Nothing but the words: worth reading, opening nothing.
            return excerpt.map(|text| Attached::Text {
                text: text.to_owned(),
                from: None,
            });
        };
        let span = place
            .and_then(|place| Some(Span::chars(place.start?, place.end?)))
            .filter(|span| !span.is_empty());
        let anchor = if ids.is_empty() {
            place.map(|place| Anchor::item(place.item.clone()))
        } else {
            Some(Anchor::items(ids))
        };
        let at = ResourceAt {
            app: app.to_owned(),
            record: record.map(str::to_owned),
            reference: ResourceRef {
                kind: resource.kind.clone(),
                id: resource.id.clone(),
                label: resource.label.clone(),
                span,
            },
        };
        if anchor.is_none() && excerpt.is_some() {
            // A selection the app reports only as words: quote them, and say
            // which record they came from so the chip can open it.
            return Some(Attached::Text {
                text: excerpt.unwrap_or_default().to_owned(),
                from: Some(at),
            });
        }
        // What the person saw on the box's chip: the item picked ("#1 Write the launch notes"),
        // never only the list it is in.
        let label = crate::widget::send_to::subject_label(self);
        (!label.is_empty()).then_some(Attached::Resource { at, anchor, label })
    }

    /// Whether there is nothing to ask about.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.anchor.is_empty()
    }

    /// Name the published resource the selection is in when the widget did
    /// not: the resource of [`Self::resource_kind`], else the first one.
    pub fn resolve_resource(&mut self, published: &[tilo_actions::ResourceSpec]) {
        if self.resource.is_some() {
            return;
        }
        fn find<'a>(
            resources: &'a [tilo_actions::ResourceSpec],
            kind: &str,
            depth: usize,
        ) -> Option<&'a tilo_actions::ResourceSpec> {
            if depth > 4 {
                return None;
            }
            resources
                .iter()
                .find(|resource| resource.kind == kind)
                .or_else(|| {
                    resources
                        .iter()
                        .find_map(|resource| find(&resource.children, kind, depth + 1))
                })
        }
        let chosen = self
            .resource_kind
            .as_deref()
            .and_then(|kind| find(published, kind, 0))
            .or_else(|| published.first());
        self.resource = chosen.map(|resource| ActionResource {
            id: resource.id.0.clone(),
            kind: resource.kind.clone(),
            label: resource.label.clone(),
        });
    }
}

/// The selection `widget` holds, if it is focused and holds one.
///
/// A widget's own [`PublishesPresence::shared_selection`] wins. Otherwise
/// it is derived from what the widget already publishes: its outbound
/// presence (focused only), keyed to a resource item, gives the place; its
/// accessibility node's value and text selection give the excerpt. A field
/// that hides its value from assistive technology (a password) shares
/// nothing.
pub fn selection_of(widget: &(impl Widget + ?Sized)) -> Option<SharedSelection> {
    let presence = capability::<dyn PublishesPresence>(widget)?;
    if let Some(selection) = presence.shared_selection() {
        return (!selection.is_empty()).then_some(selection);
    }
    let (key, payload) = presence.presence_payload()?;
    let PresencePayload::Selection { start, end } = payload else {
        return None;
    };
    let node = widget.accessibility();
    if node.as_ref().is_some_and(|node| node.state.protected) {
        return None;
    }
    let (start, end) = (start.min(end), start.max(end));
    let text = node.as_ref().and_then(|node| {
        let value = node.value.as_deref()?;
        let selected = node.text_selection?;
        let (from, to) = (
            selected.anchor.min(selected.focus),
            selected.anchor.max(selected.focus),
        );
        // The excerpt is the selection only when the node reports the same
        // range the widget publishes, so both describe one text.
        ((from, to) == (start, end))
            .then(|| value.get(from..to).map(|text| (value, from, to, text)))
            .flatten()
    });
    let selection = match (&key, text) {
        (PresenceKey::Resource { id, kind, .. }, Some((value, from, to, text))) => {
            let chars = |byte: usize| value.get(..byte).map_or(0, |head| head.chars().count());
            SharedSelection::of_text(text)
                .at(SelectionPlace::range(id.clone(), chars(from), chars(to)))
                .in_kind(kind.clone())
        }
        // A collapsed selection keyed to an item is that item selected
        // (a row, a clip, a card).
        (PresenceKey::Resource { id, kind, .. }, None) if start == end => SharedSelection {
            anchor: SelectionAnchor::default().at(SelectionPlace::item(id.clone())),
            ..SharedSelection::default()
        }
        .in_kind(kind.clone()),
        (_, Some((_, _, _, text))) => SharedSelection::of_text(text),
        _ => return None,
    };
    // The widget holding the selection is the text block it is drawn in.
    let selection = selection.within(widget.get_rect());
    (!selection.is_empty()).then_some(selection)
}

/// Nodes [`subtree_selection`] visits at most.
const SUBTREE_NODES: usize = 4096;

/// The selection of the focused widget under `root` (itself included): the
/// first in tree order that holds one. Bounded in nodes visited.
pub fn subtree_selection(root: &dyn Widget) -> Option<SharedSelection> {
    fn walk(widget: &dyn Widget, visited: &mut usize, found: &mut Option<SharedSelection>) {
        if found.is_some() || *visited >= SUBTREE_NODES {
            return;
        }
        *visited += 1;
        if let Some(selection) = selection_of(widget) {
            *found = Some(selection);
            return;
        }
        widget.for_each_child(&mut |child| walk(child, visited, found));
    }
    let mut found = None;
    walk(root, &mut 0, &mut found);
    found
}

/// The one subject rule for what pointing at `at` is about, given the
/// `focused` selection: the selection when the person pointed inside it (or
/// did not point at all, with the keyboard); `None` otherwise, for the
/// caller to name the item under the pointer, else the window. A selection
/// that does not say where it is drawn is not pointed inside.
#[must_use]
pub fn pointed_selection(
    focused: Option<SharedSelection>,
    at: Option<vello::kurbo::Point>,
) -> Option<SharedSelection> {
    focused.filter(|selection| match (at, selection.bounds) {
        (Some(at), Some(bounds)) => bounds.contains(at),
        (Some(_), None) => false,
        (None, _) => true,
    })
}

/// Upper bound on undelivered requests: a repeated chord or menu choice
/// faster than frames are drawn must not queue unbounded boxes.
const MAX_PENDING: usize = 8;

/// The longest message a box sends, in characters: well inside a protocol
/// frame, and more than anyone types into one.
pub const MAX_SEND_CHARS: usize = 20_000;

/// Someone asked to send a message from this window to these recipients
/// about what they selected.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SendToRequest {
    /// Whom it goes to: the box's recipients, a new agent among them when
    /// the person is making one.
    pub to: Vec<Recipient>,
    /// What it is about: the chips, in the order they show.
    pub about: Vec<SharedSelection>,
    /// What the person wrote.
    pub text: String,
}

/// Where in the window a request points, and what it is about: the reference
/// the app runner hands the session, resolved against what the window
/// shows.
///
/// What its asker named wins. Otherwise the `focused` selection, when the
/// person pointed at it (or did not point at all). Otherwise the window
/// itself: the first resource the app `published`, as one whole item. Its
/// resource is named from `published` when the reference does not name one.
#[must_use]
pub fn resolve_selection(
    about: Option<SharedSelection>,
    at: Option<vello::kurbo::Point>,
    focused: Option<SharedSelection>,
    published: &[tilo_actions::ResourceSpec],
) -> (Option<SharedSelection>, Option<[f64; 4]>) {
    let focused = pointed_selection(focused, at);
    let mut about = about
        .filter(|about| !about.is_empty())
        .or(focused)
        .or_else(|| {
            let window = published.first()?;
            Some(
                SharedSelection::of_text(&window.label)
                    .at(SelectionPlace::item(window.id.0.as_str()))
                    .in_resource(ActionResource {
                        id: window.id.0.clone(),
                        kind: window.kind.clone(),
                        label: window.label.clone(),
                    }),
            )
        });
    if let Some(about) = about.as_mut() {
        about.resolve_resource(published);
    }
    let pointed = at.map(|at| {
        crate::floating_surface::protocol_rect_array(vello::kurbo::Rect::from_center_size(
            at,
            (1.0, 1.0),
        ))
    });
    let bounds = about
        .as_ref()
        .and_then(SharedSelection::protocol_bounds)
        .or(pointed);
    (about, bounds)
}

thread_local! {
    static PENDING: RefCell<VecDeque<SendToRequest>> = const { RefCell::new(VecDeque::new()) };
    /// Whether a desktop is connected that can send what a box holds.
    static HOSTED: Cell<bool> = const { Cell::new(false) };
    static FOCUSED: RefCell<Option<SharedSelection>> = const { RefCell::new(None) };
    /// Whether a send-to box is open, so the app runner reads the window's
    /// selection each pass for its "+".
    static BOX_OPEN: Cell<bool> = const { Cell::new(false) };
    /// The searches the boxes asked for, waiting for the app runner.
    static SEARCHES: RefCell<VecDeque<(u64, String)>> = const { RefCell::new(VecDeque::new()) };
    /// The next request number: one per search, answered by number.
    static NEXT_SEARCH: Cell<u64> = const { Cell::new(1) };
    /// What the search answered, by request, waiting for the box that asked.
    static ROWS: RefCell<HashMap<u64, Vec<Recipient>>> = RefCell::new(HashMap::new());
    /// A box asked for with no menu around it: what is selected, where to
    /// stand, what is already typed, and whom the session opened it to.
    static ALONE: RefCell<Option<OpenSendBox>> = const { RefCell::new(None) };
}

/// A box a window is asked to open with no menu around it.
#[derive(Clone, Debug, Default)]
pub struct OpenSendBox {
    /// What the box is about, when the window knows of a selection.
    pub about: Option<SharedSelection>,
    /// Where what it is about stands, so the box opens beside it.
    pub bounds: Option<[f64; 4]>,
    /// What is already typed into it.
    pub draft: Option<String>,
    /// Whom the session opened it to, when it named them: the one place a
    /// report goes. Empty for a box the person addresses.
    pub to: Vec<Recipient>,
}

/// Open the box with no menu: what a window's own key does over a selection,
/// and what the shell's send key or its "report a problem" asks of an app.
pub fn open_send_box(
    about: Option<SharedSelection>,
    bounds: Option<[f64; 4]>,
    draft: Option<&str>,
    to: &[Recipient],
) {
    ALONE.with(|alone| {
        *alone.borrow_mut() = Some(OpenSendBox {
            about,
            bounds,
            draft: draft.map(|draft| draft.chars().take(MAX_SEND_CHARS).collect()),
            to: to.to_vec(),
        });
    });
}

/// A box asked for with no menu since the last call. For the area that
/// carries one.
pub fn take_open_send_box() -> Option<OpenSendBox> {
    ALONE.with(|alone| alone.borrow_mut().take())
}

/// Send what a box holds: the app runner hands it to the session, which acts
/// on it as the person.
pub fn send_to(request: SendToRequest) {
    PENDING.with(|pending| {
        let mut pending = pending.borrow_mut();
        if pending.len() < MAX_PENDING {
            pending.push_back(request);
        }
    });
}

/// Requests made since the last call, oldest first. For the app runner.
pub fn take_send_to() -> Vec<SendToRequest> {
    PENDING.with(|pending| pending.borrow_mut().drain(..).collect())
}

/// Whether a desktop is connected that can send what a box holds, so a menu
/// offers it. Outside a session the box is not offered.
#[must_use]
pub fn send_host_available() -> bool {
    HOSTED.with(Cell::get)
}

/// Record whether a desktop that can send what a box holds is connected.
/// For the app runner.
pub fn set_send_host_available(available: bool) {
    HOSTED.with(|hosted| hosted.set(available));
}

/// What the person has selected in this window now, as the app runner last
/// read it: what a box's "+" adds as a subject.
#[must_use]
pub fn focused_selection() -> Option<SharedSelection> {
    FOCUSED.with(|focused| focused.borrow().clone())
}

/// Record the window's selection, read each pass by the app runner.
pub fn set_focused_selection(selection: Option<SharedSelection>) {
    FOCUSED.with(|focused| *focused.borrow_mut() = selection);
}

/// Whether a send-to box is open in this window.
#[must_use]
pub fn box_open() -> bool {
    BOX_OPEN.with(Cell::get)
}

/// Record whether a send-to box is open. For the box itself.
pub fn set_box_open(open: bool) {
    BOX_OPEN.with(|box_open| box_open.set(open));
}

/// Ask the one search for the recipients matching `query`; the answer comes
/// back through [`answer_recipients`] under the number returned. Empty
/// queries ask for the ones the person reached for last.
pub fn search_recipients(query: &str) -> u64 {
    let request = NEXT_SEARCH.with(|next| {
        let request = next.get();
        next.set(request.saturating_add(1));
        request
    });
    SEARCHES.with(|searches| {
        let mut searches = searches.borrow_mut();
        if searches.len() < MAX_PENDING {
            searches.push_back((request, query.chars().take(200).collect()));
        }
    });
    request
}

/// The searches boxes asked for since the last call, oldest first. For the
/// app runner, which asks the session.
pub fn take_recipient_searches() -> Vec<(u64, String)> {
    SEARCHES.with(|searches| searches.borrow_mut().drain(..).collect())
}

/// What the search found for `request`, waiting for the box that asked.
pub fn answer_recipients(request: u64, rows: Vec<Recipient>) {
    ROWS.with(|rows_by_request| {
        rows_by_request.borrow_mut().insert(request, rows);
    });
}

/// The rows for `request`, if the search has answered: taken once, so a box
/// never draws the same answer twice.
pub fn take_recipients(request: u64) -> Option<Vec<Recipient>> {
    ROWS.with(|rows| rows.borrow_mut().remove(&request))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_selection_travels_as_json_without_its_local_kind_hint() {
        let selection = SharedSelection::of_text("late results")
            .at(SelectionPlace::range("block-2", 4, 16))
            .in_kind("rich-text");
        let wire = serde_json::to_value(&selection).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({
                "anchor": {
                    "place": {"item": "block-2", "start": 4, "end": 16},
                    "excerpt": "late results"
                }
            })
        );
        let back: SharedSelection = serde_json::from_value(wire).unwrap();
        assert_eq!(back.anchor, selection.anchor);
        assert_eq!(back.resource_kind, None);
        assert!(
            serde_json::from_value::<SharedSelection>(serde_json::json!({
                "anchor": {}, "authority": "admin"
            }))
            .is_err()
        );
    }

    #[test]
    fn a_selection_becomes_the_record_and_part_it_names() {
        use tilo_reference::{Anchor, Attached, ResourceAt, ResourceRef, Span};

        let page = ActionResource {
            id: "notes.page:1".into(),
            kind: "notes.page".into(),
            label: "Plan".into(),
        };
        // A run inside one item travels as that item and the characters, in
        // the app's own unit, so the app can address the run again.
        let run = SharedSelection::of_text("late results")
            .at(SelectionPlace::range("block-2", 4, 16))
            .in_resource(page.clone());
        let at = ResourceAt {
            app: "so.tilo.notes".into(),
            record: Some("uuid:7".into()),
            reference: ResourceRef::whole("notes.page", "notes.page:1", "Plan"),
        };
        let sent = run.to_attached(Some("so.tilo.notes"), Some("uuid:7"));
        let run_at = ResourceAt {
            reference: ResourceRef::ranged(
                "notes.page",
                "notes.page:1",
                "Plan",
                Span::chars(4, 16),
            ),
            ..at.clone()
        };
        assert_eq!(
            sent,
            Some(Attached::resource(
                run_at,
                Some(Anchor::item("block-2")),
                "late results"
            )),
            "the chip reads the words picked, as the box's chip did, not the page they are in"
        );
        let Some(Attached::Resource { at, anchor, .. }) = &sent else {
            panic!("a resource attachment: {sent:?}");
        };
        assert_eq!(
            at.reference.span,
            Some(Span::chars(4, 16)),
            "the run inside the item, in the app's own unit"
        );
        assert_eq!(anchor, &Some(Anchor::item("block-2")));

        // Several items travel as those items.
        let rows = SharedSelection::of_items([("row-1", "One"), ("row-2", "Two")])
            .in_resource(page.clone());
        assert_eq!(
            rows.to_attached(Some("so.tilo.tasks"), None),
            Some(Attached::resource(
                ResourceAt {
                    app: "so.tilo.tasks".into(),
                    record: None,
                    reference: ResourceRef::whole("notes.page", "notes.page:1", "Plan"),
                },
                Some(Anchor::items(["row-1", "row-2"])),
                "One +1"
            )),
            "rows read as the first row and how many more, never only the list"
        );

        // Words the app reports no resource for are quoted, alone.
        let words = SharedSelection::of_text("ship on Friday");
        assert_eq!(
            words.to_attached(Some("so.tilo.terminal"), None),
            Some(Attached::Text {
                text: "ship on Friday".into(),
                from: None,
            })
        );

        // Words in a record the app does not place travel with that record,
        // so a chip can still open it.
        let unplaced = SharedSelection::of_text("ship on Friday").in_resource(page);
        assert_eq!(
            unplaced.to_attached(Some("so.tilo.notes"), Some("uuid:7")),
            Some(Attached::Text {
                text: "ship on Friday".into(),
                from: Some(ResourceAt {
                    app: "so.tilo.notes".into(),
                    record: Some("uuid:7".into()),
                    reference: ResourceRef::whole("notes.page", "notes.page:1", "Plan"),
                }),
            })
        );

        // A record nobody says the app of travels as its words: the message
        // still carries what the person read, and points at no app's record.
        let unowned = SharedSelection::of_text("One").in_resource(ActionResource {
            id: "row-1".into(),
            kind: "tasks.issue".into(),
            label: "One".into(),
        });
        assert_eq!(
            unowned.to_attached(None, None),
            Some(Attached::Text {
                text: "One".into(),
                from: None,
            })
        );

        // Nothing selected, nothing to send.
        assert_eq!(
            SharedSelection::default().to_attached(Some("a.b"), None),
            None
        );
    }

    #[test]
    fn where_a_selection_is_drawn_stays_with_the_shell_and_out_of_the_selection() {
        let drawn = vello::kurbo::Rect::new(10.0, 20.0, 300.0, 60.0);
        let selection = SharedSelection::of_text("late results")
            .at(SelectionPlace::range("block-2", 4, 16))
            .within(drawn);
        assert_eq!(selection.protocol_bounds(), Some([10.0, 20.0, 300.0, 60.0]));
        let wire = serde_json::to_value(&selection).unwrap();
        assert!(
            wire.get("bounds").is_none(),
            "geometry is no part of what an agent is told: {wire}"
        );
        let back: SharedSelection = serde_json::from_value(wire).unwrap();
        assert_eq!(back.bounds, None);
        for empty in [
            vello::kurbo::Rect::ZERO,
            vello::kurbo::Rect::new(10.0, 20.0, 10.0, 60.0),
            vello::kurbo::Rect::new(f64::NAN, 0.0, 1.0, 1.0),
        ] {
            assert_eq!(selection.clone().within(empty).protocol_bounds(), None);
        }
    }

    #[test]
    fn the_resource_is_the_published_one_of_the_items_kind() {
        let published = vec![
            tilo_actions::ResourceSpec::new("settings", "app.settings", "Settings"),
            tilo_actions::ResourceSpec::new("doc:1", "notebook", "Notebook").with_child(
                tilo_actions::ResourceSpec::new("page:7", "rich-text", "Monday notes"),
            ),
        ];
        let mut selection = SharedSelection::of_text("x").in_kind("rich-text");
        selection.resolve_resource(&published);
        assert_eq!(
            selection.resource,
            Some(ActionResource {
                id: "page:7".into(),
                kind: "rich-text".into(),
                label: "Monday notes".into(),
            })
        );
        let mut unknown = SharedSelection::of_text("x").in_kind("sheet");
        unknown.resolve_resource(&published);
        assert_eq!(
            unknown.resource.map(|resource| resource.id),
            Some("settings".into())
        );
    }

    /// A focused editor keyed to resource items, describing itself to
    /// assistive technology as `text` with `selected` (bytes) selected.
    struct Editor {
        text: &'static str,
        selected: (usize, usize),
        protected: bool,
    }

    impl crate::widget::Widget for Editor {
        crate::provides!(dyn PublishesPresence);
        crate::testing::inert_widget_methods!(leaf_layout);
        fn name(&self) -> &'static str {
            "editor"
        }
        fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
            let mut node = crate::a11y::AccessNode::new(crate::a11y::Role::TextInput);
            node.value = Some(self.text.into());
            node.text_selection = Some(crate::a11y::AccessibleTextSelection {
                anchor: self.selected.1,
                focus: self.selected.0,
            });
            node.state.protected = self.protected;
            Some(node)
        }
    }

    impl PublishesPresence for Editor {
        fn presence_payload(&self) -> Option<(PresenceKey, PresencePayload)> {
            Some((
                PresenceKey::Resource {
                    app: "so.tilo.example".into(),
                    id: "para-3".into(),
                    kind: "rich-text".into(),
                },
                PresencePayload::Selection {
                    start: self.selected.0,
                    end: self.selected.1,
                },
            ))
        }
    }

    #[test]
    fn a_focused_editors_selection_is_derived_in_characters_from_what_it_publishes() {
        let editor = Editor {
            text: "Grüße, late results",
            selected: (9, 13),
            protected: false,
        };
        let selection = selection_of(&editor).expect("a selection");
        assert_eq!(
            selection.anchor.place,
            Some(SelectionPlace::range("para-3", 7, 11)),
            "byte offsets 9..13 are characters 7..11 after the umlaut and eszett"
        );
        assert_eq!(selection.anchor.excerpt.as_deref(), Some("late"));
        assert_eq!(selection.resource_kind.as_deref(), Some("rich-text"));
        assert_eq!(
            subtree_selection(&editor).map(|found| found.anchor),
            Some(selection.anchor)
        );
        let secret = Editor {
            protected: true,
            ..editor
        };
        assert_eq!(selection_of(&secret), None);
    }

    fn right_press() -> winit::event::WindowEvent {
        crate::synthetic_input::mouse_input(
            winit::event::ElementState::Pressed,
            winit::event::MouseButton::Right,
        )
    }

    fn left_press() -> winit::event::WindowEvent {
        crate::synthetic_input::mouse_input(
            winit::event::ElementState::Pressed,
            winit::event::MouseButton::Left,
        )
    }

    fn type_into(menu: &mut dyn crate::widget::Widget, text: &str) {
        for ch in text.chars() {
            crate::testing::dispatch(menu, &crate::synthetic_input::char_key(&ch.to_string()));
        }
    }

    #[test]
    fn a_sentence_typed_into_a_context_menu_sends_about_the_selection_it_was_opened_in() {
        let editor = || Editor {
            text: "late results",
            selected: (0, 4),
            protected: false,
        };
        let _ = take_send_to();
        let mut area = crate::widget::ContextMenuArea::new(Box::new(editor()), |_| {
            vec![crate::widget::MenuEntry::new("Copy", || {})]
        });
        set_send_host_available(false);
        crate::widget::context_menu::note_input(&right_press());
        assert!(area.open_at(vec2_point(), false));
        assert_eq!(
            area.menu().items().len(),
            1,
            "no desktop to send from: no row to send with"
        );
        assert!(
            !area.menu().takes_typing(),
            "one row and nothing to send from: nothing typing could do"
        );
        set_send_host_available(true);
        crate::widget::context_menu::note_input(&left_press());
        assert!(area.open_at(vec2_point(), false));
        assert!(
            !area.menu().takes_typing(),
            "a menu a button opened is no context menu"
        );
        crate::widget::context_menu::note_input(&right_press());
        assert!(area.open_at(vec2_point(), false));
        assert_eq!(
            area.menu().shown_rows(),
            &[0],
            "nothing typed: the menu's own rows, and no row to ask"
        );
        type_into(&mut area, "make this a heading");
        assert_eq!(
            area.menu().shown_rows(),
            &[1],
            "no entry answers it: only sending it"
        );
        crate::testing::dispatch(
            &mut area,
            &crate::synthetic_input::named_key(winit::keyboard::NamedKey::Enter),
        );
        let box_ = area.menu().send_to();
        assert!(box_.is_open(), "the box carries the sentence");
        assert_eq!(box_.text(), "make this a heading");
        let about = box_.subjects().first().expect("the selection");
        assert_eq!(about.anchor.excerpt.as_deref(), Some("late"));
        assert_eq!(
            about.anchor.place,
            Some(SelectionPlace::range("para-3", 0, 4))
        );
        set_send_host_available(false);
    }

    #[test]
    fn a_right_click_outside_the_selection_is_not_about_it() {
        let inside = vello::kurbo::Point::new(20.0, 10.0);
        let outside = vello::kurbo::Point::new(400.0, 300.0);
        let selected =
            SharedSelection::of_text("late").within(vello::kurbo::Rect::new(0.0, 0.0, 100.0, 20.0));
        assert_eq!(
            pointed_selection(Some(selected.clone()), Some(inside)),
            Some(selected.clone())
        );
        assert_eq!(
            pointed_selection(Some(selected.clone()), Some(outside)),
            None
        );
        assert_eq!(
            pointed_selection(Some(selected.clone()), None),
            Some(selected),
            "the Menu key is about the selection"
        );
        assert_eq!(
            pointed_selection(Some(SharedSelection::of_text("nowhere")), Some(inside)),
            None,
            "a selection that says not where it is is not pointed inside"
        );
    }

    #[test]
    fn the_reference_resolves_the_window_when_nothing_is_selected() {
        let at = vello::kurbo::Point::new(200.0, 300.0);
        let focused = SharedSelection::of_text("elsewhere")
            .within(vello::kurbo::Rect::new(0.0, 0.0, 50.0, 20.0));
        let (about, bounds) =
            resolve_selection(None, Some(at), Some(focused.clone()), &published());
        let about = about.expect("the window");
        assert_eq!(
            about.resource.as_ref().map(|resource| resource.id.as_str()),
            Some("doc:1")
        );
        assert_eq!(about.anchor.place, Some(SelectionPlace::item("doc:1")));
        assert_eq!(bounds, Some([199.5, 299.5, 200.5, 300.5]));
        let (about, bounds) = resolve_selection(
            None,
            Some(vello::kurbo::Point::new(10.0, 10.0)),
            Some(focused),
            &published(),
        );
        let about = about.expect("the selection");
        assert_eq!(about.anchor.excerpt.as_deref(), Some("elsewhere"));
        assert_eq!(
            about.resource.map(|resource| resource.id),
            Some("doc:1".into())
        );
        assert_eq!(bounds, Some([0.0, 0.0, 50.0, 20.0]));
        // An app that publishes nothing has only its window to point at.
        assert_eq!(
            resolve_selection(None, Some(at), None, &[]),
            (None, Some([199.5, 299.5, 200.5, 300.5]))
        );
    }

    fn published() -> Vec<tilo_actions::ResourceSpec> {
        vec![tilo_actions::ResourceSpec::new(
            "doc:1", "notebook", "Notebook",
        )]
    }

    fn vec2_point() -> vello::kurbo::Point {
        vello::kurbo::Point::new(1.0, 1.0)
    }

    #[test]
    fn send_requests_queue_boundedly() {
        let _ = take_send_to();
        let _ = take_recipient_searches();
        for index in 0..(MAX_PENDING + 4) {
            send_to(SendToRequest {
                to: vec![Recipient::new_agent("Ana")],
                about: vec![SharedSelection::of_text("a")],
                text: format!("note {index}"),
            });
            let _ = search_recipients(&format!("ad{index}"));
        }
        let requests = take_send_to();
        assert_eq!(requests.len(), MAX_PENDING);
        assert_eq!(requests[0].text, "note 0");
        assert!(take_send_to().is_empty());
        assert!(take_recipient_searches().len() <= MAX_PENDING);
    }

    #[test]
    fn a_search_answers_the_box_that_asked() {
        let _ = take_recipient_searches();
        let first = search_recipients("ad");
        let second = search_recipients("ada");
        assert_eq!(
            take_recipient_searches(),
            vec![(first, "ad".to_owned()), (second, "ada".to_owned()),]
        );
        assert_eq!(take_recipients(second), None, "not answered yet");
        answer_recipients(
            second,
            vec![Recipient::found(
                crate::floating_surface::RecipientKind::Person,
                "u_ada",
                "Ada",
            )],
        );
        assert_eq!(take_recipients(second).map(|rows| rows.len()), Some(1));
        assert_eq!(take_recipients(second), None, "taken once");
    }
}
