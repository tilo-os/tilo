//! The connector as an app: one bridged server per record. People see
//! which server it is, how far the bridge got and who linked their account,
//! and link their own; everything else is the bridge bot's.

use ui::i18n::{ArgValue, EmbeddedCatalog};
use ui::prelude::*;

use crate::effects::{self, DiscordEffect};
use crate::locales::CATALOG;
use crate::record::{Guild, Ownership};

fn status(guild: &Guild) -> String {
    if guild.guild_id.is_empty() {
        return CATALOG.t("discord-not-connected");
    }
    let key = match guild.ownership {
        Ownership::Importing => "discord-status-importing",
        Ownership::Adopted => "discord-status-bridged",
    };
    let count = |n: usize| ArgValue::Number(f64::from(u32::try_from(n).unwrap_or(u32::MAX)));
    CATALOG.t_args(
        key,
        &[
            ("channels", count(guild.channels.len())),
            ("links", count(guild.links.len())),
        ],
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
        let shown = code.clone();
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
        let name = cx
            .text(|g: &Guild| g.name.clone())
            .with_id("guild-name")
            .with_role(TextRole::Title);
        let summary = cx.text(status).with_id("guild-status");
        let link = cx.effect_button("discord-link-account", &DiscordEffect::LinkAccount);
        let instruction = Text::from_state(&shown, |code| {
            if code.is_empty() {
                String::new()
            } else {
                CATALOG.t("discord-link-code")
            }
        })
        .with_live(ui::a11y::Politeness::Polite);
        let code = Text::from_state(&shown, Clone::clone)
            .with_id("link-code")
            .with_role(TextRole::Code);
        Box::new(
            Column::new()
                .push(Box::new(name))
                .push(Box::new(summary))
                .push(Box::new(link))
                .push(Box::new(instruction))
                .push(Box::new(code)),
        )
    }
}

ui::dev_reload_entry!(Discord);
