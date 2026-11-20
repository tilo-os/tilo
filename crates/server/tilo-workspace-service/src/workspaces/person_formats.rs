//! The person's region and formats and their time zone, kept by this server in their own store
//! beside their vault ([`person_formats`]).
//!
//! A view this server renders for the person, or for their agent, shows dates and clocks the
//! way their own windows do ([`Workspaces::person_formats`]). Their page keeps it current: the
//! formats they choose in Region & Formats, and the zone their browser is in, which a zone they
//! chose themselves overrides. Only the person on a device of theirs reads or changes it; an
//! agent's request is refused, unknown and forbidden being the same answer.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::Value;
use tilo_actions::remote::ActionCallError::{self, UnknownAction};
use tilo_actions::remote::person_formats::{
    self, BrowserZone, ChooseZone, PersonFormats, SetFormats,
};
use tilo_actions::remote::{Caller, InvokeRequest};
use tilo_lock::MutexRecover as _;
use tilo_user_identity::DeviceCert;

use super::Workspaces;
use super::person_settings::{answer, argument};

/// The file of a person's store their formats are kept in.
const FILE: &str = "formats.json";

/// The most bytes the file holds: four formats and two zone names fit many times over, and a
/// file past it is not read.
const MAX_BYTES: u64 = 4 * 1024;

/// One change at a time on this server: a change reads the file, applies itself and writes it
/// back whole.
static CHANGING: Mutex<()> = Mutex::new(());

impl Workspaces {
    /// Where `person`'s formats are kept.
    fn formats_at(&self, person: &[u8; 32]) -> PathBuf {
        self.person_settings_root(person).join(FILE)
    }

    /// The region and formats and the zones `person` keeps here; none where they keep none
    /// yet. A view rendered here for them, or for their agent, shows dates in these
    /// ([`PersonFormats::zone`]).
    #[must_use]
    pub fn person_formats(&self, person: &[u8; 32]) -> Option<PersonFormats> {
        match read(&self.formats_at(person)) {
            Ok(kept) => kept,
            Err(error) => {
                tracing::warn!(%error, "a person's formats are not read");
                None
            }
        }
    }

    /// The formats and the zone a view rendered here for `person`, or for their agent, shows
    /// dates in: the formats they keep (the defaults where they keep none), and the zone they
    /// chose, else their browser's. None where they keep nothing here.
    #[must_use]
    pub fn person_region(
        &self,
        person: &[u8; 32],
    ) -> Option<(tilo_formats::RegionFormats, Option<String>)> {
        let kept = self.person_formats(person)?;
        let zone = kept.zone().map(str::to_owned);
        let formats = kept
            .formats
            .map(tilo_formats::RegionFormats::from_section)
            .unwrap_or_default();
        Some((formats, zone))
    }

    /// Change what `person` keeps with `change`, one change at a time.
    fn change_formats(
        &self,
        person: &[u8; 32],
        change: impl FnOnce(&mut PersonFormats),
    ) -> Result<PersonFormats, String> {
        let path = self.formats_at(person);
        let one_at_a_time = CHANGING.lock_recover();
        let mut kept = read(&path)?.unwrap_or_default();
        change(&mut kept);
        let bytes = serde_json::to_vec(&kept).map_err(|error| error.to_string())?;
        tilo_files::native::atomic::write_creating_parent_with_mode(&path, &bytes, 0o600)
            .map_err(|error| error.to_string())?;
        drop(one_at_a_time);
        Ok(kept)
    }

    /// Answer a [`person_formats::PROVIDER`] request: the person's own region, read or changed
    /// by them on a device of theirs and by nobody else.
    pub(super) fn formats_invoke(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let person = Self::asking_person(caller, certificate, workspace_id)?;
        if request.target.address.resource != person_formats::RESOURCE {
            return Err(UnknownAction);
        }
        let kept = match request.target.address.action.as_str() {
            person_formats::READ => Ok(self.person_formats(&person).unwrap_or_default()),
            person_formats::SET => {
                let SetFormats { formats } = argument(request, "the formats to keep")?;
                // Kept as the settings section reads them back: what is not a format, or not
                // one of its values, is not kept.
                let formats = tilo_formats::RegionFormats::from_section(formats).to_section();
                self.change_formats(&person, |kept| kept.formats = Some(formats))
            }
            person_formats::CHOOSE_ZONE => {
                let ChooseZone { zone } = argument(request, "the zone chosen, or none")?;
                let zone = zone.map(zone_named).transpose()?;
                self.change_formats(&person, |kept| kept.chosen_zone = zone)
            }
            person_formats::BROWSER_ZONE => {
                let BrowserZone { zone } = argument(request, "the zone the browser is in")?;
                let zone = zone_named(zone)?;
                self.change_formats(&person, |kept| kept.browser_zone = Some(zone))
            }
            _ => return Err(UnknownAction),
        };
        answer(&kept)
    }
}

/// `zone`, when it names a zone as the time-zone database does.
fn zone_named(zone: String) -> Result<String, ActionCallError> {
    if tilo_clock::is_zone_name(&zone) {
        Ok(zone)
    } else {
        Err(ActionCallError::InvalidRequest {
            reason: "a zone is named as the time-zone database names it, such as Europe/Vienna"
                .to_owned(),
        })
    }
}

/// What is kept at `path`, or none where nothing is kept yet.
fn read(path: &Path) -> Result<Option<PersonFormats>, String> {
    match std::fs::metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
        Ok(kept) if kept.len() > MAX_BYTES => {
            return Err("the kept formats are larger than they are ever kept".to_owned());
        }
        Ok(_) => {}
    }
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| error.to_string())
}
