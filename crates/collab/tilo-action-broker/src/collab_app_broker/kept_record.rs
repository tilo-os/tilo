//! A renamed app keeps using the record of the earliest id it had (`<room>#app:<earlier id>`),
//! where it is: its launch joins that channel as its own and works on it as itself.
//!
//! Only an id the launcher's catalogue believes the app had counts: the same publisher signed it
//! ([`crate::services::LauncherServices::earlier_ids`]). The connection's route
//! ([`super::JoinedChannel::route`]) names the launched app, as its worker, publications
//! and grants do; the record the channel carries ([`super::JoinedChannel::record_route`])
//! stays under the earlier id, as its reads, writes and keys do.

use super::{CrdtRoute, HostServices, JoinedChannel, native_media::LocalAppConnection};

/// The ids `app` had before, as the launcher's catalogue believes them.
pub(super) fn earlier_ids_of(services: &HostServices, app: &str) -> Vec<String> {
    services
        .launcher
        .as_ref()
        .map(|launcher| launcher.earlier_ids().of(app).to_vec())
        .unwrap_or_default()
}

/// The route a join of `record` works as, and the launched app it works as when that is not the
/// record's own app: the launch `local` of an app that had the record's id before.
pub(super) fn working_route(
    local: Option<&LocalAppConnection>,
    record: &CrdtRoute,
) -> (CrdtRoute, Option<String>) {
    let as_app = local
        .filter(|app| app.app_id != record.app_id && app.keeps_records_of(&record.app_id))
        .map(|app| app.app_id.clone());
    let route = match &as_app {
        Some(app) => CrdtRoute {
            app_id: app.clone(),
            ..record.clone()
        },
        None => record.clone(),
    };
    (route, as_app)
}

impl JoinedChannel {
    /// The route the connection works as. A record-scoped connection names its record inside
    /// the channel id (the SDK joins `<cluster>#record:<id>`), parsed back out here; the app is
    /// the launched one, also where the channel is the record of an id it had before.
    pub(super) fn route(&self) -> CrdtRoute {
        let route = self.record_route();
        match &self.as_app {
            Some(app) => CrdtRoute {
                app_id: app.clone(),
                ..route
            },
            None => route,
        }
    }

    /// The record the channel carries: the route [`Self::route`] names, except for a renamed
    /// app, whose record stays under the id it had before.
    pub(super) fn record_route(&self) -> CrdtRoute {
        CrdtRoute::from_channel_key(&self.channel_id)
    }
}
