//! Who is here, and where: the people a workspace's collaborators name, the
//! window each is in, and the calls in progress in its room. This is what the
//! empty launcher lists first.

use tilo_desktop_search::providers::happening::{CallHere, PersonHere, WindowHere, WorkspaceHere};

use crate::Shared;

impl Shared {
    /// Who is in each workspace now, and the calls in progress there, as
    /// the desk's collaborators and the presence say: what the empty palette
    /// lists first, by workspace, the one in front first.
    pub(crate) fn here_now(&self) -> Vec<WorkspaceHere> {
        let calls = self
            .sync
            .as_ref()
            .map(|sync| sync.calls())
            .unwrap_or_default();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        self.handle.read(|tree| {
            let mut workspaces: Vec<WorkspaceHere> = tree
                .workspaces
                .iter()
                .map(|workspace| {
                    let room = workspace.room().to_string();
                    let here_person = |person: &tilo_desktop_model::Collaborator| PersonHere {
                        id: person.id.clone(),
                        name: person.name.clone(),
                        colour: person
                            .cursor_color
                            .as_deref()
                            .and_then(ui::presence::parse_hex_color)
                            .unwrap_or_else(|| ui::presence::user_color(&person.id)),
                        agent: tilo_desktop::collaborators::collaborator_is_agent(
                            Some(workspace),
                            person,
                            now,
                        ),
                        // Idle and away dim their face everywhere else; the
                        // launcher says it in words.
                        away: person
                            .availability
                            .as_deref()
                            .and_then(ui::Availability::from_id)
                            .is_some_and(ui::Availability::dims),
                        tab: person.active_tab,
                    };
                    let mut people: Vec<PersonHere> = workspace
                        .collaborators
                        .iter()
                        .filter(|person| !person.invited && !person.own_device)
                        .map(&here_person)
                        .collect();
                    people.sort_by_key(|person| person.agent);
                    let mut windows: Vec<WindowHere> = Vec::new();
                    for person in people.iter().filter(|person| person.tab.is_some()) {
                        let Some(tab) = person.tab else { continue };
                        let title = tab_title(workspace, tab);
                        if title.is_empty() {
                            continue;
                        }
                        match windows.iter_mut().find(|window| window.tab == tab) {
                            Some(window) => window.people.push(person.clone()),
                            None => windows.push(WindowHere {
                                tab,
                                title,
                                people: vec![person.clone()],
                            }),
                        }
                    }
                    let calls = calls
                        .iter()
                        .filter(|call| call.room == room)
                        .map(|call| CallHere {
                            channel: call.channel.clone(),
                            name: call.name.clone(),
                            people: call
                                .people
                                .iter()
                                .map(|caller| {
                                    // Who they are on the desk, when it lists
                                    // them: the same face and name.
                                    workspace
                                        .collaborators
                                        .iter()
                                        .find(|person| person.id == caller.id)
                                        .map_or_else(
                                            || PersonHere {
                                                id: caller.id.clone(),
                                                name: caller.label.clone(),
                                                colour: ui::presence::user_color(&caller.id),
                                                agent: false,
                                                away: false,
                                                tab: None,
                                            },
                                            &here_person,
                                        )
                                })
                                .collect(),
                        })
                        .collect();
                    WorkspaceHere {
                        id: workspace.id.0,
                        room,
                        name: workspace.name.clone(),
                        active: workspace.id == tree.active,
                        people,
                        windows,
                        calls,
                    }
                })
                .filter(|workspace| !workspace.people.is_empty() || !workspace.calls.is_empty())
                .collect();
            workspaces.sort_by_key(|workspace| !workspace.active);
            workspaces
        })
    }
}

/// What window `tab` of `workspace` is called, when the workspace shows it.
fn tab_title(workspace: &tilo_desktop_model::Workspace, tab: u64) -> String {
    workspace
        .strips
        .iter()
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .find(|candidate| candidate.id == tab)
        .map(|candidate| candidate.title.trim().to_owned())
        .unwrap_or_default()
}
