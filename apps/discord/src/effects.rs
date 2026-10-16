//! What the connector does without changing its record by itself.
//!
//! A bot's program only sequences steps, so `plan-route` and `plan-event`
//! work out what an event means ([`crate::bridge`]) and answer the lists
//! the program loops over, one per verb to invoke. `link-account` and `set-bot-token` are a person's:
//! the first makes a code for `/link` on this device and stores only its
//! hash, the second keeps the bot token in the host's secret store.

use serde_json::{Value, json};
use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::tilo_resource::{self, Effect};

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
    /// Only the code's hash is kept; it expires after ten minutes.
    #[effect(returns)]
    LinkAccount,
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
        DiscordEffect::LinkAccount | DiscordEffect::SetBotToken { .. } => None,
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
            Ok(Effected::value(serde_json::json!({ "code": code })))
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
