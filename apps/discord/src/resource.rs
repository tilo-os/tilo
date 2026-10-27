//! The connector as an app: one bridged server per record. People see which
//! server it is, how far the bridge got, the channels it carries and who
//! linked their account — their own with the way to link or unlink it — and
//! everything else is the bridge bot's.

use ui::i18n::{ArgValue, EmbeddedCatalog};
use ui::icons::Icon;
use ui::prelude::*;
use ui::widget::primitives::{Fact, Facts};
use ui::widget::{ListRow, Section};

use discord_wire::op::ChannelKind;

use crate::effects::{self, DiscordEffect};
use crate::locales::CATALOG;
use crate::record::{Guild, Link, Ownership};
use crate::setup::{self, Setup, SetupDoor};

/// A channel of the server, as the page lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Channel {
    id: String,
    name: String,
    kind: ChannelKind,
    parent: Option<String>,
    position: i64,
}

/// Someone who linked their Discord account here: the account, as Discord
/// names it. The room's own id for the person stays in the record.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Linked {
    discord_user: String,
    user_name: String,
    display_name: Option<String>,
    /// Whether this is the person reading.
    mine: bool,
}

/// What the view is built from: whether a server is bridged, how far the
/// bridge got, and what it holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Shown {
    connected: bool,
    importing: bool,
    /// The server's channels, categories and their children in the order
    /// Discord shows them.
    channels: Vec<Channel>,
    /// Everyone who linked a Discord account here, the reader among them.
    links: Vec<Linked>,
    /// Whether the reader linked an account of their own.
    mine: bool,
    /// Whether the room's host offers signing in with Discord, so the page
    /// can offer the button that starts it.
    sign_in: bool,
    /// Whether the bridge mirrors the whole server, where `/link` works.
    whole_server: bool,
    /// Why calls do not reach Discord, as the bridge last heard it.
    voice_problem: Option<String>,
    /// Why the workspace's emoji do not reach the server, likewise.
    emoji_problem: Option<String>,
}

impl Shown {
    fn of(guild: &Guild, me: Option<&str>) -> Self {
        Self {
            connected: !guild.guild_id.is_empty(),
            importing: guild.ownership == Ownership::Importing,
            channels: ordered(guild),
            links: guild
                .links
                .iter()
                .map(|link| Linked::of(link, me))
                .collect(),
            mine: me.is_some_and(|me| guild.links.iter().any(|link| link.person == me)),
            sign_in: guild.sign_in_page.is_some(),
            whole_server: guild.only_channel.is_none(),
            voice_problem: guild.voice_problem.clone(),
            emoji_problem: guild.emoji_problem.clone(),
        }
    }
}

impl Linked {
    /// The name Discord shows for the account, falling back to its handle.
    fn name(&self) -> String {
        self.display_name
            .clone()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.user_name.clone())
    }

    fn of(link: &Link, me: Option<&str>) -> Self {
        Self {
            discord_user: link.discord_user.clone(),
            user_name: link.user_name.clone(),
            display_name: link.display_name.clone(),
            mine: me.is_some_and(|me| me == link.person),
        }
    }
}

/// Discord's channels in the order it shows them: each category followed by
/// its own channels, then the channels that belong to no category, each by
/// its position. A channel whose category the bridge has not read yet is
/// still shown, after the ones it has.
fn ordered(guild: &Guild) -> Vec<Channel> {
    let mut channels: Vec<Channel> = guild
        .channels
        .iter()
        .map(|channel| Channel {
            id: channel.id.clone(),
            name: channel.name.clone(),
            kind: channel.kind,
            parent: channel.parent.clone(),
            position: channel.position,
        })
        .collect();
    let by_position = |channel: &Channel| (channel.position, channel.name.clone());
    let mut top: Vec<Channel> = channels
        .iter()
        .filter(|channel| channel.parent.is_none())
        .cloned()
        .collect();
    top.sort_by_key(by_position);
    let mut rows: Vec<Channel> = Vec::new();
    for item in top {
        if item.kind == ChannelKind::Category {
            let mut children: Vec<Channel> = channels
                .iter()
                .filter(|channel| channel.parent.as_deref() == Some(item.id.as_str()))
                .cloned()
                .collect();
            children.sort_by_key(by_position);
            rows.push(item);
            rows.extend(children);
        } else {
            rows.push(item);
        }
    }
    let mut placed: Vec<&str> = rows.iter().map(|channel| channel.id.as_str()).collect();
    placed.sort_unstable();
    let mut orphans: Vec<Channel> = channels
        .drain(..)
        .filter(|channel| placed.binary_search(&channel.id.as_str()).is_err())
        .collect();
    orphans.sort_by_key(by_position);
    rows.extend(orphans);
    rows
}

fn count(n: usize) -> ArgValue {
    ArgValue::Number(f64::from(u32::try_from(n).unwrap_or(u32::MAX)))
}

/// Where the bridge stands, in a sentence.
fn status(shown: &Shown) -> String {
    CATALOG.t(if shown.importing {
        "discord-status-importing"
    } else {
        "discord-status-bridged"
    })
}

/// How much the bridge carries, as separate facts. The counts are the first
/// to go when the row is narrow: the lists below them say the same thing, and
/// a problem is never left to compete for that space.
fn facts(shown: &Shown) -> Vec<Fact> {
    vec![
        Fact::with_icon(
            Icon::Hash,
            CATALOG.t_args(
                "discord-channels",
                &[("count", count(shown.channels.len()))],
            ),
        )
        .priority(1),
        Fact::with_icon(
            Icon::Users,
            CATALOG.t_args(
                "discord-linked-accounts",
                &[("count", count(shown.links.len()))],
            ),
        )
        .priority(1),
    ]
}

/// Why calls or emoji do not reach Discord, said in its own line: a fact in
/// a row that drops facts when the window is narrow is a state that goes
/// unspoken exactly where it is needed.
fn problems(shown: &Shown) -> Vec<Element> {
    [
        (
            shown.voice_problem.as_ref(),
            CATALOG.t("discord-voice-problem"),
        ),
        (
            shown.emoji_problem.as_ref(),
            CATALOG.t("discord-emoji-problem"),
        ),
    ]
    .into_iter()
    .filter_map(|(reason, title)| {
        let reason = reason?;
        Some(Box::new(Banner::warning(title).with_message(reason.clone())) as Element)
    })
    .collect()
}

/// The icon and the words of a channel's kind: the leading mark of its row,
/// and what that mark stands for.
fn kind(channel: &Channel) -> (Icon, String) {
    let icon = match channel.kind {
        ChannelKind::Text => Icon::Hash,
        ChannelKind::Voice => Icon::Volume2,
        ChannelKind::Category => Icon::Folder,
        ChannelKind::Thread => Icon::MessageSquare,
    };
    let words = match channel.kind {
        ChannelKind::Text => CATALOG.t("discord-channel-text"),
        ChannelKind::Voice => CATALOG.t("discord-channel-voice"),
        ChannelKind::Category => CATALOG.t("discord-channel-category"),
        ChannelKind::Thread => CATALOG.t("discord-channel-thread"),
    };
    (icon, words)
}

/// Before a server is bridged: what the app is, and bridging one.
fn not_connected(setup: &Setup) -> Element {
    let setup = setup.clone();
    Box::new(
        EmptyState::new(CATALOG.t("discord-empty-title"))
            .with_id("guild-status")
            .with_icon(Icon::MessageSquare)
            .with_message(CATALOG.t("discord-empty-body"))
            .with_action(CATALOG.t("discord-bridge-server"))
            .with_action_id("discord-bridge-server")
            .on_view(move || setup.open()),
    )
}

/// The server's channels, as rows: what the bridge carries, in Discord's own
/// order, so a channel a person cannot find in Chat can be found here.
fn channels(shown: &Shown) -> Element {
    let rows: Vec<Element> = if shown.channels.is_empty() {
        vec![Box::new(Text::new(&CATALOG.t("discord-channels-empty")).dimmed()) as Element]
    } else {
        shown
            .channels
            .iter()
            .map(|channel| {
                let (icon, kind) = kind(channel);
                Box::new(
                    ListRow::new(format!("discord-channel-{}", channel.id), &channel.name)
                        .icon(icon)
                        .leading_tooltip(kind),
                ) as Element
            })
            .collect()
    };
    Box::new(Section::titled(CATALOG.t("discord-channels-title"), rows))
}

/// Everyone who linked an account, the reader first: their own row says
/// whether they linked one and carries the way in or out of it. A room that
/// links accounts only from the server's side says so, once, rather than
/// offering a control it cannot honour.
fn accounts(cx: &ViewCx<Guild>, shown: &Shown) -> Element {
    let mut rows: Vec<Element> = Vec::new();
    if let Some(mine) = shown.links.iter().find(|link| link.mine) {
        let unlink = cx.actions().bind_effect_invocation(
            Button::new(
                "discord-unlink-account",
                &CATALOG.t("discord-unlink-account"),
            ),
            &DiscordEffect::UnlinkMine,
        );
        rows.push(Box::new(
            ListRow::new(
                format!("discord-account-{}", mine.discord_user),
                &mine.name(),
            )
            .icon(Icon::Discord)
            .subtitle(format!("@{}", mine.user_name))
            .secondary(CATALOG.t("discord-you"))
            .trailing(Box::new(unlink)),
        ));
    } else {
        let mut row = ListRow::new("discord-my-account", &CATALOG.t("discord-you"))
            .icon(Icon::Discord)
            .subtitle(CATALOG.t("discord-not-linked"));
        if shown.sign_in {
            let connect = cx.actions().bind_effect_invocation(
                Button::new(
                    "discord-connect-account",
                    &CATALOG.t("discord-connect-account"),
                ),
                &DiscordEffect::ConnectAccount,
            );
            row = row.trailing(Box::new(connect));
        }
        rows.push(Box::new(row));
    }
    for link in shown.links.iter().filter(|link| !link.mine) {
        rows.push(Box::new(
            ListRow::new(
                format!("discord-account-{}", link.discord_user),
                &link.name(),
            )
            .icon(Icon::Discord)
            .subtitle(format!("@{}", link.user_name)),
        ));
    }
    if !shown.mine && !shown.sign_in {
        rows.push(Box::new(Text::new(&CATALOG.t(if shown.whole_server {
            "discord-link-elsewhere"
        } else {
            "discord-link-unavailable"
        }))));
    }
    Box::new(Section::titled(CATALOG.t("discord-accounts-title"), rows))
}

/// A page of this app's own: a readable column in a scrollable window, so a
/// server with many channels, and a view at a large text scale, are both
/// still reachable.
fn page(parts: Vec<Element>) -> Element {
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Group);
    for part in parts {
        column = column.push(part);
    }
    Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_child(Box::new(
                Container::new()
                    .readable()
                    .centered()
                    .with_inset(ui::tokens::Rhythm::Group)
                    .with_child(Box::new(column)),
            )),
    )
}

/// The app.
pub struct Discord;

impl ResourceApp for Discord {
    type Resource = Guild;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = "Discord";
    // A room bridges one server, so its guild record is the room's own.
    const ROOM_RECORD: bool = true;

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    /// A bridged server cannot be staged by a capture: the bridge observes
    /// it from Discord, so the scene is written as the record's start.
    fn scenarios() -> &'static [&'static str] {
        crate::sample::SCENARIOS
    }

    fn fixture(fixture: &mut Fixture<Guild>) {
        let Some(scenario) = fixture.scenario() else {
            return;
        };
        let scenario = scenario.to_owned();
        let person = fixture.person().to_owned();
        fixture.start(crate::sample::guild(Some(&scenario), &person));
    }

    fn view(cx: &mut ViewCx<Guild>) -> Element {
        let bridging = Setup::new();
        let answering = bridging.clone();
        cx.effects::<DiscordEffect>(move |effect, ecx| {
            if let Some(answer) = setup::answer(&answering, &effect, ecx) {
                return answer;
            }
            effects::answer(effect, ecx)
        });
        let me = cx.me().map(str::to_owned);
        let shown = cx.value(move |guild: &Guild| Shown::of(guild, me.as_deref()));
        let offered = bridging.clone();
        let page = cx.switch(&shown, move |cx, shown| {
            if !shown.connected {
                return not_connected(&offered);
            }
            let name = cx
                .text(|g: &Guild| g.name.clone())
                .with_id("guild-name")
                .with_role(TextRole::Title);
            let summary = Text::new(&status(shown)).with_id("guild-status");
            let mut parts: Vec<Element> = vec![
                Box::new(name),
                Box::new(summary),
                Box::new(Facts::new(facts(shown))),
            ];
            parts.extend(problems(shown));
            parts.push(accounts(cx, shown));
            parts.push(channels(shown));
            page(parts)
        });
        let sheet = setup::sheet(cx, &bridging);
        Box::new(SetupDoor::new(page, sheet, &bridging))
    }
}

ui::dev_reload_entry!(Discord);
