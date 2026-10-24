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
    /// Where the state is centred: the tracks' content area.
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
    /// Where playheads and the ruler's grid lines end: across the tracks
    /// while there are clips to cross; on an empty timeline, at the ruler,
    /// clear of the add-track row and the empty state under it.
    pub(super) fn playhead_bottom(&self, r: &Regions) -> f64 {
        if self.project.clips.is_empty() {
            r.ruler.y1
        } else {
            r.timeline.y1
        }
    }

    /// The tracks' content on an empty timeline: the lanes beside the
    /// header column, under the add-track row.
    pub(super) fn timeline_content_area(&self, r: &Regions) -> Rect {
        let top = self
            .add_track_buttons(r)
            .iter()
            .map(|(_, button)| button.y1 + S2)
            .fold(r.lanes.y0, f64::max)
            .min(r.timeline.y1);
        Rect::new(r.lanes.x0, top, r.lanes.x1, r.timeline.y1)
    }

    /// Whether the person has the Media pane shown (or showing), at any
    /// width: one button per job, so while it is, its own Import and its
    /// list answer the empty timeline's words.
    pub(super) fn media_pane_shown(&self) -> bool {
        self.bin_flap.is_open(self.bin_layout())
    }

    /// The empty timeline, while no clip is on it. It carries a button
    /// only while the Media pane, which does the job, is hidden.
    pub(super) fn timeline_empty(&self, r: &Regions) -> Option<TimelineEmpty> {
        if !self.project.clips.is_empty() {
            return None;
        }
        let area = self.timeline_content_area(r);
        let hidden = !self.media_pane_shown();
        Some(if self.project.media.is_empty() {
            TimelineEmpty {
                area,
                message: "Import media, then drag it here.",
                action: hidden.then_some(TimelineEmptyAction::Import),
            }
        } else {
            TimelineEmpty {
                area,
                message: "Drag a clip from the media bin onto a track.",
                action: hidden.then_some(TimelineEmptyAction::ShowMedia),
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
