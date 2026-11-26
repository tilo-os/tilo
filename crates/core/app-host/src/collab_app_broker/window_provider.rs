//! What a caller names a window by: the provider its broker lists it under.
//!
//! A window is published as one surface of the app it runs, and its page
//! actions are addressed to that surface's provider. A session holding one
//! window asks here which provider that is, so what it reports to a host is
//! what a caller can reach the window by.

use super::AppBrokerState;

impl AppBrokerState {
    /// The provider one of `app`'s live windows is listed under in `workspace`:
    /// the id a caller names it by, and the one its actions are addressed to.
    /// `record` names the record the window shows, which picks that one among
    /// the app's windows when it can; the newest window is the answer when
    /// none of them shows it. `None` while the app publishes no window there.
    #[must_use]
    pub fn window_provider(
        &self,
        app_id: &str,
        workspace: &str,
        record: Option<&str>,
    ) -> Option<u64> {
        let surfaces = self.publishers.surfaces.lock().ok()?;
        surfaces
            .values()
            .filter(|surface| {
                surface.live && surface.app_id == app_id && surface.route.cluster_id == workspace
            })
            .max_by_key(|surface| {
                (
                    record.is_some_and(|record| surface.route.document_id == record),
                    surface.peer_id,
                )
            })
            .map(|surface| surface.peer_id)
    }
}

#[cfg(test)]
mod window_provider_tests {
    use super::super::PublishedSurface;
    use super::super::{AppBrokerState, BrokerChannels, CrdtRoute, FanoutState, PeerPublisher};
    use std::sync::{Arc, Mutex};

    const BROWSER: &str = tilo_app_id::apps::BROWSER;
    const WORKSPACE: &str = "6f1b4a7e-0b2f-4d3a-9a1e-2c3d4e5f6a7b";
    const RECORD: &str = "derived:default@tab:0000000000000000";

    fn state() -> Arc<AppBrokerState> {
        let (state, _events) = AppBrokerState::new(
            &Arc::new(FanoutState::new()),
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            None,
            None,
        );
        let surface = |peer_id: u64, app: &str, record: &str, live: bool| PublishedSurface {
            peer_id,
            app_id: app.to_owned(),
            route: CrdtRoute::record(app, WORKSPACE, record),
            actions: Vec::new(),
            live,
        };
        state.replace_published_surfaces_for_test(vec![
            surface(2, BROWSER, "another-record", true),
            surface(3, BROWSER, RECORD, true),
            surface(9, BROWSER, RECORD, false),
            surface(4, "so.tilo.notes", RECORD, true),
        ]);
        state
    }

    /// The provider a window is named by is the one the broker lists it under:
    /// the newest live window of the app, on the record asked for when one shows
    /// it. A window reported under any other id is one no caller finds.
    #[test]
    fn a_window_is_named_by_the_provider_the_broker_lists_it_under() {
        let state = state();
        assert_eq!(
            state.window_provider(BROWSER, WORKSPACE, Some(RECORD)),
            Some(3),
            "the live window showing that record"
        );
        assert_eq!(
            state.window_provider(BROWSER, WORKSPACE, Some("not-shown-here")),
            Some(3),
            "the newest live window when none shows the record"
        );
        assert_eq!(
            state.window_provider(BROWSER, WORKSPACE, None),
            Some(3),
            "a closed window is no window"
        );
        assert_eq!(
            state.window_provider(BROWSER, "another-workspace", None),
            None,
            "no window of the app in a workspace it publishes none in"
        );
        assert_eq!(
            state.window_provider("so.tilo.mail", WORKSPACE, None),
            None,
            "no window of an app that publishes none"
        );
    }
}
