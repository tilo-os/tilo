//! The Discord bridge's link to one server, set up by the room's host: the
//! guild record connected to that server and the room's Chat workspace, and
//! the settings the connector's worker runs with. The bridge bot keeps both
//! sides in line from there; nothing here talks to Discord.

use std::collections::BTreeMap;

use tilo_agent_participant::AppCatalogue;
use tilo_resource::{Replica, Resource as _};
use tilo_user_identity::DeviceCert;

use super::room_record;
use crate::rooms::Rooms;

/// The Discord connector's app id, whose records the bridge keeps.
pub const APP: &str = "so.tilo.discord";
const CHAT: &str = "so.tilo.chat";

/// The bridge bot inputs that name its [`DiscordLink`]: read by the host,
/// never handed to the program.
pub const LINK_INPUTS: [&str; 5] = ["guild", "channel", "server", "mirror-channels", "map-roles"];

/// Which Discord server a bridge reaches, the one channel of it when it
/// reaches only one, and what beyond messages it may change there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscordLink {
    /// The server's id.
    pub guild_id: String,
    /// The one channel bridged, when the bridge reaches only one.
    pub only_channel: Option<String>,
    /// The server's name as the record shows it.
    pub name: String,
    /// Channels made in Tilo after the server was adopted are made in
    /// Discord too. Off by default: the bridge makes no channel there.
    pub mirror_channels: bool,
    /// Tilo's roles are kept as roles the bridge owns in Discord. Off by
    /// default: the bridge changes no role or permission there.
    pub map_roles: bool,
}

impl DiscordLink {
    /// The link a bridge bot's inputs name: `guild=<id>`, `channel=<id>` to
    /// bridge only that channel, `server=<name>`, and `mirror-channels=yes`
    /// and `map-roles=yes` to opt into changing the server beyond messages
    /// and webhooks. None without `guild`.
    ///
    /// # Errors
    /// An id is not a Discord id, `channel` comes without `guild`, or an
    /// opt-in is not `yes` or `no`.
    pub fn from_inputs(inputs: &BTreeMap<String, String>) -> Result<Option<Self>, String> {
        let snowflake = |key: &str| match inputs.get(key) {
            Some(id) if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) => {
                Ok(Some(id.clone()))
            }
            Some(id) => Err(format!("{key}={id} is not a Discord id")),
            None => Ok(None),
        };
        let switch = |key: &str| match inputs.get(key).map(String::as_str) {
            None | Some("no") => Ok(false),
            Some("yes") => Ok(true),
            Some(other) => Err(format!("{key}={other}: say yes or no")),
        };
        let only_channel = snowflake("channel")?;
        let Some(guild_id) = snowflake("guild")? else {
            return match only_channel {
                Some(_) => Err("channel= bridges a channel of the server guild= names".into()),
                None => Ok(None),
            };
        };
        Ok(Some(Self {
            guild_id,
            only_channel,
            name: inputs
                .get("server")
                .cloned()
                .unwrap_or_else(|| "Discord".to_owned()),
            mirror_channels: switch("mirror-channels")?,
            map_roles: switch("map-roles")?,
        }))
    }

    /// What the connector's worker runs with: the server, so a bot in
    /// several servers acts in this one only, and the one channel, so it
    /// neither hears nor changes anything else in the server.
    #[must_use]
    pub fn worker_env(&self) -> Vec<(String, String)> {
        std::iter::once(("DISCORD_GUILD_ID".to_owned(), self.guild_id.clone()))
            .chain(
                self.only_channel
                    .iter()
                    .map(|channel| ("DISCORD_CHANNEL_ID".to_owned(), channel.clone())),
            )
            .collect()
    }
}

/// Connect the room's guild record to `link`'s server and the room's Chat
/// workspace, as the room's owner (`host`, the room's governor): connecting
/// a server is a person's decision, and in a hosted room the operator who
/// named the server is that person. The workspace is the room's own (the one
/// its host names after the room), else its first, else a new one named
/// `workspace_name`. A record already connected the same way is left as it
/// is.
///
/// # Errors
/// The record is connected to another server or channel, or the room
/// refused a change.
pub async fn connect(
    rooms: &Rooms,
    room: &str,
    host: &DeviceCert,
    link: &DiscordLink,
    workspace_name: &str,
) -> Result<(), String> {
    let catalogue = AppCatalogue::native();
    let owner = hex::encode(host.user_public_key);
    let (chat_record, guild_record) = (room_record(&catalogue, CHAT), room_record(&catalogue, APP));
    rooms
        .prepare_application(room, host, &catalogue, APP, &guild_record)
        .await
        .map_err(|error| format!("add the Discord record: {error}"))?;
    let guild = rooms
        .record_document(room, APP, &guild_record)
        .await
        .map_err(|error| error.to_string())?
        .and_then(|bytes| automerge::AutoCommit::load(&bytes).ok())
        .and_then(|doc| discord::Guild::read(&doc).ok())
        .ok_or("the Discord record does not read")?;
    if !guild.guild_id.is_empty() {
        let connected = DiscordLink {
            guild_id: guild.guild_id.clone(),
            only_channel: guild.only_channel.clone(),
            name: link.name.clone(),
            mirror_channels: guild.mirror_channels,
            map_roles: guild.map_roles,
        };
        return if connected == *link {
            Ok(())
        } else {
            Err(format!(
                "the room's bridge is connected as {connected:?}, not {link:?}; a bridge is connected once"
            ))
        };
    }
    rooms
        .prepare_application(room, host, &catalogue, CHAT, &chat_record)
        .await
        .map_err(|error| format!("add Chat: {error}"))?;
    let workspace = rooms
        .author_application(room, host, &catalogue, CHAT, &chat_record, {
            let (owner, name, own) = (
                owner.clone(),
                workspace_name.to_owned(),
                tilo_resource::Id::from_raw(room),
            );
            move |doc| {
                let first = |doc: &automerge::AutoCommit| {
                    chat::Chat::read(doc).ok().and_then(|chat| {
                        chat.workspaces
                            .get(&own)
                            .map(|_| own.as_str().to_owned())
                            .or_else(|| {
                                chat.workspaces
                                    .iter()
                                    .next()
                                    .map(|(id, _)| id.as_str().to_owned())
                            })
                    })
                };
                if let Some(workspace) = first(doc) {
                    return Ok(workspace);
                }
                apply_as_owner::<chat::Chat>(
                    doc,
                    &owner,
                    (CHAT, "Chat"),
                    chat::Chat::CREATE_WORKSPACE.with(chat::record::chat::CreateWorkspace { name }),
                )?;
                first(doc).ok_or_else(|| "the new workspace does not read".to_owned())
            }
        })
        .await
        .map_err(|error| format!("find the room's Chat workspace: {error}"))?;
    let connect = discord::Guild::CONNECT.with(discord::record::guild::Connect {
        guild_id: link.guild_id.clone(),
        name: link.name.clone(),
        workspace,
        only_channel: link.only_channel.clone(),
        mirror_channels: Some(link.mirror_channels),
        map_roles: Some(link.map_roles),
    });
    rooms
        .author_application(room, host, &catalogue, APP, &guild_record, move |doc| {
            apply_as_owner::<discord::Guild>(doc, &owner, (APP, "Discord"), connect)
        })
        .await
        .map_err(|error| format!("connect the Discord record: {error}"))
}

/// Apply `invoke` to `doc` as `owner` acting in the app `(id, name)`,
/// confirmed as a person confirms it.
fn apply_as_owner<R: tilo_resource::Resource>(
    doc: &mut automerge::AutoCommit,
    owner: &str,
    (app, name): (&str, &str),
    invoke: tilo_resource::Invoke<R>,
) -> Result<(), String> {
    let mut replica = Replica::<R>::from_doc(std::mem::take(doc))
        .as_invoker(owner)
        .in_app(app, name);
    let applied = replica.apply_confirmed(invoke).map(drop);
    std::mem::swap(doc, replica.doc_mut());
    applied.map_err(|error| error.to_string())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard"
)]
mod tests {
    use super::*;

    fn inputs(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn a_bridge_names_its_server_and_maybe_one_channel_in_its_inputs() {
        assert_eq!(DiscordLink::from_inputs(&inputs(&[])).unwrap(), None);
        let link = DiscordLink::from_inputs(&inputs(&[("guild", "12"), ("channel", "34")]))
            .unwrap()
            .unwrap();
        assert_eq!(
            link.worker_env(),
            [
                ("DISCORD_GUILD_ID".to_owned(), "12".to_owned()),
                ("DISCORD_CHANNEL_ID".to_owned(), "34".to_owned())
            ]
        );
        assert!(
            !link.mirror_channels && !link.map_roles,
            "opt-ins default off"
        );
        let opted = DiscordLink::from_inputs(&inputs(&[
            ("guild", "12"),
            ("mirror-channels", "yes"),
            ("map-roles", "yes"),
        ]))
        .unwrap()
        .unwrap();
        assert!(opted.mirror_channels && opted.map_roles);
        assert!(DiscordLink::from_inputs(&inputs(&[("guild", "12"), ("map-roles", "y")])).is_err());
        assert!(DiscordLink::from_inputs(&inputs(&[("guild", "x")])).is_err());
        assert!(DiscordLink::from_inputs(&inputs(&[("channel", "34")])).is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_host_connects_the_room_to_the_server_its_bridge_names_once() {
        let scratch = tempfile::tempdir().unwrap();
        let (key, cert) = crate::rooms::tests::governor();
        let relay = crate::rooms::tests::view_read_relay(scratch.path(), &key, &cert).await;
        let rooms = relay.rooms().unwrap().clone();
        let room = rooms.create("Plaza").await.unwrap().room;
        let link = DiscordLink::from_inputs(&inputs(&[("guild", "12"), ("channel", "34")]))
            .unwrap()
            .unwrap();
        connect(&rooms, &room, &cert, &link, "Plaza").await.unwrap();
        let catalogue = AppCatalogue::native();
        let read = |app: &'static str| {
            let (rooms, room, record) = (rooms.clone(), room.clone(), room_record(&catalogue, app));
            async move {
                automerge::AutoCommit::load(
                    &rooms
                        .record_document(&room, app, &record)
                        .await
                        .unwrap()
                        .unwrap(),
                )
                .unwrap()
            }
        };
        let guild = discord::Guild::read(&read(APP).await).unwrap();
        let chat = chat::Chat::read(&read(CHAT).await).unwrap();
        assert_eq!(
            (guild.guild_id.as_str(), guild.only_channel.as_deref()),
            ("12", Some("34"))
        );
        let (workspace, _) = chat.workspaces.iter().next().unwrap();
        assert_eq!(guild.workspace, workspace.as_str());

        // A restart finds it connected; another server is refused.
        connect(&rooms, &room, &cert, &link, "Plaza").await.unwrap();
        let other = DiscordLink {
            guild_id: "56".into(),
            ..link
        };
        assert!(
            connect(&rooms, &room, &cert, &other, "Plaza")
                .await
                .is_err()
        );
    }
}
