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
use ui::widget::{Button, ButtonRole, ControlSize, Element, ModelView, ToggleGroup};

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

    fn request(&self, command: Command) -> impl FnMut() + 'static {
        let commands = Rc::clone(&self.commands);
        move || commands.borrow_mut().push_back(command)
    }

    /// The header's actions, in the order they read: change navigation,
    /// then the file actions with the one primary action last.
    pub(crate) fn actions(&self) -> Vec<Element> {
        [
            Command::Mode(Mode::Diff),
            Command::Previous,
            Command::Next,
            Command::LoadSample,
            Command::Open,
        ]
        .into_iter()
        .map(|command| {
            let chrome = self.clone();
            Box::new(ModelView::from_state(&self.header, move |header| {
                chrome.control(*header, command)
            })) as Element
        })
        .collect()
    }

    fn mode(&self, header: Header) -> Element {
        let merge_label = if header.compact {
            t("toolbar-merge-short")
        } else {
            t("toolbar-merge")
        };
        Box::new(
            ToggleGroup::new(
                vec![Segment::new(t("toolbar-diff")), Segment::new(merge_label)],
                usize::from(header.mode == Mode::Merge),
            )
            .with_style(SegmentedStyle::Segmented)
            .control_size(if header.compact {
                ControlSize::Large
            } else {
                ControlSize::Small
            })
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
            }),
        )
    }

    fn control(&self, header: Header, command: Command) -> Element {
        let size = if header.compact {
            ControlSize::Large
        } else {
            ControlSize::Small
        };
        let button = match command {
            Command::Previous | Command::Next => {
                let (id, icon, name) = if command == Command::Previous {
                    (
                        "diffmerge.previous-change",
                        Icon::ChevronUp,
                        t("toolbar-previous"),
                    )
                } else {
                    (
                        "diffmerge.next-change",
                        Icon::ChevronDown,
                        t("toolbar-next"),
                    )
                };
                Button::icon_only(id, icon, &name)
                    .with_enabled(header.changes)
                    .on_view(self.request(command))
            }
            Command::LoadSample => {
                let label = t(if header.compact {
                    "toolbar-sample-short"
                } else {
                    "toolbar-sample"
                });
                Button::new("diffmerge.load-sample", &label).on_effect(self.request(command))
            }
            Command::Open => {
                let button = if header.compact {
                    Button::icon_only("diffmerge.open-files", Icon::FolderOpen, &t("toolbar-open"))
                } else {
                    Button::new("diffmerge.open-files", &t("toolbar-open"))
                };
                button
                    .role(ButtonRole::Primary)
                    .on_effect(self.request(command))
            }
            Command::Mode(_) => return self.mode(header),
        };
        Box::new(button.control_size(size))
    }
}
