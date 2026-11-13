//! The Discord bridge's link to one server, set up by the workspace's host: the
//! guild record connected to that server and mirrored in the workspace's Chat, and
//! the settings the connector's worker runs with. The bridge bot keeps both
//! sides in line from there; nothing here talks to Discord.

use std::collections::BTreeMap;

use tilo_agent_participant::AppCatalogue;
use tilo_resource::Replica;
use tilo_user_identity::DeviceCert;

use super::workspace_record;
use crate::workspaces::Workspaces;

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

/// Connect the workspace's guild record to `link`'s server, mirrored in the
/// workspace's Chat, as the workspace's owner (`host`, the workspace's governor):
/// connecting a server is a person's decision, and in a hosted workspace the
/// operator who named the server is that person. A record already connected
/// the same way is left as it is.
pub async fn connect(
    workspaces: &Workspaces,
    workspace: &str,
    host: &DeviceCert,
    link: &DiscordLink,
) -> Result<(), String> {
    let catalogue = AppCatalogue::native();
    let owner = hex::encode(host.user_public_key);
    let (chat_record, guild_record) = (
        workspace_record(&catalogue, CHAT),
        workspace_record(&catalogue, APP),
    );
    workspaces
        .prepare_application(workspace, host, &catalogue, APP, &guild_record)
        .await
        .map_err(|error| format!("add the Discord record: {error}"))?;
    let guild = workspaces
        .record_document(workspace, APP, &guild_record)
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
                "the workspace's bridge is connected as {connected:?}, not {link:?}; a bridge is connected once"
            ))
        };
    }
    workspaces
        .prepare_application(workspace, host, &catalogue, CHAT, &chat_record)
        .await
        .map_err(|error| format!("add Chat: {error}"))?;
    let connect = discord::Guild::CONNECT.with(discord::record::guild::Connect {
        guild_id: link.guild_id.clone(),
        name: link.name.clone(),
        only_channel: link.only_channel.clone(),
        mirror_channels: Some(link.mirror_channels),
        map_roles: Some(link.map_roles),
    });
    workspaces
        .author_application(
            workspace,
            host,
            &catalogue,
            APP,
            &guild_record,
            move |doc| apply_as_owner::<discord::Guild>(doc, &owner, (APP, "Discord"), connect),
        )
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

/// Connect the workspace to the Discord server `inputs` name (`guild=`, and
/// `server=`, `channel=`, `mirror-channels=`, `map-roles=`), and keep that
/// link with the host: the link inputs leave the bot's own (the program
/// never reads them), the bot's posts point at `avatars_url` unless its
/// inputs say otherwise, and the worker's settings come back. The workspace's
/// operator naming the bridge on the command line and a member asking for
/// it in the workspace both come here.
///
/// # Errors
/// No server is named (a bridge without one would act in every server the
/// bot is in), an input is malformed, or the workspace refused the link.
pub async fn link_bridge(
    workspaces: &Workspaces,
    workspace: &str,
    host: &DeviceCert,
    inputs: &BTreeMap<String, String>,
    bot: &mut super::installed::InstalledBot,
    served: &Served,
) -> Result<Vec<(String, String)>, String> {
    let link =
        DiscordLink::from_inputs(inputs)?.ok_or("a Discord bridge needs guild=<server id>")?;
    connect(workspaces, workspace, host, &link).await?;
    for key in LINK_INPUTS {
        bot.inputs.remove(key);
    }
    if !bot.inputs.contains_key("context")
        && let Some(context) = served.context(workspace, host)
    {
        bot.inputs.insert("context".to_owned(), context);
    }
    let mut env = link.worker_env();
    // `/link` answers with a button into the workspace's web entry.
    if let Some(entry) = &served.web_entry {
        env.push(("DISCORD_LINK_URL".to_owned(), entry.clone()));
    }
    env.extend(api_env(served.discord_api_base.as_deref()));
    Ok(env)
}

/// Discord's own API origin: the worker reaches it with no setting at all.
const DISCORD: &str = "https://discord.com";

/// The worker settings that point the bridge at the Discord API `base` names,
/// `base` being the host's `--discord-api-base` — an origin such as
/// `http://127.0.0.1:8090`, the shape the sign-in appends `/api/v10` to. The
/// REST base under that origin and the CDN beside it, and, for an origin on
/// this machine, the override that lets the worker reach loopback at all (a
/// fake Discord). `None`, and Discord's own origin, give nothing: the worker's
/// own defaults.
#[must_use]
pub fn api_env(base: Option<&str>) -> Vec<(String, String)> {
    let Some(base) = base.map(str::trim).filter(|base| !base.is_empty()) else {
        return Vec::new();
    };
    let base = base.trim_end_matches('/');
    if base == DISCORD {
        return Vec::new();
    }
    let mut env = vec![
        ("DISCORD_API_BASE".to_owned(), format!("{base}/api/v10")),
        ("DISCORD_CDN_BASE".to_owned(), format!("{base}/cdn")),
    ];
    if loopback(base) {
        env.push(("DISCORD_ALLOW_LOOPBACK".to_owned(), "1".to_owned()));
    }
    env
}

/// Whether the origin `base` names this machine.
fn loopback(base: &str) -> bool {
    use std::net::IpAddr;
    let authority = base
        .split_once("://")
        .map_or(base, |(_, authority)| authority);
    let authority = authority.split('/').next().unwrap_or_default();
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = match host.strip_prefix('[') {
        Some(address) => address.split(']').next().unwrap_or_default(),
        None => host.split(':').next().unwrap_or_default(),
    };
    host == "localhost"
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

/// What the bridge's worker is told about the host it runs beside: the public
/// URLs its posts point at, and the Discord API the deployment named where it
/// is not Discord's own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Served {
    /// The public URL of the avatars the bridge publishes.
    pub avatars_url: Option<String>,
    /// The web entry the browser bundle is served under (`https://…/`),
    /// when the host serves one.
    pub web_entry: Option<String>,
    /// The host's browser origin.
    pub origin: Option<String>,
    /// The `--discord-api-base` origin the host was given, when the
    /// deployment points the workspace at another Discord than Discord's own
    /// (a fake one in a test stack), so the bridge's worker reaches the same
    /// one the host's sign-in does.
    pub discord_api_base: Option<String>,
}

impl Served {
    /// The bridge's `context` input: where its posts' avatars and the links
    /// of shared resources point, for the workspace `workspace` its host `host`
    /// governs. `None` when the host serves neither.
    #[must_use]
    pub fn context(&self, workspace: &str, host: &DeviceCert) -> Option<serde_json::Value> {
        let mut context = serde_json::Map::new();
        if let Some(url) = &self.avatars_url {
            context.insert(
                "avatars".into(),
                format!("{}/", url.trim_end_matches('/')).into(),
            );
        }
        // A shared resource opens on the web where `share-link` would send
        // its reader: the workspace's resource under the host's web entry.
        if let (Some(entry), Some(origin)) = (&self.web_entry, &self.origin) {
            context.insert(
                "links".into(),
                serde_json::json!({
                    "web_entry": entry,
                    "origin": origin,
                    "workspace": workspace,
                    "host_device": hex::encode(host.device_public_key),
                }),
            );
        }
        (!context.is_empty()).then_some(serde_json::Value::Object(context))
    }
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

    /// The bridge's posts point at the avatars the host publishes and at
    /// the workspace's resources on its web entry, and at neither when it
    /// serves neither.
    #[test]
    fn the_bridge_context_names_what_the_host_serves() {
        let (_, cert) = crate::workspaces::fixtures::governor();
        assert_eq!(Served::default().context("lounge", &cert), None);
        let served = Served {
            avatars_url: Some("https://rooms.example/avatars".into()),
            web_entry: Some("https://tilo.example/".into()),
            origin: Some("https://host.example:9764".into()),
            ..Served::default()
        };
        assert_eq!(
            served.context("lounge", &cert),
            Some(serde_json::json!({
                "avatars": "https://rooms.example/avatars/",
                "links": {
                    "web_entry": "https://tilo.example/",
                    "origin": "https://host.example:9764",
                    "workspace": "lounge",
                    "host_device": hex::encode(cert.device_public_key),
                },
            }))
        );
        let without_web = Served {
            web_entry: None,
            ..served
        };
        assert!(
            without_web
                .context("lounge", &cert)
                .unwrap()
                .get("links")
                .is_none()
        );
    }

    /// A deployment that names another Discord hands the bridge's worker the
    /// REST and CDN bases under that origin — the values a fake Discord prints
    /// — and lets it reach loopback only when the origin is this machine.
    #[test]
    fn a_named_api_base_points_the_bridge_worker_at_it() {
        let env = |base: Option<&str>| -> BTreeMap<String, String> {
            api_env(base).into_iter().collect()
        };
        assert!(env(None).is_empty(), "production reaches Discord's own API");
        assert!(env(Some("")).is_empty());
        assert!(env(Some("https://discord.com")).is_empty());
        assert!(env(Some("https://discord.com/")).is_empty());
        assert_eq!(
            env(Some("http://127.0.0.1:8090")),
            inputs(&[
                ("DISCORD_API_BASE", "http://127.0.0.1:8090/api/v10"),
                ("DISCORD_CDN_BASE", "http://127.0.0.1:8090/cdn"),
                ("DISCORD_ALLOW_LOOPBACK", "1"),
            ])
        );
        assert_eq!(
            env(Some("http://127.0.0.1:8090/")),
            env(Some("http://127.0.0.1:8090"))
        );
        let loopback = |base: &str| env(Some(base)).contains_key("DISCORD_ALLOW_LOOPBACK");
        assert!(loopback("http://localhost:8090"));
        assert!(loopback("http://[::1]:8090"));
        assert!(!loopback("https://discord.test"));
        assert!(!loopback("http://10.0.0.5:8090"));
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
        let (key, cert) = crate::workspaces::fixtures::governor();
        let relay = crate::workspaces::fixtures::test_host(scratch.path(), &key, &cert).await;
        let workspaces = relay.workspaces().clone();
        let workspace = workspaces.create("Plaza").await.unwrap().workspace;
        let link = DiscordLink::from_inputs(&inputs(&[("guild", "12"), ("channel", "34")]))
            .unwrap()
            .unwrap();
        connect(&workspaces, &workspace, &cert, &link)
            .await
            .unwrap();
        let catalogue = AppCatalogue::native();
        let read = |app: &'static str| {
            let (workspaces, workspace, record) = (
                workspaces.clone(),
                workspace.clone(),
                workspace_record(&catalogue, app),
            );
            async move {
                automerge::AutoCommit::load(
                    &workspaces
                        .record_document(&workspace, app, &record)
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
            "the workspace's Chat is added for the bridge to mirror"
        );
        assert_eq!(
            (guild.guild_id.as_str(), guild.only_channel.as_deref()),
            ("12", Some("34"))
        );

        // A restart finds it connected; another server is refused.
        connect(&workspaces, &workspace, &cert, &link)
            .await
            .unwrap();
        let other = DiscordLink {
            guild_id: "56".into(),
            ..link
        };
        assert!(
            connect(&workspaces, &workspace, &cert, &other)
                .await
                .is_err()
        );
    }
}
