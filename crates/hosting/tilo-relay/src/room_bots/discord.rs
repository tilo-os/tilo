//! The Discord bridge's link to one server, set up by the room's host: the
//! guild record connected to that server and mirrored in the room's Chat, and
//! the settings the connector's worker runs with. The bridge bot keeps both
//! sides in line from there; nothing here talks to Discord.

use std::collections::BTreeMap;

use tilo_agent_participant::AppCatalogue;
use tilo_resource::Replica;
use tilo_user_identity::DeviceCert;

use super::room_record;
use crate::rooms::Rooms;

/// The Discord connector's app id, whose records the bridge keeps.
pub const APP: &str = "so.tilo.discord";
/// The secret the connector's worker reads its bot token from.
pub const TOKEN_PURPOSE: &str = "bot-token";
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

/// Connect the room's guild record to `link`'s server, mirrored in the
/// room's Chat, as the room's owner (`host`, the room's governor):
/// connecting a server is a person's decision, and in a hosted room the
/// operator who named the server is that person. A record already connected
/// the same way is left as it is.
pub async fn connect(
    rooms: &Rooms,
    room: &str,
    host: &DeviceCert,
    link: &DiscordLink,
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
        .ok_or("the Discord record is missing")?;
    // A record an older relay or older devices wrote reads once migrated.
    let guild = automerge::AutoCommit::load(&guild)
        .map_err(|error| format!("the Discord record does not load: {error}"))
        .and_then(|mut doc| super::opened::<discord::Guild>(&mut doc))
        .map_err(|error| format!("the Discord record does not read: {error}"))?;
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
    let connect = discord::Guild::CONNECT.with(discord::record::guild::Connect {
        guild_id: link.guild_id.clone(),
        name: link.name.clone(),
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
pub(super) fn apply_as_owner<R: tilo_resource::Resource>(
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

/// Connect the room to the Discord server `inputs` name (`guild=`, and
/// `server=`, `channel=`, `mirror-channels=`, `map-roles=`), and keep that
/// link with the host: the link inputs leave the bot's own (the program
/// never reads them), the bot's posts point at `avatars_url` unless its
/// inputs say otherwise, and the worker's settings come back. The room's
/// operator naming the bridge on the command line and a member asking for
/// it in the room both come here.
///
/// # Errors
/// No server is named (a bridge without one would act in every server the
/// bot is in), an input is malformed, or the room refused the link.
pub async fn link_bridge(
    rooms: &Rooms,
    room: &str,
    host: &DeviceCert,
    inputs: &BTreeMap<String, String>,
    bot: &mut super::installed::InstalledBot,
    avatars_url: Option<&str>,
) -> Result<Vec<(String, String)>, String> {
    let link =
        DiscordLink::from_inputs(inputs)?.ok_or("a Discord bridge needs guild=<server id>")?;
    connect(rooms, room, host, &link).await?;
    for key in LINK_INPUTS {
        bot.inputs.remove(key);
    }
    if let (false, Some(url)) = (bot.inputs.contains_key("context"), avatars_url) {
        bot.inputs.insert(
            "context".to_owned(),
            serde_json::json!({ "avatars": format!("{}/", url.trim_end_matches('/')) }),
        );
    }
    Ok(link.worker_env())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard"
)]
mod tests {
    use super::*;
    use tilo_resource::Resource as _;

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
        let (key, cert) = crate::rooms::fixtures::governor();
        let relay = crate::rooms::fixtures::test_host(scratch.path(), &key, &cert).await;
        let rooms = relay.rooms().clone();
        let room = rooms.create("Plaza").await.unwrap().room;
        let link = DiscordLink::from_inputs(&inputs(&[("guild", "12"), ("channel", "34")]))
            .unwrap()
            .unwrap();
        connect(&rooms, &room, &cert, &link).await.unwrap();
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
        assert!(
            chat::Chat::read(&read(CHAT).await).is_ok(),
            "the room's Chat is added for the bridge to mirror"
        );
        assert_eq!(
            (guild.guild_id.as_str(), guild.only_channel.as_deref()),
            ("12", Some("34"))
        );

        // A restart finds it connected; another server is refused.
        connect(&rooms, &room, &cert, &link).await.unwrap();
        let other = DiscordLink {
            guild_id: "56".into(),
            ..link
        };
        assert!(connect(&rooms, &room, &cert, &other).await.is_err());
    }
}
