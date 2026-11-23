//! Widgets that publish where their user is (caret, selection) to peers.

use crate::widget::Widget;
use crate::widget::capability::capability;

/// A widget with a presence identity and, while focused, outbound presence.
pub trait PublishesPresence: Widget {
    /// Stable identity for presence and permission lookup.
    fn presence_key(&self) -> Option<super::PresenceKey> {
        None
    }

    /// Outbound presence (typically caret/selection) broadcast while this
    /// widget holds focus; the host polls the focused widget after each
    /// event and diffs it out to peers.
    fn presence_payload(
        &self,
    ) -> Option<(super::PresenceKey, crate::collaboration::PresencePayload)> {
        None
    }

    /// The draft this widget holds where it stands (a conversation's composer holding one): the
    /// key it is written under and any number that changes as the person types, so a reader can
    /// say "… is typing" while they do ([`super::DraftSaid`]). Never what the draft says.
    fn composing_draft(&self) -> Option<(super::PresenceKey, u64)> {
        None
    }

    /// What the person has selected here while this widget is focused, for
    /// asking an agent about it. `None` derives it from
    /// [`Self::presence_payload`] and the accessibility node (see
    /// [`crate::shared_selection::selection_of`]); a widget whose selection
    /// those cannot describe (one spanning blocks, offsets in another text)
    /// answers itself.
    fn shared_selection(&self) -> Option<crate::shared_selection::SharedSelection> {
        None
    }

    /// Where this widget draws a remote member's caret or selection `range`
    /// at `key`, in surface coordinates, at its own text geometry — the same
    /// rects it paints their caret and band at. Empty when this widget shows
    /// nothing at that key. What marks where that member is in this text, and
    /// what following them brings into view ([`range_locus`]).
    fn presence_range_rects(
        &self,
        theme: &crate::theme::Theme,
        key: &super::PresenceKey,
        range: super::SelectionRange,
    ) -> Vec<vello::kurbo::Rect> {
        let _ = (theme, key, range);
        Vec::new()
    }

    /// What this widget shows of a write landed in it, and how a byte range
    /// of that text stands on its geometry: the neutral hook a presence layer
    /// draws a writer's wash and caret from, without the widget knowing who
    /// wrote. `None` when the widget shows no write there.
    fn written_place<'a>(&'a self, theme: &'a crate::theme::Theme) -> Option<WrittenPlace<'a>> {
        let _ = theme;
        None
    }
}

/// What a widget shows of a write landed in it, for a presence layer to draw
/// where it stands.
pub struct WrittenPlace<'a> {
    /// The field the write names, when it names one; any field otherwise.
    pub region: Option<&'a str>,
    /// The item or block the write landed in.
    pub block: &'a str,
    /// The text the write covers, as this widget shows it.
    pub text: std::borrow::Cow<'a, str>,
    /// When the person last edited the text here; a write drawn before that is put away.
    pub edited: Option<crate::time::Instant>,
    /// What the person here has selected, byte offsets, while the widget holds focus.
    pub selected: Option<(usize, usize)>,
    /// The field's name, where a write under way should say which field it is changing.
    pub field: Option<&'a str>,
    /// The bands a byte range of [`Self::text`] covers and the caret at its end, where it wears
    /// one, on this widget's geometry.
    pub place: Box<
        dyn FnMut(
                &mut crate::text_render::TextRenderer,
                &tilo_actions::SemanticInteraction,
                usize,
                usize,
            ) -> (Vec<vello::kurbo::Rect>, Option<super::LabeledCaret>)
            + 'a,
    >;
}

/// `widget`'s written place, when it shows one.
pub fn written_place<'a>(
    widget: &'a dyn Widget,
    theme: &'a crate::theme::Theme,
) -> Option<WrittenPlace<'a>> {
    capability::<dyn PublishesPresence>(widget)?.written_place(theme)
}

/// `widget`'s presence identity.
pub fn presence_key(widget: &(impl Widget + ?Sized)) -> Option<super::PresenceKey> {
    capability::<dyn PublishesPresence>(widget)?.presence_key()
}

/// The draft `widget` holds where it stands, when it composes a message there.
pub fn composing_draft(widget: &(impl Widget + ?Sized)) -> Option<(super::PresenceKey, u64)> {
    capability::<dyn PublishesPresence>(widget)?.composing_draft()
}

/// `widget`'s outbound presence.
pub fn presence_payload(
    widget: &(impl Widget + ?Sized),
) -> Option<(super::PresenceKey, crate::collaboration::PresencePayload)> {
    capability::<dyn PublishesPresence>(widget)?.presence_payload()
}

/// Nodes [`subtree_presence`] visits at most.
const SUBTREE_NODES: usize = 4096;
/// Payloads [`subtree_presence`] returns at most.
const SUBTREE_PAYLOADS: usize = 8;

/// The widget under `root` (itself included) that shows the remote caret or
/// selection `range` at `key`, and the box it draws it in: what a view
/// following that member reveals, and where their cursor rests.
pub fn range_locus(
    root: &dyn Widget,
    theme: &crate::theme::Theme,
    key: &super::PresenceKey,
    range: super::SelectionRange,
) -> Option<(*const dyn Widget, vello::kurbo::Rect)> {
    fn walk(
        widget: &dyn Widget,
        theme: &crate::theme::Theme,
        key: &super::PresenceKey,
        range: super::SelectionRange,
        visited: &mut usize,
    ) -> Option<(*const dyn Widget, vello::kurbo::Rect)> {
        if *visited >= SUBTREE_NODES {
            return None;
        }
        *visited += 1;
        if let Some(presence) = capability::<dyn PublishesPresence>(widget)
            && let Some(rect) = presence
                .presence_range_rects(theme, key, range)
                .into_iter()
                .filter(|rect| rect.width() > 0.0 && rect.height() > 0.0)
                .reduce(|all, rect| all.union(rect))
        {
            return Some((std::ptr::from_ref(presence as &dyn Widget), rect));
        }
        let mut found = None;
        widget.for_each_child(&mut |child| {
            if found.is_none() {
                found = walk(child, theme, key, range, visited);
            }
        });
        found
    }
    walk(root, theme, key, range, &mut 0)
}

/// The caret or selection `presence` holds for `actor`, with the key it is
/// shown at: where that member says it is in text here. A member has at most
/// one of the two, whichever it said last.
pub fn actor_text_range<'p>(
    presence: &'p super::PresenceCtx,
    user: &crate::permissions::MemberId,
) -> Option<(&'p super::PresenceKey, super::SelectionRange)> {
    presence
        .selections
        .iter()
        .find_map(|(key, held)| {
            held.iter()
                .find(|(who, _)| who == user)
                .map(|(_, range)| (key, *range))
        })
        .or_else(|| {
            presence.carets.iter().find_map(|(key, held)| {
                held.iter().find(|(who, _)| who == user).map(|(_, pos)| {
                    (
                        key,
                        super::SelectionRange {
                            start: *pos,
                            end: *pos,
                        },
                    )
                })
            })
        })
}

/// Where the frame with `root` draws each agent among `interactions` in its
/// text, by invocation: the box its caret or selection is in rather than the
/// control its action is bound to.
pub fn agent_text_loci(
    root: &dyn Widget,
    theme: &crate::theme::Theme,
    presence: &super::PresenceCtx,
    interactions: &[tilo_actions::SemanticInteraction],
) -> Vec<(tilo_actions::InvocationId, vello::kurbo::Rect)> {
    interactions
        .iter()
        .filter(|interaction| !interaction.actor.is_person())
        .filter_map(|interaction| {
            let (key, range) = actor_text_range(presence, interaction.actor.member_id()?)?;
            let (_, rect) = range_locus(root, theme, key, range)?;
            Some((interaction.invocation, rect))
        })
        .collect()
}

/// The outbound presence of the widgets under `root` (itself included), in
/// tree order: where each editor's caret is even while it does not hold
/// keyboard focus (an action moved it, or focus is elsewhere). Bounded in
/// nodes and results.
pub fn subtree_presence(
    root: &dyn Widget,
) -> Vec<(super::PresenceKey, crate::collaboration::PresencePayload)> {
    fn walk(
        widget: &dyn Widget,
        visited: &mut usize,
        found: &mut Vec<(super::PresenceKey, crate::collaboration::PresencePayload)>,
    ) {
        if *visited >= SUBTREE_NODES || found.len() >= SUBTREE_PAYLOADS {
            return;
        }
        *visited += 1;
        if let Some(payload) = presence_payload(widget) {
            found.push(payload);
        }
        widget.for_each_child(&mut |child| walk(child, visited, found));
    }
    let mut found = Vec::new();
    walk(root, &mut 0, &mut found);
    found
}
