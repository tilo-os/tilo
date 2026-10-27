//! The launcher's busy state: people in two workspaces, a call ringing and
//! something shared with the person, and what they opened lately.
use std::time::Duration;

use super::super::ShellDriver;

impl ShellDriver {
    /// The demo's invented people gone from every workspace: a device with
    /// nobody else on it yet.
    pub fn seed_launcher_fresh_for_shots(&self) {
        let handle = self.chrome_handle();
        let st = handle.borrow();
        let mut tree = st.tree_mut();
        tree.desktop.collaborators.clear();
        for workspace in &mut tree.desktop.workspaces {
            workspace.collaborators.clear();
        }
    }

    /// Ada and Bo in Home, Ada in its first window; Alice and Bob in
    /// Research, Alice in its design call; Bo calling and Lena's share from Research waiting; Notes
    /// windows on three notes in Home over the last day and one in
    /// Research.
    pub fn seed_launcher_busy_for_shots(&mut self) {
        use tilo_desktop::notifications::{Addressed, Notification};
        self.seed_search_sources();
        self.fb.host_mut().set_palette_query("");
        self.fb.host_mut().set_palette_visible(false);
        self.seed_shared_room();
        let handle = self.chrome_handle();
        let (home_room, research) = {
            let st = handle.borrow();
            let mut tree = st.tree_mut();
            let research = tree
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.name == "Research")
                .map(|workspace| (workspace.id, workspace.room().to_string()));
            let Some(home) = tree.desktop.active_workspace_mut() else {
                return;
            };
            let first_tab = home
                .strips
                .first()
                .and_then(|strip| strip.window_groups.first())
                .map(|group| group.active_tab);
            // The demo's invented Alice is Research's, not Home's.
            home.collaborators.retain(|person| person.id != "alice");
            for person in &mut home.collaborators {
                if person.id == "ada" {
                    person.active_tab = first_tab;
                }
            }
            let home_room = home.room().to_string();
            drop(tree);
            (home_room, research)
        };
        self.with_chrome(|st| {
            st.push_notification_full(
                Notification::new("Bo is calling", "")
                    .with_app_name("Calls")
                    .with_from("Bo", "bo")
                    .with_addressed(Addressed::IncomingCall)
                    .with_dismiss_after(Duration::from_secs(600)),
            );
            let mut shared = Notification::new("Lena shared “Q3 plan”", "You can edit it.")
                .with_app_name("Files")
                .with_from("Lena", "lena")
                .with_created_at(
                    std::time::Instant::now()
                        .checked_sub(Duration::from_secs(14 * 60))
                        .unwrap_or_else(std::time::Instant::now),
                );
            shared.source_workspace = research.as_ref().map(|(id, _)| id.0);
            let id = st.push_notification_full(shared);
            // Seen as a banner and left for later: in the notification
            // centre, still unanswered.
            st.dismiss_notification(id);
        });
        // Where Notes windows were, as their bars said: what Continue lists.
        let opened = |room: &str, notes: &[(&str, &str, u64)]| {
            for (key, title, minutes_ago) in notes {
                shell::shell::search::recent_places::visit_ago(
                    "so.tilo.notes",
                    room,
                    0,
                    title,
                    Some(key),
                    Duration::from_secs(minutes_ago.saturating_mul(60)),
                );
            }
        };
        opened(
            &home_room,
            &[
                ("pitch", "Pitch deck", 4),
                ("launch", "Launch plan", 75),
                ("interviews", "Interview notes", 60 * 26),
            ],
        );
        if let Some((_, room)) = &research {
            opened(room, &[("results", "Lab results", 30)]);
            // Alice is in Research's design call, as she says there: a
            // call in a workspace not in front.
            self.fb.apply_remote_presence_for_shots(
                "alice".into(),
                ui::presence::PresenceKey::Workspace { id: room.clone() },
                ui::collaboration::PresencePayload::Availability {
                    state: ui::Availability::Available,
                    call: Some(
                        ui::CallPresence {
                            channel: "c_design".into(),
                            name: "design".into(),
                        }
                        .encode(),
                    ),
                },
            );
        }
        self.with_chrome(|_| {});
    }
}
