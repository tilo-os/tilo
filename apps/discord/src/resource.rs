//! The connector as an app: one bridged server per record. People see
//! which server it is, how far the bridge got and who linked their account,
//! and link their own; everything else is the bridge bot's.

use ui::i18n::{ArgValue, EmbeddedCatalog};
use ui::icons::Icon;
use ui::prelude::*;
use ui::widget::primitives::{Fact, Facts};

use crate::effects::{self, DiscordEffect};
use crate::locales::CATALOG;
use crate::record::{Guild, Ownership};
use crate::setup::{self, Setup, SetupDoor};

/// What the view is built from: whether a server is bridged, how far the
/// bridge got, and how many channels and accounts it holds.
#[derive(Clone, Debug, Default, PartialEq)]
struct Shown {
    connected: bool,
    importing: bool,
    channels: usize,
    links: usize,
    /// Why calls do not reach Discord, as the bridge last heard it.
    voice_problem: Option<String>,
    /// Why the workspace's emoji do not reach the server, likewise.
    emoji_problem: Option<String>,
}

impl Shown {
    fn of(guild: &Guild) -> Self {
        Self {
            connected: !guild.guild_id.is_empty(),
            importing: guild.ownership == Ownership::Importing,
            channels: guild.channels.len(),
            links: guild.links.len(),
            voice_problem: guild.voice_problem.clone(),
            emoji_problem: guild.emoji_problem.clone(),
        }
    }
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

/// The counts, as separate facts, and why calls do not reach Discord when
/// they do not: said where the bridge's state is read, never left silent.
fn facts(shown: &Shown) -> Vec<Fact> {
    let mut facts = vec![
        Fact::with_icon(
            Icon::Hash,
            CATALOG.t_args("discord-channels", &[("count", count(shown.channels))]),
        ),
        Fact::with_icon(
            Icon::Users,
            CATALOG.t_args("discord-linked-accounts", &[("count", count(shown.links))]),
        ),
    ];
    if let Some(problem) = &shown.voice_problem {
        facts.push(Fact::with_icon(
            Icon::TriangleAlert,
            CATALOG.t_args(
                "discord-voice-problem",
                &[("problem", ArgValue::Text(problem.clone()))],
            ),
        ));
    }
    if let Some(problem) = &shown.emoji_problem {
        facts.push(Fact::with_icon(
            Icon::TriangleAlert,
            CATALOG.t_args(
                "discord-emoji-problem",
                &[("problem", ArgValue::Text(problem.clone()))],
            ),
        ));
    }
    facts
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

    fn view(cx: &mut ViewCx<Guild>) -> Element {
        let bridging = Setup::new();
        let answering = bridging.clone();
        cx.effects::<DiscordEffect>(move |effect, ecx| {
            if let Some(answer) = setup::answer(&answering, &effect, ecx) {
                return answer;
            }
            effects::answer(effect, ecx)
        });
        let shown = cx.value(Shown::of);
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
            let account = ui::widget::Section::titled(
                CATALOG.t("discord-your-account"),
                vec![
                    Box::new(Text::new(&CATALOG.t("discord-link-explanation")).dimmed()) as Element,
                ],
            );
            Box::new(
                Column::new()
                    .push(Box::new(name))
                    .push(Box::new(summary))
                    .push(Box::new(Facts::new(facts(shown))))
                    .push(Box::new(account)),
            )
        });
        let sheet = setup::sheet(cx, &bridging);
        Box::new(SetupDoor::new(page, sheet, &bridging))
    }
}

ui::dev_reload_entry!(Discord);
