//! What the person lets their devices say about them: how what they are doing is shared, and what
//! the people in each workspace see of them. The choice is theirs alone, kept with them
//! ([`settings_store::presence`]) so every device and every workspace reads the same one; this is
//! where the session asks what it is.

use collab_types::presence::ShellPresence;

/// How this person shares what they are doing.
pub(crate) fn sharing() -> ui::StatusSharing {
    let sharing = settings_store::presence::SHARING.get();
    ui::StatusSharing::from_word(&sharing).unwrap_or_default()
}

/// What the people in `workspace` see of this person: their choice for it, and where they are for
/// one they never chose for.
pub(crate) fn visibility(workspace: &str) -> ui::PresenceVisibility {
    settings_store::presence::VISIBILITY
        .get()
        .get(workspace)
        .and_then(|id| ui::PresenceVisibility::from_id(id))
        .unwrap_or(ui::PresenceVisibility::WhereIAm)
}

/// `shell` as the people in a workspace that sees only the person's face read it: their face, and
/// nothing of the window they are in, what they select or what they say beside their pointer.
pub(crate) fn face_only(mut shell: Box<ShellPresence>) -> Box<ShellPresence> {
    shell.place = None;
    shell.windows.clear();
    shell.selection = None;
    shell.chat = None;
    shell.reaction = None;
    shell
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell() -> ShellPresence {
        ShellPresence {
            seq: 3,
            sent_at_ms: 1_700_000_000_000,
            label: "Ada".to_owned(),
            agent: false,
            colour: "#3355ff".to_owned(),
            activity: None,
            place: Some("strip-1".to_owned()),
            size: [1280, 800],
            windows: vec![collab_types::presence::ShellWindow {
                app: "so.tilo.notes".to_owned(),
                document: [1; 8],
                rect: [0, 0, 1000, 800],
                focused: true,
            }],
            selection: Some(collab_types::presence::ShellSelection {
                anchor: "a".to_owned(),
                head: "b".to_owned(),
                composing: 0,
            }),
            chat: Some(collab_types::presence::ShellChat {
                text: "here".to_owned(),
                sent: true,
            }),
            reaction: None,
        }
    }

    #[test]
    fn a_workspace_that_sees_only_the_face_learns_nothing_of_where_they_are() {
        let face = face_only(Box::new(shell()));
        assert_eq!(face.label, "Ada");
        assert_eq!(face.colour, "#3355ff");
        assert!(face.place.is_none());
        assert!(face.windows.is_empty(), "no window of theirs is shown");
        assert!(face.selection.is_none(), "nothing of what they select");
        assert!(face.chat.is_none(), "nothing they say beside the pointer");
    }
}
