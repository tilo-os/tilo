//! The person's time zone on a page: automatic, from the browser, or one they choose.
//!
//! Their own workspace keeps it, through the server of theirs the page is on ([`formats`]), so
//! the dates their agent reads of an app are in the zone the page shows them in. Choosing one
//! tells the server and shows it here at once; Automatic goes back to the browser's. A computer
//! of their own keeps its zone in the system's Time & date pane instead.

#![cfg_attr(
    feature = "tilo-os",
    allow(dead_code, reason = "a device's build carries this only for its tests")
)]

use std::cell::{Cell, RefCell};

use ui::LocalState;
use ui::framework::{Actions, EffectAnswer, EffectCx, Effected};
use ui::tilo_actions::remote::InvokeArgument;
use ui::tilo_actions::remote::formats::{self, Formats};
use ui::tilo_resource;
use ui::widget::Dropdown;

use super::zone_name;
use crate::effects::effect;
use crate::i18n::{t, t_args};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use provider_settings_ui::widgets::{
    Item, SettingsSection, block, error_row, note, rows, section, value_row,
};

crate::effects::effect_family!(ZoneEffect => answer);

crate::area::pane_area! {
    ZoneArea;
    crate::area::serves!(RegionFormats);
    fn poll(&self) {
        ZoneArea::poll(self);
    }
    fn busy(&self) -> bool {
        self.choosing.borrow().is_some()
    }
}

/// The choice that follows the browser's zone.
pub const AUTOMATIC: &str = "automatic";

/// The person's zone as the pane shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Shown {
    /// The server has not answered yet.
    Reading,
    /// The browser's.
    Automatic,
    /// One they chose.
    Zone(String),
}

impl Shown {
    fn of(zone: Option<String>) -> Self {
        zone.map_or(Self::Automatic, Self::Zone)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum ZoneEffect {
    /// Show dates and times in a time zone, here and in what the person's agents read, or
    /// in the browser's with "automatic".
    #[effect(kind = "outside")]
    #[effect(idempotent)]
    ChooseZone {
        #[param(
            "An IANA time zone, such as `Europe/Vienna`, or `automatic` to follow this browser's"
        )]
        zone: String,
    },
}

pub fn area(app: &SettingsApp) -> Option<&ZoneArea> {
    app.areas().find()
}

/// What the server keeps of the person's zone, and the request that changes it.
pub struct ZoneArea {
    remote: ui::RemoteActions,
    /// The zone the person chose, as the server answered it.
    chosen: RefCell<Shown>,
    reading: RefCell<Option<ui::InvocationCall>>,
    /// A choice on its way to the server, and the call that takes it there.
    choosing: RefCell<Option<(Option<String>, ui::InvocationCall)>>,
    /// Why the server could not be read or the choice was not kept.
    failed: RefCell<Option<String>>,
    /// The zones offered, read once the pane opens.
    zones: RefCell<Option<Vec<String>>>,
    pub shown: LocalState<u64>,
    asked: Cell<bool>,
}

impl ZoneArea {
    #[must_use]
    pub fn new(remote: ui::RemoteActions) -> Self {
        Self {
            remote,
            chosen: RefCell::new(Shown::Reading),
            reading: RefCell::new(None),
            choosing: RefCell::new(None),
            failed: RefCell::new(None),
            zones: RefCell::new(None),
            shown: LocalState::new(0),
            asked: Cell::new(false),
        }
    }

    /// Ask the server what it keeps, once the pane is shown.
    pub fn read(&self) {
        if self.asked.replace(true) {
            return;
        }
        *self.reading.borrow_mut() = Some(self.remote.invoke(
            formats::target(formats::READ),
            InvokeArgument::Json(serde_json::Map::new()),
            None,
        ));
    }

    /// Have the server keep `zone` as the person's, `None` for the browser's.
    pub fn choose(&self, zone: Option<String>) {
        let mut argument = serde_json::Map::new();
        argument.insert(
            "zone".into(),
            zone.clone().map_or(serde_json::Value::Null, Into::into),
        );
        let call = self.remote.invoke(
            formats::target(formats::CHOOSE_ZONE),
            InvokeArgument::Json(argument),
            None,
        );
        *self.failed.borrow_mut() = None;
        *self.choosing.borrow_mut() = Some((zone, call));
        self.changed();
    }

    /// Take the server's answers that arrived.
    pub fn poll(&self) {
        let read = self
            .reading
            .borrow()
            .as_ref()
            .and_then(crate::panes::person_settings::settled);
        if let Some(read) = read {
            *self.reading.borrow_mut() = None;
            match read.and_then(|kept| {
                serde_json::from_value::<Formats>(kept).map_err(|error| error.to_string())
            }) {
                Ok(kept) => self.keep(kept.chosen_zone),
                Err(error) => *self.failed.borrow_mut() = Some(error),
            }
            self.changed();
        }
        let chose = self
            .choosing
            .borrow()
            .as_ref()
            .and_then(|(_, call)| crate::panes::person_settings::settled(call));
        if let Some(chose) = chose {
            let zone = self.choosing.borrow_mut().take().and_then(|(zone, _)| zone);
            match chose {
                Ok(_) => self.keep(zone),
                Err(error) => {
                    *self.failed.borrow_mut() = Some(t_args(
                        "settings-region-zone-failed",
                        &[("error", error.as_str().into())],
                    ));
                }
            }
            self.changed();
        }
    }

    /// The person's choice, as the server keeps it: shown on this page at once.
    fn keep(&self, zone: Option<String>) {
        ui::format::region::install_zone_here(ui::format::region::chosen_zone(zone.as_deref()));
        *self.chosen.borrow_mut() = Shown::of(zone);
    }

    fn changed(&self) {
        self.shown.set(self.shown.get().wrapping_add(1));
    }

    /// The zone shown as chosen: the one on its way to the server, else the one it keeps.
    fn shown_choice(&self) -> Shown {
        if let Some((zone, _)) = self.choosing.borrow().as_ref() {
            return Shown::of(zone.clone());
        }
        self.chosen.borrow().clone()
    }

    fn zones(&self) -> Vec<String> {
        self.zones
            .borrow_mut()
            .get_or_insert_with(ui::clock::zone_names)
            .clone()
    }

    /// The time zone card.
    pub fn card(&self, actions: &Actions<Settings>) -> SettingsSection {
        let mut items: Vec<Item> = Vec::new();
        let chosen = match self.shown_choice() {
            Shown::Reading => None,
            Shown::Automatic => Some(AUTOMATIC.to_owned()),
            Shown::Zone(zone) => Some(zone),
        };
        match chosen {
            None => items.push(value_row(&t("settings-region-zone"), &t("time-reading"))),
            Some(chosen) => {
                let zones = self.zones();
                let mut picker = Dropdown::from_state(Vec::new(), &LocalState::new(chosen))
                    .with_id("region.zone")
                    .with_resolved_accessible_name(t("settings-region-zone"));
                picker.set_value_options(choices(&zones, tilo_clock::local_zone_name().as_deref()));
                items.push(block(Box::new(
                    actions.bind_effect(picker.fill(), effect(ZoneEffect::CHOOSE_ZONE)),
                )));
            }
        }
        if let Some(failed) = self.failed.borrow().as_ref() {
            items.push(error_row(&t("settings-region-zone"), failed));
        }
        items.push(note(&t("settings-region-zone-body")));
        section(&t("settings-region-zone-card"), rows(items))
    }
}

/// The choices the picker offers: Automatic, naming the browser's zone, then every zone by its
/// city and region.
#[must_use]
pub fn choices(zones: &[String], browser: Option<&str>) -> Vec<(String, String)> {
    let automatic = match browser.and_then(zone_name) {
        Some(zone) => t_args(
            "settings-region-zone-automatic",
            &[("zone", zone.as_str().into())],
        ),
        None => t("settings-region-zone-automatic-unnamed"),
    };
    let mut named: Vec<(String, String)> = zones
        .iter()
        .map(|zone| {
            (
                zone.clone(),
                zone_name(zone).unwrap_or_else(|| zone.clone()),
            )
        })
        .collect();
    named.sort_by(|a, b| a.1.cmp(&b.1));
    std::iter::once((AUTOMATIC.to_owned(), automatic))
        .chain(named)
        .collect()
}

pub(crate) fn answer(
    app: &SettingsApp,
    effect: ZoneEffect,
    _cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    let ZoneEffect::ChooseZone { zone } = effect;
    let area = area(app).ok_or_else(crate::area::missing)?;
    let zone = if zone == AUTOMATIC {
        None
    } else if area.zones().contains(&zone) {
        Some(zone)
    } else {
        return Err(format!("`{zone}` is not a time zone this browser knows"));
    };
    area.choose(zone);
    Ok(Effected::done())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use std::sync::{Arc, Mutex};

    use ui::tilo_actions::remote::{
        ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
        InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
    };

    use super::*;

    /// The person's server: what it keeps of their zone, and what it was asked.
    #[derive(Default)]
    struct Server {
        chosen: Mutex<Option<String>>,
        asked: Mutex<Vec<serde_json::Value>>,
    }

    impl ActionProvider for Server {
        fn query(
            &self,
            _: &Caller,
            _: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionCallError>> {
            Pending::ready(Err(ActionCallError::NotPermitted))
        }

        fn invoke(
            &self,
            _: &Caller,
            request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
            let InvokeArgument::Json(argument) = &request.argument else {
                return Pending::ready(Err(ActionCallError::NotPermitted));
            };
            if request.target == formats::target(formats::CHOOSE_ZONE) {
                self.asked
                    .lock()
                    .unwrap()
                    .push(serde_json::Value::Object(argument.clone()));
                *self.chosen.lock().unwrap() = argument
                    .get("zone")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
            }
            let kept = Formats {
                chosen_zone: self.chosen.lock().unwrap().clone(),
                browser_zone: Some("Pacific/Auckland".into()),
                ..Formats::default()
            };
            Pending::ready(Ok(InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Completed {
                    output: ui::tilo_actions::ActionOutput {
                        value: Some(serde_json::to_value(kept).unwrap()),
                        resources: Vec::new(),
                    },
                },
            }))
        }

        fn observe(
            &self,
            _: &Caller,
            _: &ui::tilo_actions::InvocationId,
        ) -> Option<InvocationState> {
            None
        }

        fn cancel(
            &self,
            _: &Caller,
            _: &ui::tilo_actions::InvocationId,
        ) -> Pending<Result<(), ActionCallError>> {
            Pending::ready(Ok(()))
        }
    }

    /// The zone the page reads its dates in now.
    fn read_in() -> Option<String> {
        ui::clock::Zone::here().name()
    }

    /// A zone the person chose is the server's to keep, and the page reads its dates in it at
    /// once; Automatic tells the server none, and the page reads the browser's again.
    #[test]
    fn a_chosen_zone_is_kept_by_the_server_and_read_in_here() {
        const VIENNA: &str = "Europe/Vienna";
        if ui::clock::Zone::named(VIENNA).is_none()
            || tilo_clock::local_zone_name().as_deref() == Some(VIENNA)
        {
            // Without a zone database, or in Vienna already, the choice shows nothing new.
            return;
        }
        let server = Arc::new(Server::default());
        let zone = ZoneArea::new(ui::RemoteActions::with_provider(
            server.clone(),
            Caller::local_person("ada"),
        ));
        let own = read_in();
        zone.read();
        zone.poll();
        assert_eq!(
            zone.shown_choice(),
            Shown::Automatic,
            "nothing chosen: automatic"
        );

        zone.choose(Some(VIENNA.into()));
        zone.poll();
        assert_eq!(
            server.asked.lock().unwrap().last(),
            Some(&serde_json::json!({ "zone": VIENNA }))
        );
        assert_eq!(zone.shown_choice(), Shown::Zone(VIENNA.into()));
        assert_eq!(
            read_in().as_deref(),
            Some(VIENNA),
            "the page reads Vienna's clock"
        );

        zone.choose(None);
        zone.poll();
        assert_eq!(
            server.asked.lock().unwrap().last(),
            Some(&serde_json::json!({ "zone": null }))
        );
        assert_eq!(zone.shown_choice(), Shown::Automatic);
        assert_eq!(
            read_in(),
            own,
            "automatic reads the browser's own zone again"
        );
    }

    /// The picker opens on Automatic, naming the browser's zone, then every zone by its city.
    #[test]
    fn automatic_names_the_browsers_zone() {
        ui::i18n::I18nManager::init("en-US");
        let offered = choices(
            &["Europe/Vienna".into(), "America/New_York".into()],
            Some("Pacific/Auckland"),
        );
        assert_eq!(offered[0].0, AUTOMATIC);
        assert!(offered[0].1.contains("Auckland"), "{}", offered[0].1);
        assert_eq!(
            offered[1..]
                .iter()
                .map(|(zone, _)| zone.as_str())
                .collect::<Vec<_>>(),
            ["America/New_York", "Europe/Vienna"]
        );
    }
}
