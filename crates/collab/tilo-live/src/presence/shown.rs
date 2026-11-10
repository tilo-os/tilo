//! One member of a workspace, flattened for the surface that shows them: the
//! board's merged word as a [`Shown`], which a page hands its JavaScript and a
//! desktop hands its roster, so both draw a member alike.

use collab_types::presence::{ShellPresence, Shown, ShownFocus, ShownLayout, ShownPoint};

use super::{Device, Member, Own};

/// What a shell says of its person that a surface draws: its face, where its
/// windows are, the caret in them and what it says beside its pointer.
fn of_shell(shown: &mut Shown, shell: Option<&ShellPresence>) {
    let Some(shell) = shell else {
        return;
    };
    shown.agent = shell.agent;
    shown.colour.clone_from(&shell.colour);
    shown.place.clone_from(&shell.place);
    shown.selection.clone_from(&shell.selection);
    shown.chat.clone_from(&shell.chat);
    shown.reaction.clone_from(&shell.reaction);
    shown.focus = shell.focus().map(|(app, record)| ShownFocus {
        app: app.to_owned(),
        record: hex::encode(record),
    });
    shown.layout = Some(ShownLayout {
        size: [
            u16::try_from(shell.size[0]).unwrap_or(u16::MAX),
            u16::try_from(shell.size[1]).unwrap_or(u16::MAX),
        ],
        windows: shell
            .windows
            .iter()
            .map(|window| collab_types::presence::ShownWindow {
                app: window.app.clone(),
                record: hex::encode(window.record),
                rect: window.rect,
                focused: window.focused,
            })
            .collect(),
    });
}

/// `member` as a surface here draws them, on the clock presence is heard on.
#[must_use]
pub fn shown(member: &Member, now_ms: f64) -> Shown {
    let mut shown = Shown {
        publisher: hex::encode(member.publisher),
        epoch: member.epoch,
        agent: member.agent(),
        label: member.label().to_owned(),
        activity: member.activity().map(|(activity, _)| activity.to_owned()),
        automatic: member.activity().is_some_and(|(_, automatic)| automatic),
        user: member.user.clone(),
        profile_ms: member.profile_ms,
        device: member.device.clone(),
        following: member.following.clone(),
        wave: member.wave.clone(),
        pointer: member.pointer(now_ms).map(|(tab, x, y)| ShownPoint {
            tab: tab.to_string(),
            at: [x, y],
            age_ms: member
                .pointer_heard_at()
                .map_or(0.0, |heard| now_ms - heard),
        }),
        availability: member.availability.id().to_owned(),
        call: member.call.as_ref().map(|call| call.call.encode()),
        ..Shown::default()
    };
    of_shell(&mut shown, member.shell.as_deref());
    shown
}

/// This device's own word as a surface here draws it: the roster's row for
/// the person at this seat, which the board never carries (a reader hears
/// everyone but itself).
#[must_use]
pub fn own_shown(own: &Own, publisher: Device) -> Shown {
    let shell_activity = own
        .shell
        .as_deref()
        .and_then(|shell| shell.activity.clone())
        .filter(|activity| !activity.trim().is_empty());
    let status = own
        .status
        .as_ref()
        .filter(|status| shell_activity.is_none() && !status.text.trim().is_empty());
    let mut shown = Shown {
        publisher: hex::encode(publisher),
        agent: own.shell.as_deref().is_some_and(|shell| shell.agent),
        label: own.label.clone(),
        automatic: status.is_some_and(|status| status.automatic),
        activity: shell_activity.or_else(|| status.map(|status| status.text.clone())),
        user: own.user.clone(),
        profile_ms: own.profile_ms,
        device: own.device.clone(),
        following: own.following.clone(),
        wave: own.wave.clone(),
        pointer: own.pointer.map(|pointer| ShownPoint {
            tab: pointer.tab.to_string(),
            at: [pointer.x.into(), pointer.y.into()],
            age_ms: 0.0,
        }),
        availability: own.availability.id().to_owned(),
        call: own.call.as_ref().map(|call| call.call.encode()),
        ..Shown::default()
    };
    of_shell(&mut shown, own.shell.as_deref());
    shown
}
