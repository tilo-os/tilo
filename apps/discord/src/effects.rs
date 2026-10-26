//! What the connector does without changing its record by itself.
//!
//! A bot's program only sequences steps, so `plan-route` and `plan-event`
//! work out what an event means ([`crate::bridge`]) and answer the lists
//! the program loops over, one per verb to invoke. The linking effects
//! are a person's, run by the room's host for them
//! ([`ui::linked_accounts`]): `my-link` says whether they linked their
//! account, `connect` starts a sign-in on Discord's consent page,
//! `link-offer` and `accept-link` show and take the account a one-time
//! token offers, and `unlink-mine` unlinks it. `set-bot-token` keeps the
//! bot token in the host's secret store.

use discord_wire::event::{link_check, link_proof, sha256_hex};
use serde_json::{Value, json};
use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::linked_accounts::{AccountName, LinkStart, OfferReading, Reading};
use ui::tilo_resource::{self, Effect, Id};

use crate::bridge;
use crate::locales::CATALOG;
use crate::outbound::Context;
use crate::record::{Guild, LinkOffer};

/// The connector's effects.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "discord.effects")]
pub enum DiscordEffect {
    /// Connect my Discord account
    ///
    /// Starts signing in on Discord's consent page, which comes back to
    /// the room's host and links the account to the invoker. Answers the
    /// page's address, for the asking device alone.
    #[effect(returns, kind = "undoable", member)]
    ConnectAccount,
    /// Read my linked account
    ///
    /// Whether this room is bridged to a Discord server, and the Discord
    /// account the reader linked, if any.
    #[effect(kind = "read", returns, audience = "agents", member)]
    MyLink,
    /// Read a link offer
    ///
    /// The Discord account a one-time link token offers, and whether the
    /// reader may take it.
    #[effect(kind = "read", returns, member)]
    LinkOffer {
        #[param(secret)]
        token: String,
    },
    /// Link the offered Discord account
    ///
    /// Links the Discord account a one-time link token offers to the
    /// invoker, using the token up.
    #[effect(kind = "undoable", member)]
    AcceptLink {
        #[param(secret)]
        token: String,
    },
    /// Unlink my Discord account
    ///
    /// Unlinks the Discord account the invoker linked, if any.
    #[effect(kind = "undoable", member)]
    UnlinkMine,
    /// Set the bot token
    ///
    /// Keeps the Discord bot token in this host's secret store, where the
    /// connector's worker reads it. It never reaches the record.
    #[effect(privileged)]
    SetBotToken {
        #[param(secret)]
        token: String,
    },
    /// Find the bot's servers
    ///
    /// The Discord servers the bot with this token is in, by id and name,
    /// to choose the one this room bridges. The token is used for this one
    /// request and kept nowhere.
    #[effect(job, returns)]
    FindServers {
        #[param(secret)]
        token: String,
    },
    /// Bridge a server
    ///
    /// Gives the room's host the bot token and asks it to run the Discord
    /// Bridge for this server: the same setup the room's operator gives on
    /// the command line, through the room's own actions, which decide
    /// whether this person may add bots.
    BridgeServer {
        #[param(secret)]
        token: String,
        #[param("The server's id")]
        guild: String,
        #[param("The server's name")]
        server: String,
    },
    /// Find the message an event is about
    ///
    /// The Chat message and channel the bridge reads before planning
    /// `event`, each empty when it is about none.
    #[effect(kind = "read", returns, audience = "agents")]
    PlanRoute { event: Value },
    /// Plan an event
    ///
    /// What `event` means for both sides, one list per verb to run.
    #[effect(kind = "read", returns, audience = "agents")]
    PlanEvent {
        event: Value,
        #[param("Chat's read-room answer")]
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
        DiscordEffect::ConnectAccount
        | DiscordEffect::MyLink
        | DiscordEffect::LinkOffer { .. }
        | DiscordEffect::AcceptLink { .. }
        | DiscordEffect::UnlinkMine
        | DiscordEffect::SetBotToken { .. }
        | DiscordEffect::FindServers { .. }
        | DiscordEffect::BridgeServer { .. } => None,
        DiscordEffect::PlanRoute { event } => Some(json!({
            "message": bridge::route(guild, event),
            "channel": bridge::route_channel(event),
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

/// Discord's consent page for one round trip carrying `state`: the page
/// the room's host offers ([`Guild::sign_in_page`]) with the state added.
#[must_use]
pub fn consent_url(page: &str, state: &str) -> String {
    format!("{page}&state={}", discord_wire::event::query_value(state))
}

/// A fresh state for one round trip: 256 random bits, URL-safe.
fn new_state() -> String {
    let mut bytes = [0u8; 32];
    rand::fill(&mut bytes);
    data_encoding::BASE64URL_NOPAD.encode(&bytes)
}

/// What [`DiscordEffect::MyLink`] answers `me`.
#[must_use]
pub fn my_link(guild: &Guild, me: Option<&str>) -> Reading {
    let link = me.and_then(|me| guild.links.iter().find(|link| link.person == me));
    // `/link` is registered only in a bridge of the whole server.
    let slash = guild.only_channel.is_none();
    Reading {
        service: "Discord".into(),
        connected: !guild.guild_id.is_empty(),
        account: link.map(|link| AccountName {
            user_name: link.user_name.clone(),
            display_name: link.display_name.clone(),
        }),
        connect: guild
            .sign_in_page
            .is_some()
            .then(|| DiscordEffect::ConnectAccount.id().to_owned()),
        elsewhere: slash.then(|| CATALOG.t("discord-link-elsewhere")),
        unlink: link.map(|_| DiscordEffect::UnlinkMine.id().to_owned()),
        offer: DiscordEffect::LinkOffer {
            token: String::new(),
        }
        .id()
        .to_owned(),
        accept: DiscordEffect::AcceptLink {
            token: String::new(),
        }
        .id()
        .to_owned(),
    }
}

/// The offer `token` opens, still good at `now_ms`.
fn offer<'a>(guild: &'a Guild, token: &str) -> Option<&'a LinkOffer> {
    guild
        .link_offers
        .find(&Id::from_raw(link_check(&link_proof(token))))
}

/// What [`DiscordEffect::LinkOffer`] answers `me` about `token` at `now_ms`.
///
/// # Errors
/// What to do instead, when the token opens no offer `me` may take.
pub fn read_offer(
    guild: &Guild,
    token: &str,
    me: Option<&str>,
    now_ms: i64,
) -> Result<OfferReading, String> {
    let Some(offer) = offer(guild, token) else {
        return Err(CATALOG.t("discord-offer-gone"));
    };
    if now_ms > offer.expires_ms {
        return Err(CATALOG.t("discord-offer-expired"));
    }
    if offer.person.is_some() && offer.person.as_deref() != me {
        return Err(CATALOG.t("discord-offer-not-yours"));
    }
    Ok(OfferReading {
        service: "Discord".into(),
        account: AccountName {
            user_name: offer.user_name.clone(),
            display_name: offer.display_name.clone(),
        },
        started_here: offer.person.is_some(),
    })
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
        DiscordEffect::ConnectAccount => {
            if guild.guild_id.is_empty() {
                return Err(CATALOG.t("discord-not-bridged"));
            }
            let Some(page) = guild.sign_in_page.clone() else {
                return Err(CATALOG.t("discord-no-sign-in"));
            };
            let state = new_state();
            cx.dispatcher()
                .submit(Guild::BEGIN_CONNECT.with(sha256_hex(state.as_bytes())));
            let start = LinkStart {
                url: consent_url(&page, &state),
            };
            Ok(Effected::value(
                serde_json::to_value(start).map_err(|error| error.to_string())?,
            ))
        }
        DiscordEffect::MyLink => {
            let reading = my_link(&guild, cx.person().as_deref());
            Ok(Effected::value(
                serde_json::to_value(reading).map_err(|error| error.to_string())?,
            ))
        }
        DiscordEffect::LinkOffer { token } => {
            let reading = read_offer(
                &guild,
                &token,
                cx.person().as_deref(),
                tilo_resource::origin_now_ms(),
            )?;
            Ok(Effected::value(
                serde_json::to_value(reading).map_err(|error| error.to_string())?,
            ))
        }
        DiscordEffect::AcceptLink { token } => {
            // Said here, in the words the person reads; the record checks
            // the same again when it takes the proof.
            read_offer(
                &guild,
                &token,
                cx.person().as_deref(),
                tilo_resource::origin_now_ms(),
            )?;
            cx.dispatcher()
                .submit(Guild::COMPLETE_LINK.with(link_proof(&token)));
            Ok(Effected::done())
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
