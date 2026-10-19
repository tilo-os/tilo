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

/// What the view is built from: whether a server is bridged, how far the
/// bridge got, and how many channels and accounts it holds.
#[derive(Clone, Debug, Default, PartialEq)]
struct Shown {
    connected: bool,
    importing: bool,
    channels: usize,
    links: usize,
}

impl Shown {
    fn of(guild: &Guild) -> Self {
        Self {
            connected: !guild.guild_id.is_empty(),
            importing: guild.ownership == Ownership::Importing,
            channels: guild.channels.len(),
            links: guild.links.len(),
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

/// The counts, as separate facts.
fn facts(shown: &Shown) -> Vec<Fact> {
    vec![
        Fact::with_icon(
            Icon::Hash,
            CATALOG.t_args("discord-channels", &[("count", count(shown.channels))]),
        ),
        Fact::with_icon(
            Icon::Users,
            CATALOG.t_args("discord-linked-accounts", &[("count", count(shown.links))]),
        ),
    ]
}

/// Before a server is bridged: what the app is and how it starts.
fn not_connected() -> Element {
    Box::new(
        EmptyState::new(CATALOG.t("discord-empty-title"))
            .with_id("guild-status")
            .with_icon(Icon::MessageSquare)
            .with_message(CATALOG.t("discord-empty-body")),
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
        let code = cx.local(String::new());
        let shown_code = code.clone();
        cx.effects::<DiscordEffect>(move |effect, ecx| {
            let linking = effect == DiscordEffect::LinkAccount;
            let answer = effects::answer(effect, ecx);
            if linking
                && let Ok(ui::framework::Effected::Done(output)) = &answer
                && let Some(value) = output.value.as_ref().and_then(|v| v["code"].as_str())
            {
                code.set(value.to_owned());
            }
            answer
        });
        let shown = cx.value(Shown::of);
        cx.switch(&shown, move |cx, shown| {
            if !shown.connected {
                return not_connected();
            }
            let name = cx
                .text(|g: &Guild| g.name.clone())
                .with_id("guild-name")
                .with_role(TextRole::Title);
            let summary = Text::new(&status(shown)).with_id("guild-status");
            let link = cx
                .effect_button("discord-link-account", &DiscordEffect::LinkAccount)
                .role(ui::widget::ButtonRole::Primary);
            let instruction = Text::from_state(&shown_code, |code| {
                if code.is_empty() {
                    String::new()
                } else {
                    CATALOG.t("discord-link-code")
                }
            })
            .with_live(ui::a11y::Politeness::Polite);
            let code = Text::from_state(&shown_code, Clone::clone)
                .with_id("link-code")
                .with_role(TextRole::Code);
            let account = ui::widget::Section::titled(
                CATALOG.t("discord-your-account"),
                vec![
                    Box::new(Text::new(&CATALOG.t("discord-link-explanation")).dimmed()) as Element,
                    Box::new(link),
                    Box::new(instruction),
                    Box::new(code),
                ],
            );
            Box::new(
                Column::new()
                    .push(Box::new(name))
                    .push(Box::new(summary))
                    .push(Box::new(Facts::new(facts(shown))))
                    .push(Box::new(account)),
            )
        })
    }
}

ui::dev_reload_entry!(Discord);
