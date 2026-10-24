//! The empty timeline: what it says, and the one action that answers it.

#[allow(
    clippy::wildcard_imports,
    reason = "this file is one part of the `VideoEditorApp` impl split out of `app.rs`; it shares the parent's whole scope"
)]
use super::*;

/// The empty timeline's node in the semantic tree, and its button's.
pub(super) const TIMELINE_EMPTY_KEY: &str = "timeline empty";
pub(super) const TIMELINE_EMPTY_ACTION_KEY: &str = "timeline empty action";

/// What the empty timeline offers: the step its words ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TimelineEmptyAction {
    /// Nothing imported yet: the Media pane's Import.
    Import,
    /// Media to drag, but the Media pane is folded away: show it.
    ShowMedia,
}

impl TimelineEmptyAction {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Import => "Import media",
            Self::ShowMedia => "Show media",
        }
    }
}

/// The empty timeline as laid out for one frame.
pub(super) struct TimelineEmpty {
    /// Where the state is centred: the lanes, down to the timeline's end.
    pub(super) area: Rect,
    pub(super) message: &'static str,
    pub(super) action: Option<TimelineEmptyAction>,
}

impl TimelineEmpty {
    pub(super) const TITLE: &'static str = "Nothing on the timeline";

    pub(super) fn content(&self) -> ui::widget::EmptyStateContent<'static> {
        ui::widget::EmptyStateContent {
            icon: Some(Icon::Layers),
            message: Some(self.message),
            action_label: self.action.map(TimelineEmptyAction::label),
            ..ui::widget::EmptyStateContent::new(Self::TITLE)
        }
    }
}

impl VideoEditorApp {
    /// The empty timeline, while no clip is on it.
    pub(super) fn timeline_empty(&self, r: &Regions) -> Option<TimelineEmpty> {
        if !self.project.clips.is_empty() {
            return None;
        }
        let area = Rect::new(r.lanes.x0, r.lanes.y0, r.lanes.x1, r.timeline.y1);
        Some(if self.project.media.is_empty() {
            TimelineEmpty {
                area,
                message: "Import media, then drag it here.",
                action: Some(TimelineEmptyAction::Import),
            }
        } else {
            TimelineEmpty {
                area,
                message: "Drag a clip from the media bin onto a track.",
                action: (!self.bin_is_visible(r)).then_some(TimelineEmptyAction::ShowMedia),
            }
        })
    }

    /// Where the empty timeline's action is drawn, and what it does.
    pub(super) fn timeline_empty_action_rect(
        &self,
        r: &Regions,
    ) -> Option<(Rect, TimelineEmptyAction)> {
        let empty = self.timeline_empty(r)?;
        let action = empty.action?;
        let rect = self.with_dialog_layout(|_, tr, theme| {
            ui::widget::EmptyState::action_rect_in(tr, theme, empty.area, &empty.content())
        })?;
        Some((rect, action))
    }

    /// Run the empty timeline's action: the same Import the Media pane's
    /// button runs, or the Media pane shown.
    pub(super) fn run_timeline_empty_action(&mut self, action: TimelineEmptyAction) {
        match action {
            TimelineEmptyAction::Import => self.trigger_import(),
            TimelineEmptyAction::ShowMedia => {
                if !self.bin_flap.is_open(self.bin_layout()) {
                    self.toggle_bin();
                }
            }
        }
    }

    /// The empty timeline in the semantic tree: its words, and its action
    /// as a button where it is drawn.
    pub(super) fn a11y_timeline_empty(&self, r: &Regions) -> Option<AccessNode> {
        let empty = self.timeline_empty(r)?;
        let mut node = AccessNode::new(Role::Group)
            .with_name(TimelineEmpty::TITLE)
            .with_description(empty.message)
            .with_bounds(empty.area)
            .with_key(ui::a11y::hash_key(&TIMELINE_EMPTY_KEY));
        if let Some((rect, action)) = self.timeline_empty_action_rect(r) {
            node = node.with_children(vec![
                AccessNode::new(Role::Button)
                    .with_name(action.label())
                    .with_bounds(rect)
                    .with_key(ui::a11y::hash_key(&TIMELINE_EMPTY_ACTION_KEY))
                    .with_state(AccessState::new().focusable(true)),
            ]);
        }
        Some(node)
    }

    /// An assistive technology's action on the empty timeline (`rest`, the
    /// path under its node).
    pub(super) fn a11y_timeline_empty_action(
        &mut self,
        action: &ui::a11y::Action,
        rest: &[u64],
    ) -> bool {
        use ui::a11y::Action;
        match (rest.first(), action) {
            (None, Action::Focus) => true,
            (Some(&key), Action::Focus) => key == ui::a11y::hash_key(&TIMELINE_EMPTY_ACTION_KEY),
            (Some(&key), Action::Activate)
                if key == ui::a11y::hash_key(&TIMELINE_EMPTY_ACTION_KEY) =>
            {
                let r = self.regions();
                let Some((_, run)) = self.timeline_empty_action_rect(&r) else {
                    return false;
                };
                self.run_timeline_empty_action(run);
                true
            }
            _ => false,
        }
    }
}
