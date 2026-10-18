//! What the connector does without changing its record by itself.
//!
//! A bot's program only sequences steps, so `plan-route` and `plan-event`
//! work out what an event means ([`crate::bridge`]) and answer the lists
//! the program loops over, one per verb to invoke. `link-account` and `set-bot-token` are a person's:
//! the first makes a code for `/link` on this device and stores only its
//! hash, the second keeps the bot token in the host's secret store.
//! `my-link` tells the person reading it whether they linked their account,
//! which the profile card and Settings show
//! ([`ui::linked_accounts`]).

use serde_json::{Value, json};
use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::linked_accounts::{AccountName, LinkCode, Reading};
use ui::tilo_resource::{self, Effect, Id};

use crate::locales::CATALOG;
use crate::outbound::Context;
use crate::record::Guild;
use crate::{bridge, link};

/// The connector's effects.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "discord.effects")]
pub enum DiscordEffect {
    /// Link my Discord account
    ///
    /// Makes a one-time code to send to the bridge with `/link` in Discord.
    /// Only the code's hash is kept; it expires after ten minutes. Any
    /// member may ask the room's host to make it for them: the code comes
    /// back to the asking device alone.
    #[effect(returns, kind = "undoable", member)]
    LinkAccount,
    /// Read my linked account
    ///
    /// Whether this room is bridged to a Discord server, and the Discord
    /// account the reader linked, if any.
    #[effect(kind = "read", returns, programmatic, member)]
    MyLink,
    /// Unlink my Discord account
    ///
    /// Unlinks the Discord account the invoker linked, if any.
    #[effect(kind = "undoable", member)]
    UnlinkMine,
    /// Set the bot token
    ///
    /// Keeps the Discord bot token in this host's secret store, where the
    /// connector's worker reads it. It never reaches the record.
    #[effect(device_only, privileged)]
    SetBotToken {
        #[param(secret)]
        token: String,
    },
    /// Find the message an event is about
    ///
    /// The Chat message and channel the bridge reads before planning
    /// `event`, each empty when it is about none, and the Chat workspace
    /// the server mirrors.
    #[effect(kind = "read", returns, programmatic)]
    PlanRoute { event: Value },
    /// Plan an event
    ///
    /// What `event` means for both sides, one list per verb to run.
    #[effect(kind = "read", returns, programmatic)]
    PlanEvent {
        event: Value,
        #[param("Chat's read-workspace answer")]
        chat: Value,
        #[param("Where avatars and Tilo links point; an install may give none")]
        context: Option<Context>,
        #[param("The room's members with their roles")]
        room: Option<Value>,
    },
}

/// The bridge's plans for `guild`, or `None` for the effects that are not
/// plans.
#[must_use]
pub fn plan(guild: &Guild, effect: &DiscordEffect) -> Option<Value> {
    match effect {
        DiscordEffect::LinkAccount
        | DiscordEffect::MyLink
        | DiscordEffect::UnlinkMine
        | DiscordEffect::SetBotToken { .. } => None,
        DiscordEffect::PlanRoute { event } => Some(json!({
            "message": bridge::route(guild, event),
            "channel": bridge::route_channel(event),
            "workspace": guild.workspace,
        })),
        DiscordEffect::PlanEvent {
            event,
            chat,
            context,
            room,
        } => serde_json::to_value(bridge::plan(
            bridge::Reading {
                guild,
                chat,
                context: &context.clone().unwrap_or_default(),
                room: room.as_ref().unwrap_or(&Value::Null),
            },
            event,
        ))
        .ok(),
    }
}

/// What [`DiscordEffect::MyLink`] answers `me` at `now_ms`.
#[must_use]
pub fn my_link(guild: &Guild, me: Option<&str>, now_ms: i64) -> Reading {
    let link = me.and_then(|me| guild.links.iter().find(|link| link.person == me));
    let pending = me
        .and_then(|me| guild.pending_links.find(&Id::from_raw(me)))
        .is_some_and(|pending| now_ms <= pending.expires_ms);
    Reading {
        service: "Discord".into(),
        connected: !guild.guild_id.is_empty(),
        account: link.map(|link| AccountName {
            user_name: link.user_name.clone(),
            display_name: link.display_name.clone(),
        }),
        pending,
        link: DiscordEffect::LinkAccount.id().to_owned(),
        unlink: link.map(|_| DiscordEffect::UnlinkMine.id().to_owned()),
    }
}

/// The code `/link` takes, with the line saying what to do with it.
#[must_use]
pub fn link_code(code: String) -> LinkCode {
    LinkCode {
        instruction: CATALOG.t("discord-link-code"),
        code,
    }
}

/// Answer `effect` for the guild the view shows.
///
/// # Errors
/// Why the token could not be stored.
pub fn answer(effect: DiscordEffect, cx: &mut EffectCx<Guild>) -> EffectAnswer {
    let guild = cx.live().get();
    if let Some(value) = plan(&guild, &effect) {
        return Ok(Effected::value(value));
    }
    match effect {
        DiscordEffect::LinkAccount => {
            if guild.guild_id.is_empty() {
                return Err("this record is not connected to a Discord server yet".into());
            }
            let code = link::new_code();
            cx.dispatcher()
                .submit(Guild::BEGIN_LINK.with(link::hash(&code)));
            Ok(Effected::value(
                serde_json::to_value(link_code(code)).map_err(|error| error.to_string())?,
            ))
        }
        DiscordEffect::MyLink => {
            let reading = my_link(
                &guild,
                cx.person().as_deref(),
                tilo_resource::origin_now_ms(),
            );
            Ok(Effected::value(
                serde_json::to_value(reading).map_err(|error| error.to_string())?,
            ))
        }
        DiscordEffect::UnlinkMine => {
            let me = cx.person();
            if let Some(link) = guild
                .links
                .iter()
                .find(|link| Some(link.person.as_str()) == me.as_deref())
            {
                cx.dispatcher()
                    .submit(Guild::UNLINK.with(link.discord_user.clone()));
            }
            Ok(Effected::done())
        }
        DiscordEffect::SetBotToken { token } => set_bot_token(&token),
        _ => Ok(Effected::done()),
    }
}

fn set_bot_token(token: &str) -> EffectAnswer {
    let store = ui::secrets::SecretStore::for_app(crate::APP_ID);
    crate::token::store_token(&store, token).map_err(|error| error.to_string())?;
    Ok(Effected::done())
}

#[cfg(test)]
mod tests;
