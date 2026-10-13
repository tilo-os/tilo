//! Role mapping (discord-bridge.md §5, "Roles"), only when the bridge is
//! connected with `map_roles`. Tilo's five roles become Discord roles the
//! bridge makes and owns ("Tilo Viewer" … "Tilo Owner"); their definitions
//! are kept in line, membership only for linked accounts. `@everyone` and
//! the server's own roles are never changed. Discord roles only add
//! permissions, so what Viewer and Commenter may not do is denied through an
//! overwrite for that bridge role on every bridged channel.

use discord_wire::op::{DiscordOp, MemberRolesOp, OverwriteKind, OverwriteOp, RoleSpec};
use discord_wire::perm;
use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Id, JsonSchema, Settled};

use crate::record::{Guild, MappedKind};

/// Tilo's roles, lowest first.
pub const TILO_ROLES: [&str; 5] = ["viewer", "commenter", "editor", "moderator", "owner"];

/// What an Editor may do in Discord; the other roles are told apart from it.
pub const EVERYONE: u64 = perm::VIEW_CHANNEL
    | perm::READ_MESSAGE_HISTORY
    | perm::SEND_MESSAGES
    | perm::SEND_MESSAGES_IN_THREADS
    | perm::ADD_REACTIONS
    | perm::ATTACH_FILES
    | perm::CONNECT
    | perm::SPEAK
    | perm::USE_APPLICATION_COMMANDS;

const MODERATION: u64 =
    perm::MANAGE_MESSAGES | perm::MODERATE_MEMBERS | perm::KICK_MEMBERS | perm::BAN_MEMBERS;

/// The Discord permissions of a Tilo role.
#[must_use]
pub fn permissions(role: &str) -> u64 {
    match role {
        "viewer" => {
            EVERYONE
                & !(perm::SEND_MESSAGES
                    | perm::SEND_MESSAGES_IN_THREADS
                    | perm::ADD_REACTIONS
                    | perm::ATTACH_FILES)
        }
        "commenter" => {
            EVERYONE & !(perm::SEND_MESSAGES | perm::SEND_MESSAGES_IN_THREADS | perm::ATTACH_FILES)
        }
        "moderator" | "owner" => EVERYONE | MODERATION,
        _ => EVERYONE,
    }
}

/// What a role's overwrite denies on each mapped channel.
#[must_use]
pub fn channel_deny(role: &str) -> u64 {
    EVERYONE & !permissions(role) & !perm::VIEW_CHANNEL
}

/// The Discord name of the bridge's role for Tilo's `role`.
fn display_name(role: &str) -> String {
    let mut chars = role.chars();
    let name = chars
        .next()
        .map(|c| c.to_ascii_uppercase().to_string() + chars.as_str())
        .unwrap_or_default();
    format!("Tilo {name}")
}

const fn color(role: &str) -> u32 {
    match role.as_bytes() {
        b"viewer" => 0x0095_a5a6,
        b"commenter" => 0x0034_98db,
        b"editor" => 0x002e_cc71,
        b"moderator" => 0x00e6_7e22,
        b"owner" => 0x00e7_4c3c,
        _ => 0,
    }
}

/// Create or correct one role, then map it as a role.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoleItem {
    pub op: RoleSpec,
}

/// One role reconcile run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RolePlan {
    /// `apply-role`, then `map` (kind role, tilo = `op.tilo`).
    pub apply: Vec<RoleItem>,
    /// `set-overwrite`.
    pub overwrites: Vec<OverwriteOp>,
}

impl RolePlan {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.apply.is_empty() && self.overwrites.is_empty()
    }
}

/// The plan that keeps the bridge's own roles in line with Tilo's.
#[must_use]
pub fn plan(guild: &Guild) -> RolePlan {
    let mut plan = RolePlan::default();
    // Without role mapping, or confined to one channel, the bridge leaves
    // the server's roles and permissions alone.
    if !guild.map_roles || guild.guild_id.is_empty() || guild.only_channel.is_some() {
        return plan;
    }
    for role in TILO_ROLES {
        let want_name = display_name(role);
        let want_perms = perm::bits(permissions(role));
        let mapped = guild.mapping(MappedKind::Role, role);
        let current = mapped.and_then(|m| guild.roles.find(&Id::from_raw(m.discord.as_str())));
        // Mapped but not observed yet: just created, Discord's echo not in.
        if mapped.is_some()
            && current.is_none()
            && !guild.roles.is_empty()
            && !role_vanished(guild, mapped.map(|m| m.discord.as_str()))
        {
            continue;
        }
        let same = current.is_some_and(|r| {
            r.name == want_name && r.permissions == want_perms && r.color == color(role)
        });
        if !same {
            plan.apply.push(RoleItem {
                op: RoleSpec {
                    id: current.map(|r| r.id.clone()),
                    tilo: role.into(),
                    name: want_name,
                    permissions: want_perms,
                    color: color(role),
                },
            });
        }
    }
    // Overwrites for the roles that may do less than @everyone. One asked
    // for and not yet seen in a layout event is not asked again: the layout
    // events Discord sent before it took effect still arrive, and asking
    // again on each one echoes yet more of them. `Guild::observe` forgets
    // the ask once a layout reports a change to that overwrite.
    let asked: Vec<&OverwriteOp> = guild
        .outbox
        .entries()
        .filter(|(_, entry)| entry.settled.as_ref().is_none_or(Settled::is_done))
        .filter_map(|(_, entry)| match &entry.value.op {
            DiscordOp::SetOverwrite(op) => Some(op),
            _ => None,
        })
        .collect();
    for role in ["viewer", "commenter"] {
        let Some(role_id) = guild
            .mapping(MappedKind::Role, role)
            .map(|m| m.discord.clone())
        else {
            continue;
        };
        let deny = channel_deny(role);
        for mapping in guild.map.iter().filter(|m| m.kind == MappedKind::Channel) {
            let Some(channel) = guild.channels.find(&Id::from_raw(mapping.discord.as_str())) else {
                continue;
            };
            let current = channel
                .overwrites
                .0
                .iter()
                .find(|o| o.id == role_id && o.kind == 0);
            let want = OverwriteOp {
                channel: channel.id.clone(),
                target: role_id.clone(),
                target_kind: OverwriteKind::Role,
                allow: "0".into(),
                deny: perm::bits(deny),
            };
            if current.is_none_or(|o| perm::parse(&o.deny) != deny || perm::parse(&o.allow) != 0)
                && !asked.contains(&&want)
            {
                plan.overwrites.push(want);
            }
        }
    }
    plan
}

fn role_vanished(guild: &Guild, discord: Option<&str>) -> bool {
    discord.is_some_and(|d| guild.vanished.contains(&d.to_owned()))
}

/// Bring a linked member's managed roles in line with their Tilo role.
/// `current` is their Discord roles when an event reported them; without it
/// every other managed role is removed, which Discord treats as a no-op for
/// roles they do not hold. `None` when the account is not linked, nothing
/// changes, or the bridge is confined to one channel.
#[must_use]
pub fn plan_member(
    guild: &Guild,
    discord_user: &str,
    tilo_role: Option<&str>,
    current: Option<&[String]>,
) -> Option<MemberRolesOp> {
    if !guild.map_roles || guild.only_channel.is_some() {
        return None;
    }
    guild.links.find(&Id::from_raw(discord_user))?;
    let managed: Vec<String> = TILO_ROLES
        .iter()
        .filter_map(|role| {
            guild
                .mapping(MappedKind::Role, role)
                .map(|m| m.discord.clone())
        })
        .collect();
    let want = tilo_role
        .and_then(|role| guild.mapping(MappedKind::Role, role))
        .map(|m| m.discord.clone());
    let holds = |role: &String| current.is_none_or(|roles| roles.contains(role));
    let add: Vec<String> = want
        .iter()
        .filter(|w| current.is_none_or(|roles| !roles.contains(w)))
        .cloned()
        .collect();
    let remove: Vec<String> = managed
        .into_iter()
        .filter(|r| Some(r) != want.as_ref() && holds(r))
        .collect();
    (!add.is_empty() || !remove.is_empty()).then(|| MemberRolesOp {
        user: discord_user.into(),
        add,
        remove,
    })
}

/// The Discord accounts linked to `person`.
#[must_use]
pub fn linked_accounts(guild: &Guild, person: &str) -> Vec<String> {
    guild
        .links
        .iter()
        .filter(|l| l.person == person)
        .map(|l| l.discord_user.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use discord_wire::event::OverwriteInfo;
    use discord_wire::op::ChannelKind;
    use ui::tilo_resource::Json;

    use super::*;
    use crate::layout::tests::{dchan, mapping};
    use crate::record::{DRole, Link, Ownership};

    fn drole(id: &str, name: &str, perms: u64, color: u32) -> DRole {
        DRole {
            id: id.into(),
            name: name.into(),
            permissions: perm::bits(perms),
            color,
            position: 0,
            managed: false,
        }
    }

    fn guild() -> Guild {
        Guild {
            guild_id: "1".into(),
            ownership: Ownership::Adopted,
            map_roles: true,
            ..Guild::default()
        }
    }

    fn settled() -> Guild {
        let mut g = guild();
        let mut roles = vec![drole("1", "@everyone", EVERYONE, 0)];
        for (i, role) in TILO_ROLES.iter().enumerate() {
            let id = format!("r{i}");
            roles.push(drole(
                &id,
                &display_name(role),
                permissions(role),
                color(role),
            ));
            g.map.push(mapping(MappedKind::Role, role, &id));
        }
        g.roles = roles.into_iter().collect();
        g.map.push(mapping(MappedKind::Channel, "c1", "d1"));
        let mut d1 = dchan("d1", ChannelKind::Text, "general", None, 0);
        d1.overwrites = Json(vec![
            OverwriteInfo {
                id: "r0".into(),
                kind: 0,
                allow: "0".into(),
                deny: perm::bits(channel_deny("viewer")),
            },
            OverwriteInfo {
                id: "r1".into(),
                kind: 0,
                allow: "0".into(),
                deny: perm::bits(channel_deny("commenter")),
            },
        ]);
        g.channels.push(d1);
        g
    }

    #[test]
    fn without_role_mapping_no_role_or_permission_is_touched() {
        let mut g = settled();
        g.map_roles = false;
        g.roles = [drole("1", "@everyone", 0, 0)].into_iter().collect();
        assert!(plan(&g).is_empty());
        g.links.push(Link {
            discord_user: "d_ada".into(),
            person: "u_ada".into(),
            linked_at: 0,
        });
        assert_eq!(plan_member(&g, "d_ada", Some("moderator"), None), None);
    }

    #[test]
    fn a_new_server_gets_five_roles_of_the_bridges_own_and_everyone_is_left_alone() {
        let mut g = guild();
        g.roles = [
            drole("1", "@everyone", 0, 0),
            drole("r_mods", "Moderator", perm::MANAGE_MESSAGES, 0),
        ]
        .into_iter()
        .collect();
        let p = plan(&g);
        let names: Vec<_> = p.apply.iter().map(|i| i.op.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Tilo Viewer",
                "Tilo Commenter",
                "Tilo Editor",
                "Tilo Moderator",
                "Tilo Owner"
            ]
        );
        assert!(
            p.apply.iter().all(|i| i.op.id.is_none()),
            "the server's own roles are never taken over"
        );
        assert!(
            p.overwrites.is_empty(),
            "overwrites follow once the roles exist"
        );
    }

    #[test]
    fn a_settled_server_plans_nothing() {
        assert!(plan(&settled()).is_empty(), "{:?}", plan(&settled()));
    }

    #[test]
    fn a_role_edited_in_discord_is_reverted() {
        let mut g = settled();
        g.roles.take(&Id::from_raw("r3"));
        g.roles.push(drole(
            "r3",
            "Mods",
            permissions("moderator") | perm::MANAGE_CHANNELS,
            0,
        ));
        let p = plan(&g);
        assert_eq!(p.apply.len(), 1);
        assert_eq!(p.apply[0].op.id.as_deref(), Some("r3"));
        assert_eq!(
            perm::parse(&p.apply[0].op.permissions) & perm::MANAGE_CHANNELS,
            0,
            "nobody gets Manage Channels"
        );
    }

    #[test]
    fn viewers_and_commenters_are_denied_through_overwrites() {
        let mut g = settled();
        g.map.push(mapping(MappedKind::Channel, "c2", "d2"));
        g.channels
            .push(dchan("d2", ChannelKind::Text, "new", None, 1));
        let p = plan(&g);
        assert_eq!(p.overwrites.len(), 2);
        let viewer = p.overwrites.iter().find(|o| o.target == "r0").unwrap();
        assert_ne!(perm::parse(&viewer.deny) & perm::SEND_MESSAGES, 0);
        let commenter = p.overwrites.iter().find(|o| o.target == "r1").unwrap();
        assert_eq!(
            perm::parse(&commenter.deny) & perm::ADD_REACTIONS,
            0,
            "commenters react"
        );
    }

    #[test]
    fn an_overwrite_asked_for_is_asked_again_only_once_a_layout_answers_it() {
        let mut g = settled();
        g.map.push(mapping(MappedKind::Channel, "c2", "d2"));
        g.channels
            .push(dchan("d2", ChannelKind::Text, "new", None, 1));
        let first = plan(&g);
        let asked = first.overwrites[0].clone();
        let entry = Id::from_raw("e1");
        g.outbox.push(
            entry.clone(),
            discord_wire::op::Queued {
                op: DiscordOp::SetOverwrite(asked.clone()),
                publish: Vec::new(),
                attach: Vec::new(),
            },
        );
        assert_eq!(plan(&g).overwrites, first.overwrites[1..], "queued");
        g.outbox
            .settle(&entry, Settled::Done(discord_wire::op::DiscordResult::Done));
        // A layout from before it took effect: nothing changed for it.
        let stale = layout_of(&g);
        g.observe(String::new(), stale.clone(), roles_of(&g));
        assert_eq!(
            plan(&g).overwrites,
            first.overwrites[1..],
            "done, no echo yet"
        );
        // Someone sets something else on it in Discord: answered, so reverted.
        let mut edited = stale;
        let d2 = edited.iter_mut().find(|c| c.id == "d2").unwrap();
        d2.overwrites.push(OverwriteInfo {
            id: asked.target.clone(),
            kind: 0,
            allow: "0".into(),
            deny: "0".into(),
        });
        g.observe(String::new(), edited, roles_of(&g));
        assert!(g.outbox.is_empty(), "the ask is forgotten");
        assert!(plan(&g).overwrites.contains(&asked));
    }

    fn roles_of(g: &Guild) -> Vec<discord_wire::event::RoleInfo> {
        g.roles
            .iter()
            .map(|r| discord_wire::event::RoleInfo {
                id: r.id.clone(),
                name: r.name.clone(),
                permissions: r.permissions.clone(),
                color: r.color,
                position: r.position,
                managed: r.managed,
            })
            .collect()
    }

    fn layout_of(g: &Guild) -> Vec<discord_wire::event::ChannelInfo> {
        g.channels
            .iter()
            .map(|c| discord_wire::event::ChannelInfo {
                id: c.id.clone(),
                kind: c.kind.0,
                name: c.name.clone(),
                parent: c.parent.clone(),
                position: c.position,
                topic: c.topic.clone(),
                overwrites: c.overwrites.0.clone(),
            })
            .collect()
    }

    #[test]
    fn membership_is_mirrored_only_for_linked_accounts() {
        let mut g = settled();
        assert_eq!(
            plan_member(&g, "d_ada", Some("moderator"), None),
            None,
            "unlinked"
        );
        g.links.push(Link {
            discord_user: "d_ada".into(),
            person: "u_ada".into(),
            linked_at: 0,
        });
        let op = plan_member(
            &g,
            "d_ada",
            Some("moderator"),
            Some(&["r2".to_owned(), "x".to_owned()]),
        )
        .unwrap();
        assert_eq!(op.add, ["r3"]);
        assert_eq!(op.remove, ["r2"], "only managed roles are taken away");
        assert_eq!(
            plan_member(&g, "d_ada", Some("moderator"), Some(&["r3".to_owned()])),
            None
        );
        let blind = plan_member(&g, "d_ada", Some("editor"), None).unwrap();
        assert_eq!(blind.add, ["r2"]);
        assert_eq!(blind.remove.len(), 4);
        assert_eq!(linked_accounts(&g, "u_ada"), ["d_ada"]);
    }
}
