//! What a change under a [`BlockEditor`] costs the window above it: nothing.
//!
//! Another document's blocks are laid out again in the stack the editor already holds, and the
//! pictures' controls a document keeps or lets go of are mounted and unmounted under the
//! editor's own node, so the editor reports a change of its own children
//! ([`StructureChange::Children`]) and carries it out in place (`remount_children`) rather than
//! asking the host to compose the window's layout tree again for the note, the task or the
//! picture a person turned to.

use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style};

use super::{BlockEditor, HandleMetrics, ImageAttachment};
use crate::theme::Theme;
use crate::widget::{Remount, StructureChange, Widget};

impl BlockEditor {
    /// What the editor owes the host for what changed under it: a change of its own children
    /// while it holds a stack to carry it out under, and nothing at all for a document whose
    /// stack is merely laid out again.
    pub(super) fn change_of_its_own(&mut self) -> StructureChange {
        // Blocks that came or went may bring or take a picture's control.
        if self.restack {
            self.refresh_image_attachments();
        }
        let own = std::mem::take(&mut self.structure_dirty);
        let mut change = StructureChange::of_children(self.children_mut());
        // With no node laid out the remount cannot carry it out: the host builds its tree, and
        // the build places what the editor holds.
        if own {
            change = change.join(StructureChange::Children);
        }
        change
    }

    /// Carry out the change the editor reported in the retained tree: the stack's leaf stands
    /// and each picture's control the document holds is mounted, while every control a picture
    /// no longer shows — the one a re-sourced picture had, the ones of a block another document
    /// does not have — is let go of. Everything else keeps its node and its layout.
    pub(super) fn remount_own(&mut self, remount: &mut Remount<'_>) -> bool {
        let Some(node) = self.layout_node else {
            return false;
        };
        // A node the tree no longer holds (another widget's change built it again): the whole
        // tree is built again rather than mounted under an id that may name another widget's
        // node by now.
        if remount.taffy().style(node).is_err() {
            return false;
        }
        let unit;
        let theme = if self.content_sized {
            unit = remount.theme().at_unit_text_scale();
            &unit
        } else {
            remount.theme()
        };
        self.text_scale = f64::from(theme.text_scale);
        self.handle = HandleMetrics::from_tokens(&theme.tokens.spacing, &theme.tokens.metrics);
        if self.flush {
            for block in &mut self.blocks {
                block.line.set_flush();
            }
        }
        self.prepare_blocks(theme);
        if self.restack {
            self.refresh_image_attachments();
        }
        let placed: Vec<NodeId> = self
            .image_attachments
            .values()
            .filter_map(ImageAttachment::placed_node)
            .collect();
        let mut children = self.content_node.into_iter().collect::<Vec<_>>();
        let ids: Vec<String> = self.blocks.iter().map(|block| block.id.clone()).collect();
        for id in ids {
            let Some(image) = self.image_attachments.get_mut(&id) else {
                continue;
            };
            let child = if let Some(child) = image.laid_out_node() {
                child
            } else {
                let editing = image.editing;
                let button = image.button_mut();
                let Some(child) = remount.mount_with(node, |taffy, _, map| {
                    Some(button.build_layout(taffy, theme, map))
                }) else {
                    return false;
                };
                let mut style = remount.taffy().style(child).cloned().unwrap_or_default();
                style.position = taffy::Position::Absolute;
                let _ = remount.taffy().set_style(child, style);
                image.node = Some(child);
                image.node_editing = editing;
                child
            };
            children.push(child);
        }
        for gone in placed.into_iter().filter(|gone| !children.contains(gone)) {
            remount.unmount(gone);
        }
        if remount.set_children(node, &children).is_err() {
            return false;
        }
        // The editor's own box carries what its change made of it: its handle lane, and where
        // a document's body starts its text column.
        let own = self.own_style(theme);
        if remount.taffy().style(node).ok() != Some(&own) {
            let _ = remount.taffy().set_style(node, own);
        }
        self.structure_dirty = false;
        self.children_mut()
            .into_iter()
            .all(|child| child.remount_children(remount))
    }

    /// The style of the editor's own node: the block stack's leaf and the pictures' controls in
    /// a column, the drag handles standing in its own leading column so a host leaves no gutter
    /// for them and they are never clipped by the window edge at any text scale.
    ///
    /// A document's body hangs its handle lane outside the text column, so its text starts on
    /// the page's content edge, under the title, and still runs to the column's end, as native
    /// Notes lines a note's title up with its text. It is placed there, not margined there: a
    /// margin would have the layout ask the block stack for heights at a second width than the
    /// one it is applied at, and its blocks then paint taller than the rect they got.
    pub(super) fn own_style(&self, theme: &Theme) -> Style {
        let mut style = Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            align_items: Some(taffy::AlignItems::Stretch),
            gap: taffy::Size {
                width: length(0.0_f32),
                height: length(0.0_f32),
            },
            padding: taffy::Rect {
                left: length(self.handle_column() as f32),
                right: length(0.0_f32),
                top: length(0.0_f32),
                bottom: length(0.0_f32),
            },
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            ..Default::default()
        };
        if self.body.is_some() {
            style.inset.left = length(-self.text_inset(theme) as f32);
        }
        style
    }
}
