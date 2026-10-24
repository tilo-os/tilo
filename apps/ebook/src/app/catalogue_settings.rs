//! Whether this device keeps a copy of the book catalogues search reads
//! ([`crate::catalogue`]): the setting's row in the Library pane, and
//! keeping or releasing the copy as it says.

use ui::paint::PaintCtx;

use super::{Action, BooksApp, SettingsColumn, SettingsRow, switch_size};

impl BooksApp {
    /// Whether this device can keep the catalogues at all: a browser keeps
    /// no copy, so it shows no switch.
    fn offers_catalogues(&self) -> bool {
        self.catalogue_setting
            .as_ref()
            .is_some_and(|setting| setting.settings().available())
    }

    /// Whether this device keeps a copy of the book catalogues.
    pub(super) fn keeps_catalogues(&self) -> bool {
        self.catalogue_setting
            .as_ref()
            .map_or_else(crate::catalogue::CatalogueCopies::default, |setting| {
                setting.get()
            })
            .keep_catalogues
    }

    /// Keep or release the catalogue copy as the setting says, off the UI
    /// thread: turned off, the copy is left for the device to evict when it
    /// needs the room, and the store is asked over the network again.
    pub(super) fn follow_catalogue_setting(&mut self) {
        let keep = self.keeps_catalogues();
        let Some(catalogue) = &self.catalogue else {
            return;
        };
        if self.catalogue_kept.replace(keep) == Some(keep) {
            return;
        }
        let catalogue = std::sync::Arc::clone(catalogue);
        if ui::background::Background::current()
            .spawn("books-catalogue", move || catalogue.keep(keep))
            .is_err()
        {
            tracing::debug!("books: nothing runs background work to tell the catalogue copy");
            self.catalogue_kept = None;
        }
    }

    pub(super) fn on_toggle_keep_catalogues(&mut self) {
        let keep = !self.keeps_catalogues();
        if let Some(setting) = &self.catalogue_setting
            && let Err(error) = setting.set(crate::catalogue::CatalogueCopies {
                keep_catalogues: keep,
            })
        {
            tracing::warn!(%error, "books: couldn't keep the catalogue setting");
        }
        self.follow_catalogue_setting();
    }

    /// Whether this device keeps a copy of the book catalogues search
    /// reads.
    pub(super) fn draw_catalogue_settings(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        col: SettingsColumn,
        y: f64,
    ) -> f64 {
        if !self.offers_catalogues() {
            return y;
        }
        let theme = ctx.theme;
        let y = y + f64::from(theme.tokens.spacing.rhythm.section);
        let y = Self::draw_settings_section(ctx, col, y, "Search", None);
        let (switch_w, _) = switch_size(theme);
        let trailing = switch_w + 2.0 * f64::from(theme.tokens.spacing.s3);
        let row = Self::draw_settings_row(
            ctx,
            col.row_span(theme),
            y,
            &SettingsRow {
                title: crate::catalogue::KEEP_CATALOGUES,
                secondary: Some(
                    "Search finds books from Project Gutenberg at once, also when you\u{2019}re offline.",
                ),
                trailing,
                separator: false,
                selected: false,
                renaming: false,
                title_family: None,
            },
        );
        let on = self.keeps_catalogues();
        self.switch_at(
            ctx,
            "keep-catalogues",
            col.x1(),
            row.center().y,
            on,
            Action::ToggleKeepCatalogues,
        );
        row.y1
    }
}
