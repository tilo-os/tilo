//! What the attested launch of an app may do on a route it joins: its own
//! route, or another app's read-only as its surface declares.
use super::{AdmittedJoinIdentity, AppBrokerState, CrdtRoute, person_document_join};
use tilo_actions::remote::asked::AskedFacts;
use tilo_actions::remote::{ActionCallError, ResourceAddress};

/// What the attested launch of an app may do on a route it joins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RouteAccess {
    /// Its own route, or one that names no app: it reads and changes there
    /// as its admission allows.
    Own,
    /// Another app's record whose kinds it declared it reads (`reads` in
    /// its surface, or a record of the person's its surface declares): it
    /// reads, live, and changes nothing. It changes another app's resources
    /// only by invoking that app's actions.
    ReadOnly,
}

/// What the attested launch of `app` may do on `route`: its own route, or one of the workspace's
/// records ([`collab_types::WORKSPACE_DOCUMENTS`]); or, read-only, a record of another app whose
/// kinds its surface declares it reads, once the device's one reader lets the person read that
/// record. In the person's own workspace only the records of theirs its surface declares
/// ([`person_document_join`]). Anything else is refused: no app writes on another app's route.
pub(super) fn attested_route_access(
    app: &str,
    route: &CrdtRoute,
    own_workspace: bool,
) -> Result<RouteAccess, String> {
    if own_workspace {
        person_document_join(app, route)?;
    }
    if route.app_id.is_empty() || route.app_id == app {
        return Ok(RouteAccess::Own);
    }
    // A workspace record is changed from any app's window of its workspace; the
    // workspace's grants on its owner decide each change.
    if collab_types::shared_workspace_document(&route.app_id, &route.document_id).is_some() {
        return Ok(RouteAccess::Own);
    }
    if !own_workspace && !collab_types::app_surface::reads_app(app, &route.app_id) {
        return Err(format!(
            "{app} declares no read of {}'s resources; it changes another app's \
             resources only by invoking its actions",
            route.app_id
        ));
    }
    Ok(RouteAccess::ReadOnly)
}

impl AppBrokerState {
    /// Whether the person `identity` certifies may read `route`'s record, which `app` joins to
    /// read: as the device's one reader decides, and where the person is to be asked first, once
    /// they answer the read's asked record ([`tilo_actions::remote::asked::AskedReads`]).
    pub(super) async fn person_reads_joined(
        &self,
        identity: &AdmittedJoinIdentity,
        app: &str,
        route: &CrdtRoute,
    ) -> Result<(), String> {
        let refused = || {
            format!(
                "the person may not read the record of {} that {app} reads",
                route.app_id
            )
        };
        let reads = self.records().ok_or_else(refused)?;
        let caller = identity.certified_caller(&route.cluster_id, false);
        let address = ResourceAddress {
            workspace: route.cluster_id.clone(),
            app: route.app_id.clone(),
            record: route.document_id.clone(),
        };
        // The device's reader may block to decide, never on the broker's own thread.
        let (deciding, deciding_for) = (caller.clone(), address.clone());
        let decided = tokio::task::spawn_blocking(move || reads.may_read(&deciding, &deciding_for))
            .await
            .map_err(|_| refused())?
            .await;
        let facts = match decided {
            Some(Ok(())) => return Ok(()),
            Some(Err(ActionCallError::ConfirmationRequired(facts))) => *facts,
            _ => return Err(refused()),
        };
        let asked = self.asked_reads().ok_or_else(refused)?;
        let facts = AskedFacts {
            verb: if facts.verb.is_empty() {
                tilo_actions::remote::asked::READ.to_owned()
            } else {
                facts.verb
            },
            on: facts.on.or_else(|| Some(route.document_id.clone())),
            ..facts
        };
        match asked.ask_read(&caller, app, &address, facts).await {
            Some(Ok(())) => Ok(()),
            Some(Err(why)) => Err(format!("{}: {why}", refused())),
            None => Err(refused()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace_route(app: &str) -> CrdtRoute {
        CrdtRoute {
            app_id: app.into(),
            cluster_id: "4f2c1a64-0d7e-4b8e-9b55-2a6f3c9d1e10".into(),
            document_id: "record-1".into(),
        }
    }

    /// A route to `record` of `app` in the person's own workspace.
    fn person_route(app: &str, record: &str) -> CrdtRoute {
        CrdtRoute {
            app_id: app.into(),
            cluster_id: collab_types::person_workspace(&[7; 32]).to_string(),
            document_id: record.into(),
        }
    }

    /// The surfaces these cases register: what each app may read, and the
    /// person's own record one of them declares.
    static NOTES: &[u8] =
        b"[[resources]]\nkind = \"tl.test.lang-notes.Note\"\nlabel = \"Note\"\nversion = 1\n";
    static BOARD: &[u8] = b"reads = [\"tl.test.lang-notes.Note\"]\n\n[[resources]]\nkind = \"tl.test.lang-board.Card\"\nlabel = \"Card\"\nversion = 1\n";
    static CHAT: &[u8] =
        b"[[resources]]\nkind = \"tl.test.lang-chat.Message\"\nlabel = \"Message\"\nversion = 1\n";
    static TERMINAL: &[u8] = b"[[resources]]\nkind = \"test-person.accounts\"\nlabel = \"Accounts\"\nperson = true\nversion = 1\n";

    /// An attested launch changes only its own route. Another app's record
    /// it joins only to read, when its surface declares that it reads that
    /// app's kinds and the person may read the record; it changes such a
    /// record only through the owning app's actions.
    #[test]
    fn an_app_writes_only_its_own_route_and_reads_another_apps_only_as_declared() {
        collab_types::app_surface::register_package("test.lang-notes", NOTES);
        collab_types::app_surface::register_package("test.lang-board", BOARD);
        collab_types::app_surface::register_package("test.lang-chat", CHAT);
        workspace_routes_are_the_apps_own_or_declared_reads();
        person_records_are_read_never_written();
    }

    /// Its own route, whatever it reads; another app's only to read, and only
    /// when it declares that read and the person may read the record.
    fn workspace_routes_are_the_apps_own_or_declared_reads() {
        assert_eq!(
            attested_route_access(
                "test.lang-board",
                &workspace_route("test.lang-board"),
                false
            ),
            Ok(RouteAccess::Own),
            "its own route, whatever it reads"
        );
        assert_eq!(
            attested_route_access("test.lang-board", &workspace_route(""), false),
            Ok(RouteAccess::Own),
            "a route of no app is the workspace's"
        );
        assert_eq!(
            attested_route_access(
                "test.lang-board",
                &workspace_route("test.lang-notes"),
                false
            ),
            Ok(RouteAccess::ReadOnly),
            "Board reads the notes it declares, read-only"
        );
        assert!(
            attested_route_access("test.lang-board", &workspace_route("test.lang-chat"), false)
                .is_err(),
            "nor another app's record it declares no read of"
        );
        let emoji = CrdtRoute {
            document_id: collab_types::workspace_document_id("tilo.workspace.emoji"),
            ..workspace_route("so.tilo.chat")
        };
        assert_eq!(
            attested_route_access("test.lang-board", &emoji, false),
            Ok(RouteAccess::Own),
            "a workspace record is any app's of the workspace, as its grants decide"
        );
        assert!(
            attested_route_access("test.lang-chat", &workspace_route("test.lang-notes"), false)
                .is_err(),
            "an app that reads nothing joins only its own route"
        );
        assert!(
            attested_route_access("test.unknown", &workspace_route("test.lang-notes"), false)
                .is_err(),
            "an app whose surface the broker does not hold reads nothing"
        );
    }

    /// Another app's record of the person's is joined to be read, never
    /// written.
    fn person_records_are_read_never_written() {
        let accounts = collab_types::person_document_id("test-person.accounts");
        collab_types::app_surface::register_package("test.person-reader", TERMINAL);
        assert_eq!(
            attested_route_access(
                "test.person-reader",
                &person_route("test.person-settings", &accounts),
                true
            ),
            Ok(RouteAccess::ReadOnly),
            "another app's record of the person's is read, never written"
        );
    }
}
