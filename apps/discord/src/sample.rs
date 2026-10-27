//! What the capture scenarios show: a room bridged to a small server, with
//! channels, categories and two linked accounts. Never written into a real
//! workspace — a capture cannot wait for the bridge to observe a server, so
//! the scene is written as the record's start instead.

use discord_wire::op::ChannelKind;

use crate::record::{DChannel, Guild, Link, Ownership};

pub(crate) const SCENARIOS: &[&str] = &["bridged", "importing", "unlinked"];

/// The reader's own Discord account.
const MINE: &str = "d_ada";
/// Another member's.
const THEIRS: &str = "d_bo";

fn channel(
    id: &str,
    kind: ChannelKind,
    name: &str,
    parent: Option<&str>,
    position: i64,
) -> DChannel {
    DChannel {
        id: id.into(),
        kind,
        name: name.into(),
        parent: parent.map(str::to_owned),
        position,
        ..DChannel::default()
    }
}

fn link(discord_user: &str, person: &str, user_name: &str, display_name: &str) -> Link {
    Link {
        discord_user: discord_user.into(),
        person: person.into(),
        user_name: user_name.into(),
        display_name: Some(display_name.into()),
        ..Link::default()
    }
}

/// The bridged server a scenario shows. `person` is the capture's own
/// participant, who has linked `MINE`.
pub(crate) fn guild(scenario: Option<&str>, person: &str) -> Guild {
    let mut guild = Guild {
        guild_id: "1140209000000000001".into(),
        name: "Tilo testers".into(),
        ownership: if scenario == Some("importing") {
            Ownership::Importing
        } else {
            Ownership::Adopted
        },
        sign_in_page: Some("https://discord.com/oauth2/authorize?client_id=1".into()),
        bot_user: "1140209000000000002".into(),
        ..Guild::default()
    };
    for channel in [
        channel("d_root", ChannelKind::Text, "welcome", None, 0),
        channel("d_standup", ChannelKind::Text, "standup", None, 1),
        channel("d_launch", ChannelKind::Text, "launch", None, 2),
        channel("d_voice", ChannelKind::Voice, "Lounge", None, 3),
        channel("d_product", ChannelKind::Category, "Product", None, 4),
        channel("d_bugs", ChannelKind::Text, "bugs", Some("d_product"), 0),
        channel(
            "d_design",
            ChannelKind::Text,
            "design",
            Some("d_product"),
            1,
        ),
        channel("d_office", ChannelKind::Category, "Office", None, 5),
        channel("d_lunch", ChannelKind::Voice, "Lunch", Some("d_office"), 0),
    ] {
        guild.channels.push(channel);
    }
    if scenario == Some("unlinked") {
        guild
            .links
            .push(link(THEIRS, "u_bo", "bo.nowak", "Bo Nowak"));
    } else {
        guild
            .links
            .push(link(MINE, person, "ada.l", "Ada Lovelace"));
        guild
            .links
            .push(link(THEIRS, "u_bo", "bo.nowak", "Bo Nowak"));
    }
    if scenario != Some("importing") {
        guild.voice_problem = Some("the bot may not connect in Lunch".into());
    }
    guild
}
