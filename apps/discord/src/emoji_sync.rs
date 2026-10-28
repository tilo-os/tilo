//! The workspace's emoji in the bridged server (discord-bridge.md §3,
//! "Custom emoji"): the bridge uploads each workspace emoji as a custom
//! emoji of the server, renames it when the workspace's is renamed, and
//! deletes it when the workspace's goes, so `:name:` written or reacted in
//! Tilo shows as that emoji in Discord.
//!
//! Only emoji the bridge uploaded itself are ever renamed or deleted; it
//! keeps their ids in its record's map (`MappedKind::Emoji`). Where the
//! server already has an emoji of its own under a workspace emoji's name,
//! that one stands for it and nothing is uploaded. The plan compares the
//! workspace's emoji with what the bridge uploaded, so running it again
//! changes nothing: it runs whenever the workspace's emoji change and
//! whenever Discord reports the server's layout (on every start, and when
//! its emoji or the bot's roles change), which is how emoji the server had
//! no room or permission for arrive once it does.

use std::collections::{BTreeMap, BTreeSet};

use discord_wire::op::{DeleteEmojiOp, EmojiSpec};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ui::tilo_resource::{self, JsonSchema};

use crate::emoji::discord_name;
use crate::record::{Guild, MappedKind, Mapping};

/// One workspace emoji as the room's host hands it to the bridge: its name
/// and the content address of its picture, which the host keeps in its
/// content store for the worker to upload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wanted {
    pub name: String,
    pub picture: String,
}

/// The workspace's emoji from the host's `emoji` input (`{"emoji": [{name,
/// picture}]}`); `None` when the host bound none, which leaves the
/// server's emoji as they are.
#[must_use]
pub fn wanted(input: Option<&Value>) -> Option<Vec<Wanted>> {
    let list = input?.get("emoji")?;
    serde_json::from_value(list.clone()).ok()
}

/// An upload or rename, then `settle-emoji` with its result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApplyItem {
    pub op: EmojiSpec,
    /// The picture's content address, kept with the emoji in the map.
    pub picture: String,
    /// The workspace emoji's name before a rename; always written, as
    /// `null` for an upload, since the program binds it.
    #[serde(default)]
    pub was: Option<String>,
}

/// What the server's emoji need: deletes run before uploads, so a picture
/// replaced under the same name frees its slot first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EmojiPlan {
    pub deletes: Vec<DeleteEmojiOp>,
    pub applies: Vec<ApplyItem>,
}

/// Bring the server's emoji in line with the workspace's `wanted`.
#[must_use]
pub fn plan(guild: &Guild, wanted: &[Wanted]) -> EmojiPlan {
    let mut plan = EmojiPlan::default();
    // A bridge of one channel changes nothing else in the server, and the
    // server's own emoji are known only once its layout was observed.
    if guild.guild_id.is_empty() || guild.only_channel.is_some() || guild.bot_user.is_empty() {
        return plan;
    }
    let uploaded: Vec<&Mapping> = guild
        .map
        .iter()
        .filter(|mapping| mapping.kind == MappedKind::Emoji)
        .collect();
    let ours: BTreeSet<&str> = uploaded.iter().map(|m| m.discord.as_str()).collect();
    let wanted: BTreeMap<&str, &str> = wanted
        .iter()
        .filter(|emoji| crate::emoji::valid_discord_name(&emoji.name))
        .map(|emoji| (emoji.name.as_str(), emoji.picture.as_str()))
        .collect();
    // Names whose server emoji stays or is being made: each once.
    let mut done: BTreeSet<&str> = BTreeSet::new();
    let mut gone: Vec<&Mapping> = Vec::new();
    for mapping in &uploaded {
        match wanted.get(mapping.tilo.as_str()) {
            Some(picture) if mapping.picture.as_deref().is_none_or(|p| p == *picture) => {
                done.insert(mapping.tilo.as_str());
            }
            _ => gone.push(mapping),
        }
    }
    let unmade = |name: &str, done: &BTreeSet<&str>| {
        !done.contains(name) && guild.mapping(MappedKind::Emoji, name).is_none()
    };
    for mapping in gone {
        // A workspace emoji renamed: another name with the same picture
        // that the server has no emoji for yet takes this one.
        let renamed = mapping.picture.as_deref().and_then(|picture| {
            wanted
                .iter()
                .find(|(name, p)| **p == picture && unmade(name, &done))
                .map(|(name, _)| *name)
        });
        match renamed.filter(|_| !wanted.contains_key(mapping.tilo.as_str())) {
            Some(name) => {
                done.insert(name);
                plan.applies.push(ApplyItem {
                    op: EmojiSpec {
                        id: Some(mapping.discord.clone()),
                        name: discord_name(name),
                        picture: mapping.picture.clone(),
                        tilo: name.to_owned(),
                    },
                    picture: mapping.picture.clone().unwrap_or_default(),
                    was: Some(mapping.tilo.clone()),
                });
            }
            None => plan.deletes.push(DeleteEmojiOp {
                emoji: mapping.discord.clone(),
                tilo: mapping.tilo.clone(),
            }),
        }
    }
    let replaced: BTreeSet<&str> = plan.deletes.iter().map(|d| d.tilo.as_str()).collect();
    for (name, picture) in &wanted {
        if done.contains(name)
            || (guild.mapping(MappedKind::Emoji, name).is_some() && !replaced.contains(name))
        {
            continue;
        }
        // The server's own emoji of that name stands for it.
        if guild
            .emoji(&discord_name(name))
            .is_some_and(|id| !ours.contains(id))
        {
            continue;
        }
        plan.applies.push(ApplyItem {
            op: EmojiSpec {
                id: None,
                name: discord_name(name),
                picture: Some((*picture).to_owned()),
                tilo: (*name).to_owned(),
            },
            picture: (*picture).to_owned(),
            was: None,
        });
    }
    plan
}

#[cfg(test)]
mod tests;
