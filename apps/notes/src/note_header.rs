//! The note header: the tags, where the note sits and when it was written,
//! and the notes that link to it, as one group of toolkit widgets under the
//! title.
//!
//! Tags and incoming links are real buttons: they take focus, activate from
//! the keyboard and assistive technology, and are laid out and painted by the
//! toolkit. A press only records intent; the app drains it and admits it as the
//! same operation a command-palette entry would.

use std::cell::Cell;
use std::rc::Rc;

use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{Button, ButtonRole, Column, ControlSize, Row, Text};

use crate::main_panel::{BacklinkChip, TagChip};
use crate::model::{PageId, TagId};

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
    column: Option<Column>,
    stale: bool,
    tag_open: Rc<Cell<Option<TagId>>>,
    backlink_open: Rc<Cell<Option<PageId>>>,
}

impl NoteHeader {
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

    /// Drain a press of a tag.
    pub fn take_tag_open(&self) -> Option<TagId> {
        self.tag_open.take()
    }

    /// Drain a press of an incoming link.
    pub fn take_backlink_open(&self) -> Option<PageId> {
        self.backlink_open.take()
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
                let open = Rc::clone(&self.tag_open);
                let id = tag.id;
                row = row.push(Box::new(
                    Button::new(&format!("notes.tag-chip:{}", tag.id), &tag.name, None)
                        .control_size(ControlSize::Small)
                        .with_accessible_name(&format!("Tag: {}", tag.name))
                        .tooltip(ui::i18n::I18nManager::format(
                            "notes-surface-tag",
                            &[("tag", tag.name.clone().into())],
                        ))
                        .on_click(move || open.set(Some(id))),
                ));
            }
            top = top.push(Box::new(row));
        }
        for (index, caption) in self.captions.iter().enumerate() {
            top = top.push(Box::new(
                Text::new(caption)
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
                let open = Rc::clone(&self.backlink_open);
                let page = backlink.page;
                row = row.push(Box::new(
                    Button::new(
                        &format!("notes.backlink-chip:{}", backlink.page),
                        &backlink_label(backlink),
                        None,
                    )
                    .control_size(ControlSize::Small)
                    .role(ButtonRole::Secondary)
                    .with_accessible_name(&format!("Linked from: {}", backlink.title))
                    .tooltip(ui::i18n::I18nManager::format(
                        "notes-surface-backlink",
                        &[("title", backlink.title.clone().into())],
                    ))
                    .on_click(move || open.set(Some(page))),
                ));
            }
            links = links.push(Box::new(row));
            group = group.push(Box::new(links));
        }
        group
    }
}
