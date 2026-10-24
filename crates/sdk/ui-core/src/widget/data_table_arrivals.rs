//! A model's rows on their way, in a [`DataTableCore`]: their placeholder
//! timing, and asking again for the ones that failed.

use super::{Arrival, DataTableCore};

impl DataTableCore {
    /// Look at where the visible rows' data stands at `now`: a row on its
    /// way starts its placeholder timing, a landed one shows once due.
    pub(super) fn observe_arrivals(&mut self, now: crate::time::Instant) {
        let Some(model) = self.model.as_deref() else {
            return;
        };
        let (start, end) = self.visible_row_range();
        let mut seen = std::collections::HashSet::new();
        for index in start..end.min(model.len()) {
            let Some(key) = model.key_at(index) else {
                continue;
            };
            self.arrivals
                .observe(&key, &model.row_state(index), Some(self.motion), now);
            seen.insert(key);
        }
        self.arrivals.retain(now, |key| seen.contains(key));
    }

    /// Ask the model again for failed row `index`, when it can be.
    pub(super) fn retry_failed_row(&mut self, index: usize) -> bool {
        let Some(model) = self.model.as_deref_mut() else {
            return false;
        };
        if !matches!(model.row_state(index), Arrival::Failed(_)) || !model.can_retry(index) {
            return false;
        }
        model.retry(index);
        self.visible_window_changed();
        true
    }

    /// What failed row `index` says: why, and that pressing it asks again
    /// when the model can.
    pub(super) fn failure_label(&self, index: usize, message: &str) -> String {
        if self
            .model
            .as_deref()
            .is_some_and(|model| model.can_retry(index))
        {
            format!(
                "{message} \u{00b7} {}",
                crate::i18n::I18nManager::lookup_plain("arriving-retry")
            )
        } else {
            message.to_owned()
        }
    }
}

/// Paint a failed row's `label` (why, and how to ask again) in its `slot`.
pub(super) fn paint_row_failure(
    scene: &mut vello::Scene,
    transform: vello::kurbo::Affine,
    text_renderer: &mut crate::text_render::TextRenderer,
    theme: &crate::theme::Theme,
    label: &str,
    slot: vello::kurbo::Rect,
) {
    let tokens = &theme.tokens;
    let style = tokens.typography.style(crate::tokens::TextRole::Caption);
    let (_, text_h) = text_renderer.measure_style(label, None, &style);
    let top = super::centered_text_top(slot.y0, slot.height(), text_h);
    text_renderer.draw_style(
        scene,
        label,
        tokens.colors.text_primary,
        transform.pre_translate((slot.x0, top).into()),
        None,
        &style,
    );
}
