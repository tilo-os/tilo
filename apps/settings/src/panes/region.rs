//! Region & Formats pane — the editor for the **shared region store**.
//!
//! This panel edits [`crate::region::RegionSettings`], the global per-user
//! locale document persisted at
//! [`RegionSettings::store_path`] (`~/.config/tilo/settings.json`). The
//! Sheets app reads `locale.currency` / `locale.date_format` /
//! `locale.time_format` for its cell formatting, and the Weather app reads
//! `locale.measurement` for its °C/°F + km/h vs mph toggle. Editing here is
//! the single source of truth those apps follow.
//!
//! Each control is a `Dropdown` bound to a CRDT key; `sync` reads the key,
//! maps the selected string back to the typed enum, and persists the store.
//! On first sync it seeds the dropdowns from the loaded store so they rest
//! on the user's current preference (or the sample default under `--shots`).

use std::collections::HashMap;
use ui::doc::read_str;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId, transaction::Transactable};
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Dropdown, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::actions::{SETTINGS_ACTION_TYPE, SettingsAction, set_text_action};
use crate::i18n::t;
use crate::region::{
    Currency, DateStyle, FirstDayOfWeek, Measurement, NumberStyle, RegionSettings, TimeStyle,
};
use crate::widgets::{card, choice_row, rows};

fn date_label(style: DateStyle) -> String {
    t(match style {
        DateStyle::Iso => "settings-region-date-iso",
        DateStyle::Us => "settings-region-date-us",
        DateStyle::Eu => "settings-region-date-eu",
        DateStyle::Long => "settings-region-date-long",
    })
}

fn date_from_shown(label: &str) -> Option<DateStyle> {
    DateStyle::ALL
        .iter()
        .copied()
        .find(|style| style.label() == label || date_label(*style) == label)
}

fn time_label(style: TimeStyle) -> String {
    t(match style {
        TimeStyle::H24 => "settings-region-time-24",
        TimeStyle::H12 => "settings-region-time-12",
    })
}

fn time_from_shown(label: &str) -> Option<TimeStyle> {
    TimeStyle::ALL
        .iter()
        .copied()
        .find(|style| style.label() == label || time_label(*style) == label)
}

fn measure_label(measure: Measurement) -> String {
    t(match measure {
        Measurement::Metric => "settings-region-measure-metric",
        Measurement::Imperial => "settings-region-measure-imperial",
    })
}

fn measure_from_shown(label: &str) -> Option<Measurement> {
    Measurement::ALL
        .iter()
        .copied()
        .find(|measure| measure.label() == label || measure_label(*measure) == label)
}

fn weekday_label(day: FirstDayOfWeek) -> String {
    t(match day {
        FirstDayOfWeek::Monday => "settings-region-weekday-monday",
        FirstDayOfWeek::Sunday => "settings-region-weekday-sunday",
        FirstDayOfWeek::Saturday => "settings-region-weekday-saturday",
    })
}

fn weekday_from_shown(label: &str) -> Option<FirstDayOfWeek> {
    FirstDayOfWeek::ALL
        .iter()
        .copied()
        .find(|day| day.label() == label || weekday_label(*day) == label)
}

pub const CURRENCY_KEY: &str = "region.currency";
pub const DATE_KEY: &str = "region.date_format";
pub const TIME_KEY: &str = "region.time_format";
pub const MEASUREMENT_KEY: &str = "region.measurement";
pub const NUMBER_STYLE_KEY: &str = "region.number_style";
pub const FIRST_DAY_KEY: &str = "region.first_day";

pub struct RegionPane {
    settings: RegionSettings,
    needs_seed: bool,
    commit_context_pending: bool,
    actions: crate::resource::ActionSink,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl Default for RegionPane {
    fn default() -> Self {
        Self::new()
    }
}

impl RegionPane {
    pub fn new() -> Self {
        Self::with_settings(RegionSettings::load())
    }

    /// Construct from explicit settings — used by tests and the
    /// deterministic `--shots` render.
    pub fn with_settings(settings: RegionSettings) -> Self {
        Self {
            settings,
            needs_seed: true,
            commit_context_pending: false,
            actions: crate::resource::ActionSink::default(),
            composed: None,
            rect: Rect::ZERO,
        }
    }

    pub fn settings(&self) -> RegionSettings {
        self.settings
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let choice = choice_row;
        let loc = self.settings.locale;

        // Currency dropdown — options are ISO codes (the stored form).
        let currency_opts: Vec<String> =
            Currency::ALL.iter().map(|c| c.code().to_string()).collect();
        let currency = Dropdown::new(currency_opts, "region.currency.edit")
            .with_id("region.currency")
            .with_placeholder(loc.currency.code());

        let date_current = date_label(loc.date_format);
        let date_opts: Vec<String> = DateStyle::ALL.iter().copied().map(date_label).collect();
        let date = Dropdown::new(date_opts, "region.date_format.edit")
            .with_id("region.date")
            .with_placeholder(&date_current);

        let time_current = time_label(loc.time_format);
        let time_opts: Vec<String> = TimeStyle::ALL.iter().copied().map(time_label).collect();
        let time = Dropdown::new(time_opts, "region.time_format.edit")
            .with_id("region.time")
            .with_placeholder(&time_current);

        let measure_current = measure_label(loc.measurement);
        let unit_opts: Vec<String> = Measurement::ALL
            .iter()
            .copied()
            .map(measure_label)
            .collect();
        let units = Dropdown::new(unit_opts, "region.measurement.edit")
            .with_id("region.measurement")
            .with_placeholder(&measure_current);

        let number_style = Dropdown::new(
            NumberStyle::ALL
                .iter()
                .map(|style| style.label().to_string())
                .collect(),
            "region.number_style.edit",
        )
        .with_id("region.number_style")
        .with_placeholder(loc.number_style.label());

        let weekday_current = weekday_label(loc.first_day_of_week);
        let first_day = Dropdown::new(
            FirstDayOfWeek::ALL
                .iter()
                .copied()
                .map(weekday_label)
                .collect(),
            "region.first_day.edit",
        )
        .with_id("region.first_day")
        .with_placeholder(&weekday_current);

        let currency_title = t("settings-region-currency");
        let date_title = t("settings-region-date-format");
        let time_title = t("settings-region-time-format");
        let measure_title = t("settings-region-measurement");
        let number_title = t("settings-region-number-format");
        let first_title = t("settings-region-first-day");
        let formats = card(
            &t("settings-region-formats-card"),
            rows(vec![
                choice(&currency_title, Box::new(currency)),
                choice(&date_title, Box::new(date)),
                choice(&time_title, Box::new(time)),
                choice(&measure_title, Box::new(units)),
                choice(&number_title, Box::new(number_style)),
                choice(&first_title, Box::new(first_day)),
            ]),
        );

        let amount = loc.number_style.amount(loc.currency.symbol());
        let temp = t(match loc.measurement {
            Measurement::Metric => "settings-region-preview-c",
            Measurement::Imperial => "settings-region-preview-f",
        });
        let week_starts = weekday_label(loc.first_day_of_week);
        let preview = card(
            &t("settings-region-preview-card"),
            rows(vec![
                preview_line(&t("settings-region-preview-amount"), &amount),
                preview_line(&t("settings-region-preview-date"), &loc.date_format.today()),
                preview_line(&t("settings-region-preview-time"), &loc.time_format.now()),
                preview_line(&t("settings-region-preview-temp"), &temp),
                preview_line(&t("settings-region-preview-week"), &week_starts),
            ]),
        );

        crate::widgets::cards(vec![formats, preview])
    }

    fn draft_key(canonical: &str) -> String {
        format!("{canonical}.edit")
    }

    fn dispatch(&mut self, state: &mut AutoCommit, action: &SettingsAction) {
        self.actions.dispatch_with_context(
            state,
            SETTINGS_ACTION_TYPE,
            action,
            tilo_actions::ActionContext::for_action("so.tilo.settings", "Settings", action),
        );
    }

    pub(crate) fn bind_dispatcher(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Settings>,
    ) {
        self.actions.attach(dispatcher);
    }

    fn reconcile(
        &mut self,
        state: &mut AutoCommit,
        root: &ObjId,
        key: &str,
        current: &str,
    ) -> (String, bool) {
        let canonical = read_str(state, root, key).unwrap_or_else(|| current.to_string());
        let draft_key = Self::draft_key(key);
        let draft = read_str(state, root, &draft_key).unwrap_or_else(|| canonical.clone());
        if canonical != current {
            let _ = state.put(root, draft_key, canonical.as_str());
            (canonical, true)
        } else if draft != canonical {
            self.dispatch(state, &set_text_action(key, draft.clone()));
            (draft, true)
        } else {
            (current.to_string(), false)
        }
    }
}

impl Widget for RegionPane {
    ui::provides!(dyn ui::undo::UndoPoints);

    fn sync(&mut self, state: &mut AppModel, root: &ModelPath) {
        let (state, root) = ui::doc::document(state, root);
        if self.needs_seed {
            let loc = self.settings.locale;
            for (key, value) in [
                (CURRENCY_KEY, loc.currency.code().to_string()),
                (DATE_KEY, date_label(loc.date_format)),
                (TIME_KEY, time_label(loc.time_format)),
                (MEASUREMENT_KEY, measure_label(loc.measurement)),
                (NUMBER_STYLE_KEY, loc.number_style.label().to_string()),
                (FIRST_DAY_KEY, weekday_label(loc.first_day_of_week)),
            ] {
                self.dispatch(state, &set_text_action(key, value));
            }
            self.needs_seed = false;
        }

        let mut changed = false;

        let currency_now = self.settings.locale.currency.code();
        let (code, did_change) = self.reconcile(state, root, CURRENCY_KEY, currency_now);
        changed |= did_change;
        if let Some(c) = Currency::from_code(&code)
            && c != self.settings.locale.currency
        {
            self.settings.locale.currency = c;
            changed = true;
        }
        let date_now = date_label(self.settings.locale.date_format);
        let (label, did_change) = self.reconcile(state, root, DATE_KEY, &date_now);
        changed |= did_change;
        if let Some(d) = date_from_shown(&label)
            && d != self.settings.locale.date_format
        {
            self.settings.locale.date_format = d;
            changed = true;
        }
        let time_now = time_label(self.settings.locale.time_format);
        let (label, did_change) = self.reconcile(state, root, TIME_KEY, &time_now);
        changed |= did_change;
        if let Some(style) = time_from_shown(&label)
            && style != self.settings.locale.time_format
        {
            self.settings.locale.time_format = style;
            changed = true;
        }
        let measure_now = measure_label(self.settings.locale.measurement);
        let (label, did_change) = self.reconcile(state, root, MEASUREMENT_KEY, &measure_now);
        changed |= did_change;
        if let Some(m) = measure_from_shown(&label)
            && m != self.settings.locale.measurement
        {
            self.settings.locale.measurement = m;
            changed = true;
        }
        let style_now = self.settings.locale.number_style.label();
        let (label, did_change) = self.reconcile(state, root, NUMBER_STYLE_KEY, style_now);
        changed |= did_change;
        if let Some(style) = NumberStyle::ALL.iter().find(|style| style.label() == label)
            && *style != self.settings.locale.number_style
        {
            self.settings.locale.number_style = *style;
            changed = true;
        }
        let first_day_now = weekday_label(self.settings.locale.first_day_of_week);
        let (label, did_change) = self.reconcile(state, root, FIRST_DAY_KEY, &first_day_now);
        changed |= did_change;
        if let Some(day) = weekday_from_shown(&label)
            && day != self.settings.locale.first_day_of_week
        {
            self.settings.locale.first_day_of_week = day;
            changed = true;
        }

        if changed && let Err(e) = self.settings.save() {
            warn!(target: "tilo::settings", op = "region_save", error = %e, "failed to persist region settings");
        }

        if let Some(c) = &mut self.composed {
            c.sync(state, root);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition();
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);
        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::auto(),
            },
            ..Default::default()
        };
        let node = taffy.new_with_children(style, &[child]).unwrap_or(child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&cn) = children.first()
        {
            c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let was_handled = *ctx.handled;
        if let Some(c) = &mut self.composed
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, cn));
        }
        self.commit_context_pending |= !was_handled && *ctx.handled;
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &'static str {
        "RegionPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ui::undo::UndoPoints for RegionPane {
    fn take_commit_context(&mut self) -> Option<ui::tilo_actions::ActionContext> {
        let pending = std::mem::take(&mut self.commit_context_pending)
            || self
                .composed
                .as_ref()
                .is_some_and(|content| ui::undo::needs_commit(content.as_ref()));
        crate::panes::preference_commit_context(
            pending,
            "region",
            &t("settings-region-undo-name"),
            &t("settings-region-undo-action"),
        )
    }
}

fn preview_line(label: &str, value: &str) -> crate::widgets::El {
    crate::widgets::value_row(label, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::region::Locale;

    /// Seeding, currency/date/time/unit edits and ignoring unknown values.
    #[test]
    fn region_pane_seeds_and_applies_known_choices() {
        // Case `seed_writes_current_settings_into_crdt`.
        {
            let mut pane = RegionPane::with_settings(RegionSettings {
                locale: Locale {
                    currency: Currency::EUR,
                    ..Default::default()
                },
            });
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            assert_eq!(read_str(&doc, &root, CURRENCY_KEY).as_deref(), Some("EUR"));
        }

        // Case `changing_currency_key_updates_settings`.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let mut pane = RegionPane::with_settings(RegionSettings::default());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            // Simulate the dropdown writing a new currency code.
            let _ = doc.put(&root, CURRENCY_KEY, "JPY");
            pane.sync(&mut doc, &root);
            assert_eq!(pane.settings().locale.currency, Currency::JPY);
        }

        // Case `changing_date_and_time_and_units_updates_settings`.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let mut pane = RegionPane::with_settings(RegionSettings::default());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            let _ = doc.put(&root, DATE_KEY, DateStyle::Eu.label());
            let _ = doc.put(&root, TIME_KEY, TimeStyle::H12.label());
            let _ = doc.put(&root, MEASUREMENT_KEY, Measurement::Imperial.label());
            pane.sync(&mut doc, &root);
            assert_eq!(pane.settings().locale.date_format, DateStyle::Eu);
            assert_eq!(pane.settings().locale.time_format, TimeStyle::H12);
            assert_eq!(pane.settings().locale.measurement, Measurement::Imperial);
        }

        // Case `unknown_dropdown_value_is_ignored`.
        {
            // The pane persists through the per-user settings file.
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let mut pane = RegionPane::with_settings(RegionSettings::default());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            let _ = doc.put(&root, CURRENCY_KEY, "not-a-currency");
            pane.sync(&mut doc, &root);
            // Stayed at the default.
            assert_eq!(pane.settings().locale.currency, Currency::USD);
        }
    }
}
