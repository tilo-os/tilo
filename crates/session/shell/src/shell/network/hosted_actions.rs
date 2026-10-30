//! Asking a hosted room's host: reading a resource view it serves, and
//! whether another device hosts the room this one holds.
use super::{ShellNetwork, room_topic_id};
use crate::shell::native_workspace_owner::NativeWorkspaceOwner;
use crate::shell::state::RoomId;

/// How long one request to a room's host may take end to end.
#[cfg(not(test))]
fn host_timeout() -> std::time::Duration {
    std::time::Duration::from_secs(30)
}
/// How long one request to a room's host may take end to end: in test
/// builds, whose fixture host shares a loaded runner with the rest of the
/// suite, the shared test hang bound.
#[cfg(test)]
fn host_timeout() -> std::time::Duration {
    tilo_test_env::hang_bound()
}

/// Check that `page` is the answer to asking `host` for `provider`'s
/// `wanted` resource in `room`: every row is a view of that resource, and
/// the locator it names, if any, is this source's.
///
/// A page that found nothing — an application filter that cannot match the
/// record's own, say — names no page to browse with, so it need not carry a
/// locator. A page carrying rows does, and one naming a locator must name
/// this source whether or not it found anything.
fn resource_view_answer(
    page: &tilo_actions::remote::CataloguePage,
    room: RoomId,
    host: &str,
    provider: &str,
    wanted: &tilo_actions::remote::ViewResourceQuery,
) -> Result<(), String> {
    match page.locator.as_ref() {
        Some(locator)
            if locator.room != room.to_string()
                || locator.host_device != host
                || locator.provider != provider =>
        {
            return Err("resource host answered outside the requested source".into());
        }
        None if !page.rows.is_empty() => {
            return Err("resource host has no public browser locator".into());
        }
        _ => {}
    }
    if page.rows.iter().any(|row| {
        row.provider != provider
            || row.resource.id != wanted.id
            || row.resource.kind != wanted.kind
            || row
                .action
                .as_ref()
                .is_none_or(|action| action.kind != tilo_actions::OperationKind::View)
    }) {
        return Err("resource host answered outside the requested source".into());
    }
    Ok(())
}

impl ShellNetwork {
    /// Discover one existing resource on the currently governed room's host,
    /// using the same authenticated remote-action transport as the palette.
    pub(crate) async fn discover_resource_view(
        &self,
        room: RoomId,
        query: tilo_actions::remote::CatalogueQuery,
    ) -> Result<tilo_actions::remote::CataloguePage, String> {
        use crate::shell::remote_action_link::RemoteActionLink as _;
        use tilo_actions::remote::RemoteActionMessage;
        let hosted = self.hosted_room(room).ok_or("no host serves this room")?;
        let wanted = query
            .view_resource
            .as_ref()
            .ok_or("missing resource view query")?
            .clone();
        let provider = query
            .provider
            .as_ref()
            .ok_or("missing resource provider")?
            .clone();
        let target = hosted.host();
        // Fresh ids do not collide with another palette's or room's counter.
        let correlation = uuid::Uuid::new_v4().as_u64_pair().0;
        let pending = self.request(
            room_topic_id(room),
            target,
            RemoteActionMessage::CatalogueRequest { correlation, query },
        )?;
        let answer = tokio::time::timeout(host_timeout(), pending)
            .await
            .map_err(|_| "resource host lookup timed out")?
            .ok_or("resource host lookup ended")?;
        let RemoteActionMessage::CatalogueResponse {
            correlation: got,
            result,
        } = answer
        else {
            return Err("resource host answered another request".into());
        };
        if got != correlation {
            return Err("resource host correlation changed".into());
        }
        let page = result.map_err(|error| error.to_string())?;
        let fresh = self.hosted_room(room).ok_or("no host serves this room")?;
        if fresh.host() != target {
            return Err("resource host or selected device changed".into());
        }
        resource_view_answer(&page, room, &hex::encode(target), &provider, &wanted)?;
        Ok(page)
    }

    /// Whether another device hosts `room`: its records are committed by
    /// that host, and this device reads them.
    pub(crate) fn governed_elsewhere(&self, room: RoomId) -> bool {
        self.hosted_room(room)
            .is_some_and(|hosted| !hosted.hosted_here())
    }

    /// This device's key for `room`, once its custody as `owner` admits it
    /// is current: what signs proposals and decisions made there.
    pub(crate) fn native_signing_key_of(
        &self,
        owner: &NativeWorkspaceOwner,
        room: RoomId,
    ) -> Result<ed25519_dalek::SigningKey, String> {
        self.check_native_custody(owner, room)?;
        self.host_identity
            .as_ref()
            .map(|identity| identity.device_signing_key.clone())
            .ok_or_else(|| "this device has no identity".into())
    }
}

#[cfg(test)]
mod tests {
    use tilo_actions::remote::{
        CataloguePage, CatalogueRow, ResourceViewLocator, ViewResourceQuery,
    };
    use tilo_actions::{
        ActionAddress, ActionContract, ActionDescriptor, ExecutionLocus, InputSpec,
    };

    use super::{RoomId, resource_view_answer};

    const PROVIDER: &str = "record:so.tilo.chat:part-1";
    const HOST: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";

    fn room() -> RoomId {
        RoomId::parse("2b2b2b2b-2b2b-2b2b-2b2b-2b2b2b2b2b2b").expect("a room id")
    }

    fn wanted() -> ViewResourceQuery {
        ViewResourceQuery {
            id: "chat.messages.messages:hello".into(),
            kind: "chat.messages.messages".into(),
        }
    }

    /// One view row for `wanted()`, as the host's answer carries it.
    fn view_row() -> CatalogueRow {
        let wanted = wanted();
        CatalogueRow::for_action(
            PROVIDER,
            "so.tilo.chat",
            ActionDescriptor {
                semantic_identity: None,
                address: ActionAddress::new(wanted.id, "open"),
                resource_kind: wanted.kind,
                resource_label: "Hello".into(),
                action_label: "Open".into(),
                keywords: Vec::new(),
                icon: None,
                description: None,
                locus: ExecutionLocus::default(),
                kind: tilo_actions::OperationKind::View,
                contract: ActionContract::view(),
                input: InputSpec::None,
                params: Vec::new(),
                view: None,
            },
        )
    }

    /// The locator the host issues for `wanted()`'s page in `room()`.
    fn locator() -> ResourceViewLocator {
        ResourceViewLocator {
            origin: "https://room.example".into(),
            room: room().to_string(),
            host_device: HOST.into(),
            provider: PROVIDER.into(),
            web_entry: None,
        }
    }

    fn answer(page: &CataloguePage) -> Result<(), String> {
        resource_view_answer(page, room(), HOST, PROVIDER, &wanted())
    }

    #[test]
    fn a_matching_app_filtered_view_keeps_its_locator() {
        let page = CataloguePage {
            revision: "r1".into(),
            rows: vec![view_row()],
            locator: Some(locator()),
            ..CataloguePage::default()
        };
        assert_eq!(answer(&page), Ok(()));
    }

    #[test]
    fn a_mismatching_app_filtered_view_is_empty_and_names_no_locator() {
        // The host narrowed to an application this record's row does not
        // publish, so it found nothing: a legitimate answer, not an error.
        let page = CataloguePage {
            revision: "r1".into(),
            ..CataloguePage::default()
        };
        assert_eq!(answer(&page), Ok(()));
    }

    #[test]
    fn an_absent_app_filter_still_requires_a_locator_for_rows() {
        let page = CataloguePage {
            revision: "r1".into(),
            rows: vec![view_row()],
            ..CataloguePage::default()
        };
        assert_eq!(
            answer(&page),
            Err("resource host has no public browser locator".into())
        );
    }

    #[test]
    fn an_empty_answer_naming_another_source_is_still_refused() {
        for claimed in [
            ResourceViewLocator {
                room: uuid::Uuid::nil().to_string(),
                ..locator()
            },
            ResourceViewLocator {
                host_device: "b2".repeat(32),
                ..locator()
            },
            ResourceViewLocator {
                provider: "record:so.tilo.notes:other".into(),
                ..locator()
            },
        ] {
            let page = CataloguePage {
                revision: "r1".into(),
                locator: Some(claimed),
                ..CataloguePage::default()
            };
            assert_eq!(
                answer(&page),
                Err("resource host answered outside the requested source".into())
            );
        }
    }

    #[test]
    fn a_row_outside_the_requested_view_is_refused() {
        let mut elsewhere = view_row();
        elsewhere.provider = "record:so.tilo.notes:other".into();
        let mut other_kind = view_row();
        other_kind.resource.kind = "notes.block".into();
        let mut other_action = view_row();
        other_action.action.as_mut().expect("a view row").kind =
            tilo_actions::OperationKind::Effect;
        for row in [elsewhere, other_kind, other_action] {
            let page = CataloguePage {
                revision: "r1".into(),
                rows: vec![row],
                locator: Some(locator()),
                ..CataloguePage::default()
            };
            assert_eq!(
                answer(&page),
                Err("resource host answered outside the requested source".into())
            );
        }
    }

    #[test]
    fn a_room_this_device_holds_alone_is_governed_here() {
        let tree = crate::shell::state::WorkspaceTree::bootstrap_demo().shared();
        let room = tree.read().desktop.workspaces[0].room();
        let (net, _commands) = super::super::shell_network_for_test(tree, 0);
        assert!(!net.governed_elsewhere(room));
    }
}
