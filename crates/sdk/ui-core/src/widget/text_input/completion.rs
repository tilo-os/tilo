//! A field's inline completion: what it offers, the drawer at its caret, and what a pick
//! writes ([`crate::widget::text_completion`] and [`crate::widget::completion`]).

use vello::Scene;
use vello::kurbo::Affine;

use super::{MentionCandidate, TextCompletion, TextInput};
use crate::color_compat::ColorCompat as _;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

impl TextInput {
    #[must_use]
    pub fn completion(&self) -> TextCompletion {
        if self.holds_secret() || self.kind.refuses_completion() {
            return TextCompletion::PLAIN;
        }
        self.completion_setting
            .unwrap_or_else(|| self.kind.default_completion())
    }

    #[must_use]
    pub fn with_completion(mut self, completion: TextCompletion) -> Self {
        self.set_completion(completion);
        self
    }

    pub fn set_completion(&mut self, completion: TextCompletion) {
        self.completion_setting = Some(completion);
        self.refresh_completion();
    }

    #[must_use]
    pub fn with_mention_candidates(mut self, candidates: Vec<MentionCandidate>) -> Self {
        self.mention_candidates = candidates;
        self
    }

    pub fn set_mention_candidates(&mut self, candidates: Vec<MentionCandidate>) {
        self.mention_candidates = candidates;
        self.refresh_completion();
    }

    #[must_use]
    pub fn is_completion_open(&self) -> bool {
        self.completion.is_open()
    }

    pub(super) fn completion_roster(&self) -> Vec<MentionCandidate> {
        let mut roster = self.mention_roster.clone();
        for extra in &self.mention_candidates {
            if !roster.iter().any(|known| known.id == extra.id) {
                roster.push(extra.clone());
            }
        }
        roster.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        });
        roster
    }

    /// Open, narrow or close the candidate list from the caret, unless the
    /// field is not focused, is masked, is composing with an input method,
    /// or offers nothing.
    pub(super) fn refresh_completion(&mut self) {
        if !self.is_focused
            || self.effectively_disabled()
            || self.read_only
            || self.is_composing()
            || self.completion().is_plain()
        {
            self.completion.close();
            return;
        }
        let setting = self.completion();
        let roster = self.completion_roster();
        self.completion
            .follow(&self.text, self.core.caret(), &roster, setting);
    }

    pub fn set_completion_sources(
        &mut self,
        sources: Vec<crate::widget::completion::CompletionSource>,
    ) {
        if self.completion.set_sources(sources) {
            self.refresh_completion();
        }
    }

    pub fn set_on_completion_create(&mut self, create: impl FnMut(char, &str) + 'static) {
        self.on_completion_create = Some(Box::new(create));
    }

    pub fn set_on_command(
        &mut self,
        start: impl FnMut(&crate::widget::completion::Candidate) + 'static,
    ) {
        self.on_command = Some(Box::new(start));
    }

    #[must_use]
    pub fn pending_token(&self) -> Option<std::ops::Range<usize>> {
        self.completion
            .token()
            .map(|token| token.start..token.end.min(self.text.len()))
    }

    pub(super) fn accept_completion(&mut self, pick: crate::widget::completion::Pick) -> bool {
        use crate::widget::completion::Pick;
        let Some(token) = self.completion.token().cloned() else {
            return false;
        };
        let end = token.end.min(self.text.len());
        let Some(expected) = self.text.get(token.start..end) else {
            return false;
        };
        let expected = expected.to_owned();
        // The accepted form is no longer a token being typed: a mention's
        // `@Name` must not offer itself again, nor an emoji's shortcode.
        self.completion.dismiss();
        let candidate = match pick {
            Pick::Insert(candidate) => candidate,
            Pick::Command(candidate) => {
                if let Some(start) = self.on_command.as_mut() {
                    start(&candidate);
                }
                return true;
            }
            Pick::Create { trigger, query } => {
                if let Some(create) = self.on_completion_create.as_mut() {
                    create(trigger, &query);
                }
                return true;
            }
        };
        let spaced = token.trigger != ':'
            && !self
                .text
                .get(end..)
                .is_some_and(|rest| rest.starts_with(char::is_whitespace));
        let value = if spaced {
            format!("{} ", candidate.insert)
        } else {
            candidate.insert.clone()
        };
        if !self.replace_range(token.start..end, &expected, &value) {
            return false;
        }
        self.set_cursor_pos(token.start.saturating_add(value.len()));
        if token.trigger == ':' {
            crate::emoji_catalogue::record_recent(&candidate.insert);
        }
        if let Some(inserted) = self.on_reference_inserted.as_mut()
            && let Some(reference) = &candidate.reference
        {
            inserted(
                token.trigger,
                token.start..token.start.saturating_add(candidate.insert.len()),
                reference,
            );
        }
        true
    }

    pub fn set_on_reference_inserted(
        &mut self,
        inserted: impl FnMut(char, std::ops::Range<usize>, &str) + 'static,
    ) {
        self.on_reference_inserted = Some(Box::new(inserted));
    }

    pub(super) fn draw_pending_token(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let Some(range) = self.pending_token() else {
            return;
        };
        let tint = theme
            .tokens
            .colors
            .intent
            .accent_ink
            .with_alpha_factor(0.14);
        let radius = f64::from(theme.tokens.radii.item).min(4.0);
        for rect in self.selection_rects(theme, range.start, range.end) {
            let rect = rect.inflate(1.0, 0.0);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                tint,
                None,
                &vello::kurbo::RoundedRect::from_rect(rect, radius),
            );
        }
    }

    pub(super) fn paint_completion(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        if !self.completion.is_open() {
            return;
        }
        let caret = self.caret_rect(theme, self.core.caret());
        self.completion.paint(
            scene,
            transform,
            text,
            theme,
            caret,
            self.rect,
            presence.viewport.get(),
        );
    }
}
