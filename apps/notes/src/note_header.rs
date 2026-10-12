//! The note header: the tags, where the note sits and when it was written,
//! and the notes that link to it, as one group of toolkit widgets under the
//! title.
//!
//! Tags and incoming links are chips that act: they take focus, activate from
//! the keyboard and assistive technology, and are laid out and painted by the
//! toolkit. A press invokes the effect that browses the tag or opens the
//! note, the same operation a command-palette entry does.

use std::cell::RefCell;
use std::rc::Rc;

use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{Chip, Column, Row, Text, Widget};

use crate::effect::NotesEffect;
use crate::main_panel::{BacklinkChip, TagChip};
use crate::notebook::Notebook;

/// What an incoming-link button says: the source note, and how many times it
/// links here when more than once.
fn backlink_label(backlink: &BacklinkChip) -> String {
    if backlink.references > 1 {
        format!("{} ({})", backlink.title, backlink.references)
    } else {
        backlink.title.clone()
    }
}

/// The header's content and the widgets built from it.
#[derive(Default)]
pub struct NoteHeader {
    tags: Vec<TagChip>,
    backlinks: Vec<BacklinkChip>,
    captions: Vec<String>,
    /// What the built caption lines read when they sync: a line's words can
    /// change (the edit time turning over a minute) without rebuilding the
    /// header's widgets.
    shown_captions: Rc<RefCell<Vec<String>>>,
    column: Option<Column>,
    stale: bool,
    /// What the chips invoke, once the notebook's window is attached.
    actions: Option<ui::framework::Actions<Notebook>>,
}

impl NoteHeader {
    /// Bind the chips to the notebook window's effects from now on.
    pub fn attach(&mut self, actions: ui::framework::Actions<Notebook>) {
        self.actions = Some(actions);
        self.stale = true;
    }

    /// What the chips invoke, when attached.
    pub fn actions(&self) -> Option<ui::framework::Actions<Notebook>> {
        self.actions.clone()
    }

    fn effect(&self, effect: &NotesEffect) -> Option<tilo_actions::ActionBinding<()>> {
        self.actions.as_ref()?.effect(effect)
    }

    /// Show these tags. Returns whether the header changed.
    pub fn set_tags(&mut self, tags: Vec<TagChip>) -> bool {
        let changed = self.tags != tags;
        if changed {
            self.tags = tags;
            self.stale = true;
        }
        changed
    }

    /// Show these incoming links. Returns whether the header changed.
    pub fn set_backlinks(&mut self, backlinks: Vec<BacklinkChip>) -> bool {
        let changed = self.backlinks != backlinks;
        if changed {
            self.backlinks = backlinks;
            self.stale = true;
        }
        changed
    }

    /// Show these caption lines under the tags. Returns whether the header
    /// changed.
    pub fn set_captions(&mut self, captions: Vec<String>) -> bool {
        let changed = self.captions != captions;
        if changed {
            self.captions = captions;
            self.stale = true;
        }
        changed
    }

    /// Show these caption lines in the built widgets when only their words
    /// changed: the same number of lines, in a header that is built and not
    /// about to be rebuilt. They show it at their next [`Self::sync_built`].
    /// Returns whether it did; otherwise use [`Self::set_captions`].
    pub fn retext_captions(&mut self, captions: &[String]) -> bool {
        if self.stale
            || self.column.is_none()
            || captions.is_empty()
            || captions.len() != self.captions.len()
        {
            return false;
        }
        if self.captions != captions {
            self.captions = captions.to_vec();
            self.shown_captions.replace(self.captions.clone());
        }
        true
    }

    /// Sync the built widgets, so retexted captions read their new words.
    pub fn sync_built(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        if let Some(column) = self.column.as_mut() {
            column.sync(state, path);
        }
    }

    /// Whether there is nothing to show: no tags, no captions, no links.
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty() && self.backlinks.is_empty() && self.captions.is_empty()
    }

    /// The widgets, rebuilt first when the content changed since the last
    /// layout pass. `None` when there is nothing to show.
    pub fn column(&mut self, theme: &Theme) -> Option<&mut Column> {
        if self.is_empty() {
            self.column = None;
            self.stale = false;
            return None;
        }
        if self.stale || self.column.is_none() {
            self.column = Some(self.build(theme));
            self.stale = false;
        }
        self.column.as_mut()
    }

    /// The built widgets, as of the last layout pass.
    pub fn built(&self) -> Option<&Column> {
        self.column.as_ref()
    }

    /// The built widgets, mutably, as of the last layout pass.
    pub fn built_mut(&mut self) -> Option<&mut Column> {
        self.column.as_mut()
    }

    fn build(&self, theme: &Theme) -> Column {
        let rhythm = theme.tokens.spacing.rhythm;
        let mut group = Column::new()
            .with_intrinsic_height()
            .with_no_padding()
            .with_gap(rhythm.sibling);
        let mut top = Column::new()
            .with_intrinsic_height()
            .with_no_padding()
            .with_gap(rhythm.related);
        if !self.tags.is_empty() {
            let mut row = Row::new().with_gap(rhythm.related);
            for tag in &self.tags {
                let mut chip = Chip::new(&tag.name)
                    .with_id(format!("notes.tag-chip:{}", tag.id))
                    .with_accessible_name(format!("Tag: {}", tag.name))
                    .tooltip(ui::i18n::I18nManager::format(
                        "notes-surface-tag",
                        &[("tag", tag.name.clone().into())],
                    ));
                if let Some(open) = self.effect(&NotesEffect::OpenTag { tag: tag.id }) {
                    chip = chip.on_action(open);
                }
                row = row.push(Box::new(chip));
            }
            top = top.push(Box::new(row));
        }
        self.shown_captions.replace(self.captions.clone());
        for index in 0..self.captions.len() {
            let shown = Rc::clone(&self.shown_captions);
            top = top.push(Box::new(
                Text::dynamic(move || shown.borrow().get(index).cloned().unwrap_or_default())
                    .with_id(format!("notes.caption:{index}"))
                    .with_role(TextRole::Caption)
                    .dimmed(),
            ));
        }
        group = group.push(Box::new(top));
        if !self.backlinks.is_empty() {
            let mut links = Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .with_gap(rhythm.related);
            links = links.push(Box::new(
                Text::new(&ui::i18n!("notes-linked-from"))
                    .with_id("notes.linked-from")
                    .with_role(TextRole::Subheading)
                    .dimmed(),
            ));
            let mut row = Row::new().with_gap(rhythm.related);
            for backlink in &self.backlinks {
                let mut chip = Chip::new(backlink_label(backlink))
                    .with_id(format!("notes.backlink-chip:{}", backlink.page))
                    .with_accessible_name(format!("Linked from: {}", backlink.title))
                    .tooltip(ui::i18n::I18nManager::format(
                        "notes-surface-backlink",
                        &[("title", backlink.title.clone().into())],
                    ));
                if let Some(open) = self.effect(&NotesEffect::OpenNote {
                    page: backlink.page,
                    block: None,
                }) {
                    chip = chip.on_action(open);
                }
                row = row.push(Box::new(chip));
            }
            links = links.push(Box::new(row));
            group = group.push(Box::new(links));
        }
        group
    }
}
