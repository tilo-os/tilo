//! The collaboration a workspace brings: what its members put on the desk, the calls they are in,
//! the invitations they redeemed, whose view this device follows, and where this person is on the
//! desk for everyone here to read.

use crate::sync::layout::live_record_as;
use collab_types::presence::{ShellPresence, ShellWindow};
use tilo_desktop_surface::navigation;
use tilo_live::presence::{Point, Tile, anchor};

use crate::Session;

impl Session {
    /// Keep the workspaces this session takes part in current with the desktop's, and hand the
    /// desktop who is here.
    pub(crate) fn follow_collaboration(&self) {
        let Some(sync) = &self.shared.sync else {
            return;
        };
        self.shared.handle.read(|tree| sync.follow_desktop(tree));
        sync.follow();
        // This device's call, as everyone in the workspace reads it, and
        // theirs as this session's apps show them: a join or leave reaches a
        // channel's roster on the pass it happened on.
        if let Some(plane) = &self.action_plane {
            plane.follow_calls(sync);
            // Where everyone else here types, in this device's windows on the same record.
            let workspaces = self.shared.handle.read(|tree| {
                tree.workspaces
                    .iter()
                    .map(tilo_desktop_model::Workspace::id)
                    .filter(|id| !id.is_nil())
                    .map(|id| id.to_string())
                    .collect()
            });
            plane.carets.follow(sync, workspaces);
            // Who is here in the workspace in front, named and coloured as the bar and People
            // draw them: the apps there are told, so a caret wears its person's face.
            if let Some((workspace, _)) = sync.front_members() {
                let people = sync
                    .here()
                    .borrow()
                    .persons()
                    .into_iter()
                    .map(|(id, name, (r, g, b))| tilo_desktop_model::Collaborator {
                        id,
                        name,
                        cursor_color: Some(format!("#{r:02x}{g:02x}{b:02x}")),
                        ..tilo_desktop_model::Collaborator::default()
                    })
                    .collect();
                plane.desk.set_people(&workspace, people);
            }
        }
        // Whose view this device follows: their window here comes forward each time the place they
        // work in moves, and their app where no window here shows it.
        self.shared.follow_people();
        // Where this person is on this desk, so a page here draws their pointer, follows them to the
        // record they work in and shows a mini view of their screen.
        self.say_where_i_am(sync);
        // The agents the person admitted at this device's door are here with them.
        self.say_agents_here(sync);
        // What became of the invitations redeemed is the directory's to say, and the desk's to
        // show.
        self.shared.follow_enrollments();
    }

    /// Say where this person is on this desk, in the workspace in front: the pointer over the tile
    /// it is on, and their shell — its face, its screen and the window they work in, each window
    /// named by its app and its record's short digest only. Said again only when one of them moved,
    /// so a desk at rest publishes nothing.
    fn say_where_i_am(&self, sync: &crate::sync::Client) {
        let shown = self.shared.handle.shown_tiles();
        let area = self.shared.handle.shown_strip().map(|(_, area, _)| area);
        let (place, focused) = self.shared.handle.read(|tree| {
            (
                tree.active_workspace()
                    .and_then(|workspace| navigation::active_strip(tree, workspace))
                    .map(|strip| strip.0.to_string()),
                navigation::active_route(tree).map(|route| route.tab),
            )
        });
        let tiles: Vec<Tile> = shown
            .iter()
            .map(|&(tab, rect)| Tile::new(tab, rect.x0, rect.y0, rect.width(), rect.height()))
            .collect();
        let pointer = anchor(&tiles, self.compositor.pointer_location()).map(|(tab, x, y)| Point {
            tab,
            x,
            y,
        });
        let own = sync.own();
        let (label, identity) = crate::people::own_face(&own, &self.shared.person);
        let (r, g, b) = crate::people::own_rgb(&identity);
        let carets = self.action_plane.as_ref().map(|plane| &plane.carets);
        let said = |app: &str| carets.and_then(|carets| carets.said_in(app));
        let shell = self.shared.handle.read(|tree| {
            shell(
                tree,
                &self.shared.notes,
                &shown,
                area,
                &Focus {
                    place: place.clone(),
                    focused,
                    label: label.clone(),
                    colour: format!("#{r:02x}{g:02x}{b:02x}"),
                    said: &said,
                },
            )
        });
        sync.set_where(pointer, shell);
    }

    /// Speak for the agents the person admitted at this device's door, each in the workspace it
    /// works in: a member of its own there, listed beside everyone else for everyone there, in its
    /// own colour, saying what it is doing and the window of the app it does it in.
    fn say_agents_here(&self, sync: &crate::sync::Client) {
        use tilo_session_client::narration::lines;
        let seated = self.shared.seats.here();
        let runs: Vec<(String, String, ShellPresence)> = self.shared.handle.read(|tree| {
            seated
                .into_iter()
                .map(|seated| {
                    let (line, focus) = match &seated.doing {
                        // The window of the app it acts in, and what that window is called: the
                        // note, the sheet, the meeting. What it does is said on that record, as
                        // the window's own activity cards say it.
                        Some(doing) => {
                            let window = doing.app.as_deref().and_then(|app| {
                                app_window(tree, &self.shared.notes, &seated.workspace, app)
                            });
                            let step = tilo_actions::Step::new(
                                doing.step.unwrap_or(tilo_actions::StepKind::Work),
                                window.as_ref().map(|(_, title)| title.as_str()),
                            )
                            .in_app(doing.app.as_deref());
                            (lines::step(&step), window.map(|(record, _)| record))
                        }
                        None => (lines::waiting(), None),
                    };
                    // The colour the agent wears wherever it shows: the one what it was admitted as
                    // gives it, never the run it works in, and clear of the colour of the person it
                    // works for — the one rule and the one key every surface asks (the workspace's
                    // member list keys it so for its card in a window and its marks).
                    let (r, g, b) = ui::presence::participant_color(
                        &seated.agent,
                        true,
                        Some(&self.shared.person),
                    );
                    let face = tilo_session_client::presence::view(
                        0,
                        ui::time::unix_now_ms(),
                        &seated.name,
                        [r, g, b],
                        Some(line),
                        focus.as_ref(),
                        None,
                    );
                    (seated.workspace, seated.agent, face)
                })
                .collect()
        });
        sync.speak_for(&runs);
    }
}

/// The window of `app` in `workspace` an agent at work there is placed in, by the record it shows,
/// and what the window is called (the note's title, the sheet's name): where an agent at work in
/// that app is, and the record it says it works in.
fn app_window(
    tree: &tilo_desktop_model::DesktopTree,
    notes: &crate::continuations::Continuations,
    workspace: &str,
    app: &str,
) -> Option<(tilo_relay_protocol::live::LiveRecord, String)> {
    let records = notes.records();
    let held = tree
        .workspaces
        .iter()
        .find(|held| held.id().to_string() == workspace)?;
    held.tabs().find_map(|tab| {
        let record = live_record_as(
            tree,
            tab.id,
            records.get(&tab.id)?,
            notes.app_of(tab.id).as_deref(),
        )?;
        (record.app == app).then(|| (record, tab.title.trim().to_owned()))
    })
}

/// What the shell says about the person and the screen it is of, beside the desk itself.
struct Focus<'a> {
    /// The strip they work in, as the workspace names it.
    place: Option<String>,
    /// The window in front, when a window is.
    focused: Option<tilo_desktop_model::TabId>,
    /// What they are called here.
    label: String,
    /// Their colour here, as `#rrggbb`.
    colour: String,
    /// Where they type in a window of the app named, as their presence says it.
    said: &'a dyn Fn(&str) -> Option<collab_types::presence::ShellSelection>,
}

/// Where this person is on this desk, as presence says it to the others: their screen's size and
/// the windows on it, each by its app and its record's short digest only, and which of them they
/// work in — the layout of `area`, the screen's content, by the rule native shells and pages both
/// place by. `None` while the desk shows no screen to place anything on.
fn shell(
    tree: &tilo_desktop_model::DesktopTree,
    notes: &crate::continuations::Continuations,
    shown: &[(tilo_desktop_model::TabId, ui::kurbo::Rect)],
    area: Option<ui::kurbo::Rect>,
    focus: &Focus<'_>,
) -> Option<Box<ShellPresence>> {
    let records = notes.records();
    let layout = crate::sync::layout::placed(
        area,
        shown,
        |tab| {
            let record =
                live_record_as(tree, tab, records.get(&tab)?, notes.app_of(tab).as_deref());
            record.map(|record| (record.app, record.record))
        },
        focus.focused,
    )?;
    Some(Box::new(ShellPresence {
        // The board numbers each view it says: this one only says what changed.
        seq: 0,
        sent_at_ms: ui::time::unix_now_ms(),
        label: focus.label.clone(),
        agent: false,
        colour: focus.colour.clone(),
        activity: None,
        place: focus.place.clone(),
        size: [layout.size[0].into(), layout.size[1].into()],
        windows: layout
            .windows
            .iter()
            .map(|window| ShellWindow {
                app: window.app.clone(),
                record: window.record,
                rect: window.rect,
                focused: window.focused,
            })
            .collect(),
        // Where they type, in the window they work in.
        selection: layout
            .windows
            .iter()
            .find(|window| window.focused)
            .and_then(|window| (focus.said)(&window.app)),
        chat: None,
        reaction: None,
    }))
}

#[cfg(test)]
mod tests {
    use collab_types::PeerPresencePayload;
    use collab_types::presence::{Payload, Presence};
    use tilo_desktop_model::{
        DesktopTree, GroupId, Strip, StripId, Tab, TabId, TileContent, WindowGroup, WindowWidth,
        Workspace, WorkspaceId, WorkspaceSlot,
    };
    use tilo_relay_protocol::live::LiveRecord;
    use tilo_session_core::presence::{Follow, Place, Step, Target};
    use ui::kurbo::Rect;

    use super::{Focus, shell};
    use crate::sync::{Client, OwnPresence};

    const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";
    /// The tab of the Notes window, and the record it shows.
    const NOTES: u64 = 7;
    const RECORD: &str = "notes-1";
    const NOTES_APP: &str = "so.tilo.notes";

    fn device(seed: u8) -> iroh::EndpointId {
        iroh::SecretKey::from_bytes(&[seed; 32]).public()
    }

    /// A desk showing one workspace, with one window called Notes showing [`RECORD`].
    fn desk() -> DesktopTree {
        let workspace = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
        let slot = WorkspaceSlot::of_workspace(workspace);
        let mut workspace = Workspace::in_workspace(slot, workspace, "Desk");
        let mut strip = Strip::new(StripId::from(1), "Main");
        strip.window_groups.push(WindowGroup {
            id: GroupId::from(1_u64),
            tabs: vec![Tab {
                id: TabId::from(NOTES),
                title: "Notes".to_owned(),
                tile: TileContent::LocalApp {
                    app_id: NOTES_APP.to_owned(),
                    label: "Notes".to_owned(),
                    note: String::new(),
                },
                app_title: None,
                view: String::new(),
                pinned: false,
            }],
            active_tab: TabId::from(NOTES),
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        workspace.strips.push(strip);
        DesktopTree {
            workspaces: vec![workspace.clone()],
            active: workspace.slot,
            ..DesktopTree::default()
        }
    }

    /// The window `tab` stands at on the screen's content area, as the desk laid it out.
    fn standing(tab: u64) -> (u64, Rect) {
        (tab, Rect::new(32.0, 80.0, 1248.0, 788.0))
    }

    fn own(label: &str) -> OwnPresence {
        OwnPresence {
            label: label.to_owned(),
            ..OwnPresence::default()
        }
    }

    /// One presence the other device said in the workspace.
    fn said(from: u8, payload: PeerPresencePayload) -> (iroh::EndpointId, Presence) {
        let payload = match payload {
            PeerPresencePayload::Arrived => Payload::Arrived,
            PeerPresencePayload::Person { name } => Payload::Person { name },
            PeerPresencePayload::Shell(shell) => Payload::Shell(shell),
            other => panic!("the tests say no {other:?} as a desk"),
        };
        let _ = from;
        (device(from), Presence::desktop(WORKSPACE, payload))
    }

    /// The shell this desk says it is: what it shows, where the person works on it and what they
    /// are called here.
    fn said_shell() -> Box<collab_types::presence::ShellPresence> {
        let tree = desk();
        let notes = named(&[(NOTES, RECORD)]);
        shell(
            &tree,
            &notes,
            &[standing(NOTES)],
            Some(Rect::new(0.0, 36.0, 1280.0, 800.0)),
            &Focus {
                place: Some("1".to_owned()),
                focused: Some(NOTES),
                label: "Quinten".to_owned(),
                colour: "#3b82f6".to_owned(),
                said: &|_| None,
            },
        )
        .expect("the desk has a screen to place its windows on")
    }

    /// The desk's windows as their apps named them, with no app said: read from the tree.
    fn named(held: &[(u64, &str)]) -> crate::continuations::Continuations {
        let notes = crate::continuations::Continuations::default();
        for (tab, record) in held {
            notes.note(*tab, NOTES_APP, WORKSPACE, record);
        }
        notes
    }

    /// An agent's place is the window of the app it acts in, named as that window is: a page here
    /// following it opens the record and reads the same title beside its face.
    #[test]
    fn an_agents_place_names_the_record_its_window_shows() {
        let tree = desk();
        let notes = named(&[(NOTES, RECORD)]);
        let (record, title) = super::app_window(&tree, &notes, WORKSPACE, NOTES_APP)
            .expect("the Notes window is there");
        assert_eq!(record.app, NOTES_APP);
        assert_eq!(record.record, RECORD);
        assert_eq!(title, "Notes", "what the window is called");
        assert_eq!(
            super::app_window(&tree, &notes, WORKSPACE, "so.tilo.sheets"),
            None,
            "no window of an app that shows none"
        );
        // Without the window's record, nothing names where it works: never a wrong window.
        assert_eq!(
            super::app_window(
                &tree,
                &crate::continuations::Continuations::default(),
                WORKSPACE,
                NOTES_APP
            ),
            None
        );
    }

    /// A desk says where this person is: the app and the record the window they work in shows, so
    /// a page here draws them in it. A page showing the same record names that window as theirs,
    /// and following them brings it forward.
    #[test]
    fn a_desk_says_where_this_person_works_and_a_page_follows_them_to_it() {
        let shell = said_shell();
        let [window] = shell.windows.as_slice() else {
            panic!("one window on the desk: {:?}", shell.windows);
        };
        assert_eq!(window.app, NOTES_APP);
        assert_eq!(
            window.record,
            LiveRecord {
                app: NOTES_APP.to_owned(),
                record: RECORD.to_owned(),
            }
            .short_digest(),
            "the record's short digest, as presence names it"
        );
        assert!(window.focused, "the window they work in");
        assert!(shell.size[0] > 0 && shell.size[1] > 0, "its screen");
        assert_eq!(shell.label, "Quinten", "their name here");
        assert_eq!(shell.colour, "#3b82f6", "their colour here");
        assert!(!shell.agent, "a person, not an agent");

        // The page shows the same record in the same window, and hears them say where they are.
        let (page, _said) = Client::offline(device(0xaa), own("Ben"));
        page.follow_desktop(&desk());
        for greeting in [
            PeerPresencePayload::Person {
                name: "Quinten".to_owned(),
            },
            PeerPresencePayload::Arrived,
        ] {
            let (from, presence) = said(0xf0, greeting);
            page.heard_presence(WORKSPACE, from, &presence);
        }
        let (from, presence) = said(0xf0, PeerPresencePayload::Shell(shell));
        page.heard_presence(WORKSPACE, from, &presence);
        let notes = named(&[(NOTES, RECORD)]);
        let people = page.people(&desk(), &notes, &|_: &str| Some("Notes".to_owned()));
        let them = people
            .iter()
            .find(|person| person.id == device(0xf0).to_string())
            .expect("they are here");
        assert_eq!(
            them.tab,
            Some(NOTES),
            "the window their record shows is theirs"
        );
        let mut follow = Follow::default();
        follow.follow(Some(them.id.clone()));
        let places: Vec<Place<'_>> = people
            .iter()
            .map(|person| Place {
                id: &person.id,
                tab: person.tab,
                app: None,
            })
            .collect();
        assert_eq!(follow.step(&places), Step::Bring(Target::Window(NOTES)));
        assert_eq!(follow.step(&places), Step::Rest, "brought forward once");
    }
}
