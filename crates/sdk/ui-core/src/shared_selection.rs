//! What the person has selected in a window, for asking an agent about it.
//!
//! A person who selects a sentence, a range of cells or a row and asks an
//! agent "rewrite this" means that selection. The toolkit derives it once,
//! from the focused widget, so every app gets it without code of its own: the
//! place in the resource's terms (an item and character offsets, the form an
//! action's place argument takes), the resource it is in, and a short
//! excerpt. A widget that knows its selection better (a multi-block editor)
//! says so through [`PublishesPresence::shared_selection`].
//!
//! Asking goes through the desktop: [`ask_agent`] queues the request, and the
//! app runner hands it to the session shell, which opens its agent composer
//! seeded with the selection. Outside a Tilo session nothing can answer, so
//! [`agent_host_available`] is false and no "Ask agent…" entry is offered.
//!
//! [`PublishesPresence::shared_selection`]: crate::presence::capability::PublishesPresence::shared_selection

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use tilo_actions::{ActionResource, SelectionAnchor, SelectionPlace};

use crate::collaboration::PresencePayload;
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

    /// Whether there is nothing to ask about.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.anchor.is_empty()
    }

    /// Name the published resource the selection is in when the widget did
    /// not: the resource of [`Self::resource_kind`], else the first one.
    pub fn resolve_resource(&mut self, published: &[tilo_actions::Resource]) {
        if self.resource.is_some() {
            return;
        }
        fn find<'a>(
            resources: &'a [tilo_actions::Resource],
            kind: &str,
            depth: usize,
        ) -> Option<&'a tilo_actions::Resource> {
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

/// Upper bound on undelivered requests: a repeated chord or menu choice
/// faster than frames are drawn must not queue unbounded composers.
const MAX_PENDING: usize = 4;

thread_local! {
    static PENDING: RefCell<VecDeque<Option<SharedSelection>>> =
        const { RefCell::new(VecDeque::new()) };
    static HOSTED: Cell<bool> = const { Cell::new(false) };
}

/// Ask the desktop's agent about `selection` (or about this window, with
/// nothing selected). The app runner hands the request to the shell.
pub fn ask_agent(selection: Option<SharedSelection>) {
    PENDING.with(|pending| {
        let mut pending = pending.borrow_mut();
        if pending.len() < MAX_PENDING {
            pending.push_back(selection.filter(|selection| !selection.is_empty()));
        }
    });
}

/// Requests made since the last call, oldest first. For the app runner.
pub fn take_agent_requests() -> Vec<Option<SharedSelection>> {
    PENDING.with(|pending| pending.borrow_mut().drain(..).collect())
}

/// Whether a desktop is connected that can hand a request to an agent, so
/// "Ask agent…" is worth offering.
pub fn agent_host_available() -> bool {
    HOSTED.with(Cell::get)
}

/// Record whether a desktop that can answer [`ask_agent`] is connected.
/// For the app runner.
pub fn set_agent_host_available(available: bool) {
    HOSTED.with(|hosted| hosted.set(available));
}

/// Ask the agent about the selection under `root`, for an "Ask agent…"
/// entry that acts on whatever its surface has selected.
pub fn ask_agent_about(root: &dyn Widget) {
    ask_agent(subtree_selection(root));
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
            tilo_actions::Resource::new("settings", "app.settings", "Settings"),
            tilo_actions::Resource::new("doc:1", "notebook", "Notebook").with_child(
                tilo_actions::Resource::new("page:7", "rich-text", "Monday notes"),
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

    #[test]
    fn a_menu_offers_to_ask_only_while_a_desktop_agent_can_answer() {
        let editor = Editor {
            text: "late results",
            selected: (0, 4),
            protected: false,
        };
        let _ = take_agent_requests();
        set_agent_host_available(false);
        assert!(crate::widget::ask_agent_entry(&editor).is_none());
        set_agent_host_available(true);
        let entry = crate::widget::ask_agent_entry(&editor).expect("offered");
        assert_eq!(
            entry.item().label(),
            Some(crate::i18n::I18nManager::lookup_plain("ask-agent-about-selection").as_str())
        );
        let mut area = crate::widget::ContextMenuArea::new(Box::new(editor), |_| {
            vec![crate::widget::MenuEntry::new("Copy", || {})]
        });
        assert!(area.open_at(vec2_point(), true));
        let last = area.menu().items().len() - 1;
        area.choose(last);
        set_agent_host_available(false);
        let asked = take_agent_requests();
        assert_eq!(asked.len(), 1);
        let asked = asked[0].clone().expect("the selection");
        assert_eq!(asked.anchor.excerpt.as_deref(), Some("late"));
        assert_eq!(
            asked.anchor.place,
            Some(SelectionPlace::range("para-3", 0, 4))
        );
    }

    fn vec2_point() -> vello::kurbo::Point {
        vello::kurbo::Point::new(1.0, 1.0)
    }

    #[test]
    fn requests_queue_boundedly_and_drop_empty_selections() {
        let _ = take_agent_requests();
        ask_agent(Some(SharedSelection::default()));
        for _ in 0..10 {
            ask_agent(Some(SharedSelection::of_text("a")));
        }
        let requests = take_agent_requests();
        assert_eq!(requests.len(), MAX_PENDING);
        assert_eq!(requests[0], None);
        assert!(take_agent_requests().is_empty());
    }
}
