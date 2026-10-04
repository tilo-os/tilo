//! The window's header controls and what they share with the comparison.
//!
//! The mode switch, change navigation and the file actions sit in the content
//! column's header, which the shell draws. They are ordinary shared controls;
//! each one queues a [`Command`] that the comparison drains on its next sync,
//! and the comparison publishes the [`Header`] state the controls show.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use ui::LocalState;
use ui::icons::Icon;
use ui::widget::toggle_group::{Segment, SegmentedStyle};
use ui::widget::{Button, ButtonRole, ControlSize, Element, ModelView, Row, ToggleGroup};

use crate::app::Mode;
use crate::i18n::t;

/// What a header control asks of the comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Mode(Mode),
    Previous,
    Next,
    Open,
    LoadSample,
}

/// What the header controls show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Header {
    pub mode: Mode,
    /// A comparison with changes is loaded, so there is somewhere to go.
    pub changes: bool,
    /// The window is too narrow for the controls' labels: they drop to short
    /// labels and icons.
    pub compact: bool,
}

/// The state the header and the comparison share. Clones refer to the same
/// state.
#[derive(Clone)]
pub(crate) struct Chrome {
    header: LocalState<Header>,
    commands: Rc<RefCell<VecDeque<Command>>>,
}

impl Chrome {
    pub(crate) fn new() -> Self {
        Self {
            header: LocalState::new(Header {
                mode: Mode::Diff,
                changes: false,
                compact: false,
            }),
            commands: Rc::default(),
        }
    }

    /// The next control request, oldest first.
    pub(crate) fn take(&self) -> Option<Command> {
        self.commands.borrow_mut().pop_front()
    }

    /// Show `header` on the controls; nothing happens when it is unchanged.
    pub(crate) fn publish(&self, header: Header) {
        if self.header.get() != header {
            self.header.set(header);
        }
    }

    /// What the controls show now.
    #[cfg(test)]
    pub(crate) fn header(&self) -> Header {
        self.header.get()
    }

    /// Queue `command` as a control would.
    #[cfg(test)]
    pub(crate) fn push(&self, command: Command) {
        self.commands.borrow_mut().push_back(command);
    }

    fn request(&self, command: Command) -> impl FnMut() + 'static {
        let commands = Rc::clone(&self.commands);
        move || commands.borrow_mut().push_back(command)
    }

    /// The header's controls, in the order they read: the mode switch, change
    /// navigation, then the file actions with the one primary action last.
    pub(crate) fn actions(&self) -> Vec<Element> {
        let chrome = self.clone();
        vec![Box::new(ModelView::from_state(
            &self.header,
            move |header| chrome.controls(*header),
        ))]
    }

    fn controls(&self, header: Header) -> Element {
        let compact = header.compact;
        let merge_label = if compact {
            t("toolbar-merge-short")
        } else {
            t("toolbar-merge")
        };
        let mode = ToggleGroup::new(
            vec![Segment::new(t("toolbar-diff")), Segment::new(merge_label)],
            usize::from(header.mode == Mode::Merge),
        )
        .with_style(SegmentedStyle::Segmented)
        .control_size(ControlSize::Small)
        .on_view("diffmerge.mode", {
            let mut diff = self.request(Command::Mode(Mode::Diff));
            let mut merge = self.request(Command::Mode(Mode::Merge));
            move |index| {
                if index == 0 {
                    diff();
                } else {
                    merge();
                }
            }
        });
        let step = |id: &str, icon: Icon, name: String, command: Command| {
            Button::icon_only(id, icon, &name)
                .control_size(ControlSize::Small)
                .with_enabled(header.changes)
                .on_view(self.request(command))
        };
        let sample_label = if compact {
            t("toolbar-sample-short")
        } else {
            t("toolbar-sample")
        };
        let sample = Button::new("diffmerge.load-sample", &sample_label, None)
            .control_size(ControlSize::Small)
            .on_effect(self.request(Command::LoadSample));
        // The one primary action: a labelled button, or its icon when the
        // window is narrow.
        let open = if compact {
            Button::icon_only("diffmerge.open-files", Icon::FolderOpen, &t("toolbar-open"))
        } else {
            Button::new("diffmerge.open-files", &t("toolbar-open"), None)
        }
        .role(ButtonRole::Primary)
        .control_size(ControlSize::Small)
        .on_effect(self.request(Command::Open));
        // The window then draws no History bar of its own.
        let mut history = ui::widget::history_button().control_size(ControlSize::Small);
        history.set_compact_toolbar_presentation(compact);
        Box::new(
            Row::new()
                .no_wrap()
                .push(Box::new(mode))
                .push(Box::new(step(
                    "diffmerge.previous-change",
                    Icon::ChevronUp,
                    t("toolbar-previous"),
                    Command::Previous,
                )))
                .push(Box::new(step(
                    "diffmerge.next-change",
                    Icon::ChevronDown,
                    t("toolbar-next"),
                    Command::Next,
                )))
                .push(Box::new(sample))
                .push(Box::new(open))
                .push(Box::new(history)),
        )
    }
}
