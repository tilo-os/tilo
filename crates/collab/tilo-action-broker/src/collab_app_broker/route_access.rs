//! What the attested launch of an app may do on a route it joins: its own
//! route, or another app's read-only as its surface declares.
use super::{
    ActionKind, AdmittedJoinIdentity, Answer, CrdtRoute, WorkspaceTreeHandle, person_record_join,
};

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

/// What the attested launch of `app` may do on `route`: its own route, or
/// one of the workspace's records
/// ([`collab_types::WORKSPACE_RECORDS`]); or, read-only, a record of another app whose kinds its surface declares it
/// reads, when the person may read that record (`reads_record`). In the
/// person's own room only the records of theirs its surface declares
/// ([`person_record_join`]). Anything else is refused: no app writes on
/// another app's route.
pub(super) fn attested_route_access(
    app: &str,
    route: &CrdtRoute,
    personal_cluster: bool,
    reads_record: impl FnOnce() -> bool,
) -> Result<RouteAccess, String> {
    if personal_cluster {
        person_record_join(app, route)?;
    }
    if route.app_id.is_empty() || route.app_id == app {
        return Ok(RouteAccess::Own);
    }
    // A pair record ([`collab_types::direct_pair`]) is the app's that keeps the
    // conversation in it: no other app joins one, whatever it declares it
    // reads, and the two people's own doors decide who of them reads it.
    if collab_types::direct_pair(&route.record_id).is_some() {
        return Err(format!(
            "{app} joins no record of a pair of people; {} keeps it",
            route.app_id
        ));
    }
    // A workspace record is changed from any app's window of its workspace; the
    // workspace's grants on its owner decide each change.
    if collab_types::workspace_record(&route.app_id, &route.record_id).is_some() {
        return Ok(RouteAccess::Own);
    }
    if !personal_cluster && !ui::app_surface::reads_app(app, &route.app_id) {
        return Err(format!(
            "{app} declares no read of {}'s resources; it changes another app's \
             resources only by invoking its actions",
            route.app_id
        ));
    }
    if !reads_record() {
        return Err(format!(
            "the person may not read the record of {} that {app} reads",
            route.app_id
        ));
    }
    Ok(RouteAccess::ReadOnly)
}

/// Whether `identity` may read the record `route` names, as the workspace's
/// committed authority decides now.
pub(super) fn identity_reads_record(
    membership: Option<&WorkspaceTreeHandle>,
    identity: &AdmittedJoinIdentity,
    route: &CrdtRoute,
    now: u64,
) -> bool {
    let node =
        collab_types::validator::document_node(&route.cluster_id, &route.app_id, &route.record_id);
    identity
        .with_current_authority(membership, &route.cluster_id, |authority| {
            authority
                .decide(
                    &node,
                    ActionKind::Read,
                    &tilo_desktop_model::context_at(now),
                )
                .answer
                != Answer::Deny
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace_route(app: &str) -> CrdtRoute {
        CrdtRoute {
            app_id: app.into(),
            cluster_id: "4f2c1a64-0d7e-4b8e-9b55-2a6f3c9d1e10".into(),
            record_id: "record-1".into(),
        }
    }

    /// A route to `record` of `app` in the person's own room.
    fn person_route(app: &str, record: &str) -> CrdtRoute {
        CrdtRoute {
            app_id: app.into(),
            cluster_id: collab_types::personal_cluster(&[7; 32]).to_string(),
            record_id: record.into(),
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
        ui::app_surface::register_package("test.lang-notes", NOTES);
        ui::app_surface::register_package("test.lang-board", BOARD);
        ui::app_surface::register_package("test.lang-chat", CHAT);
        workspace_routes_are_the_apps_own_or_declared_reads();
        person_records_are_read_never_written();
        pair_records_belong_to_the_app_that_keeps_them();
    }

    /// Its own route, whatever it reads; another app's only to read, and only
    /// when it declares that read and the person may read the record.
    fn workspace_routes_are_the_apps_own_or_declared_reads() {
        assert_eq!(
            attested_route_access(
                "test.lang-board",
                &workspace_route("test.lang-board"),
                false,
                || false
            ),
            Ok(RouteAccess::Own),
            "its own route, whatever it reads"
        );
        assert_eq!(
            attested_route_access("test.lang-board", &workspace_route(""), false, || false),
            Ok(RouteAccess::Own),
            "a route of no app is the workspace's"
        );
        assert_eq!(
            attested_route_access(
                "test.lang-board",
                &workspace_route("test.lang-notes"),
                false,
                || true
            ),
            Ok(RouteAccess::ReadOnly),
            "Board reads the notes it declares, read-only"
        );
        assert!(
            attested_route_access(
                "test.lang-board",
                &workspace_route("test.lang-notes"),
                false,
                || false
            )
            .is_err(),
            "not when the person may not read the record"
        );
        assert!(
            attested_route_access(
                "test.lang-board",
                &workspace_route("test.lang-chat"),
                false,
                || true
            )
            .is_err(),
            "nor another app's record it declares no read of"
        );
        let emoji = CrdtRoute {
            record_id: collab_types::workspace_record_id("tilo.workspace.emoji"),
            ..workspace_route("so.tilo.chat")
        };
        assert_eq!(
            attested_route_access("test.lang-board", &emoji, false, || false),
            Ok(RouteAccess::Own),
            "a workspace record is any app's of the workspace, as its grants decide"
        );
        assert!(
            attested_route_access(
                "test.lang-chat",
                &workspace_route("test.lang-notes"),
                false,
                || true
            )
            .is_err(),
            "an app that reads nothing joins only its own route"
        );
        assert!(
            attested_route_access(
                "test.unknown",
                &workspace_route("test.lang-notes"),
                false,
                || true
            )
            .is_err(),
            "an app whose surface the broker does not hold reads nothing"
        );
    }

    /// Another app's record of the person's is joined to be read, never
    /// written.
    fn person_records_are_read_never_written() {
        let accounts = collab_types::person_record_id("test-person.accounts");
        ui::app_surface::register_package("test.person-reader", TERMINAL);
        assert_eq!(
            attested_route_access(
                "test.person-reader",
                &person_route("test.person-settings", &accounts),
                true,
                || true
            ),
            Ok(RouteAccess::ReadOnly),
            "another app's record of the person's is read, never written"
        );
    }

    /// A pair's record is the app's that keeps the conversation in it: no
    /// other app joins one, whatever it declares it reads.
    fn pair_records_belong_to_the_app_that_keeps_them() {
        let pair = collab_types::direct_pair_id(
            "4f2c1a64-0d7e-4b8e-9b55-2a6f3c9d1e10",
            &"11".repeat(32),
            &"22".repeat(32),
        )
        .unwrap();
        let conversation = CrdtRoute {
            record_id: format!("derived:on-demand@{}", collab_types::direct_record(&pair)),
            ..workspace_route("test.lang-chat")
        };
        assert_eq!(
            attested_route_access("test.lang-chat", &conversation, false, || false),
            Ok(RouteAccess::Own),
            "the app that keeps a pair's conversation joins its record"
        );
        assert!(
            attested_route_access("test.lang-notes", &conversation, false, || true).is_err(),
            "no other app joins a pair's record, whatever it declares it reads"
        );
        assert!(
            attested_route_access("test.lang-board", &conversation, false, || true).is_err(),
            "an app that reads another's kinds joins no pair record of them either"
        );
    }
}
