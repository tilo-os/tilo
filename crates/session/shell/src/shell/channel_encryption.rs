//! End-to-end encryption of collaboration channels on this device. The
//! service is `tilo_action_broker::channel_encryption`'s; its governance
//! messages travel over this session's room transport.
pub use tilo_action_broker::channel_encryption::*;

/// The handle of this device's channel keys for the room host this session
/// runs: the host holds no keys of its own but takes part in a room's
/// channels as this device's one peer, asking the session's channel service
/// for them ([`tilo_room_host::rooms::ChannelKeys`]), so one leaf of the
/// device signs in every group.
pub(crate) fn host_device_keys(keys: &ChannelKeyProvider) -> tilo_room_host::rooms::DeviceKeys {
    struct SessionKeys(ChannelKeyProvider);

    impl tilo_room_host::rooms::ChannelKeys for SessionKeys {
        fn channel_keys<'a>(
            &'a self,
            room: &'a str,
            route: &'a collab_types::ChannelRoute,
            create: bool,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Option<Result<channel_crypto::ChannelContentKeys, String>>,
                    > + Send
                    + 'a,
            >,
        > {
            let keys = self.0.clone();
            let (room, route) = (room.to_owned(), route.clone());
            Box::pin(async move {
                // Asked off the host's task: the service takes its lock and
                // reads its state on that thread.
                let asked = tokio::task::spawn_blocking(move || {
                    let Some(access) = keys.get() else {
                        return Some(Err("this session's channel service is not running".into()));
                    };
                    let room = super::state::RoomId::parse(&room)?;
                    if create {
                        match access.access_record(room, &route).0 {
                            ChannelAccess::Ready(keys) => Some(Ok(keys)),
                            ChannelAccess::Waiting | ChannelAccess::Removed => {
                                Some(Err("the channel's keys are not ready".into()))
                            }
                            ChannelAccess::NoSecretStore => Some(Err(
                                "this device has no secret store, so it holds no channel keys"
                                    .into(),
                            )),
                            ChannelAccess::Unavailable => None,
                        }
                    } else {
                        access.record_keys(room, &route)
                    }
                })
                .await;
                asked.unwrap_or_else(|error| Some(Err(error.to_string())))
            })
        }
    }

    std::sync::Arc::new(SessionKeys(keys.clone()))
}

impl GovernanceTransport for super::network::ShellNetwork {
    fn publish(&self, message: neighbour_wire::NeighbourMsg) -> bool {
        self.publish_channel_governance(message)
    }

    fn take_inbound(&self) -> Vec<InboundChannelGovernance> {
        self.inbound_channel_governance_handle()
            .lock()
            .map(|mut inbound| std::mem::take(&mut *inbound))
            .unwrap_or_default()
    }
}
