//! Who is here, and where: the members a workspace lists, the window each is in, and the calls in
//! progress in its workspace. This is what the empty launcher lists first.

#![cfg_attr(
    target_arch = "wasm32",
    allow(dead_code, reason = "the page's collaboration feeds this next")
)]

use tilo_desktop_search::providers::happening::{CallHere, PersonHere, WindowHere, WorkspaceHere};

use crate::Shared;

impl Shared {
    /// Who is in each workspace now, and the calls in progress there, as the desk's member list and
    /// the presence say: what the empty palette lists first, by workspace, the one in front first.
    pub(crate) fn here_now(&self) -> Vec<WorkspaceHere> {
        let calls = self
            .sync
            .as_ref()
            .map(|sync| sync.calls())
            .unwrap_or_default();
        let joined = self.calls_this_person_is_in();
        self.handle.read(|tree| {
            let mut workspaces: Vec<WorkspaceHere> = tree
                .workspaces
                .iter()
                .map(|workspace| {
                    let workspace_id = workspace.id().to_string();
                    let here_person = |member: &collab_types::Member| {
                        let here = workspace
                            .present
                            .iter()
                            .find(|here| here.member == member.id);
                        PersonHere {
                            id: here.map_or_else(|| member.id.key(), |here| here.device.clone()),
                            name: member.name.clone(),
                            colour: collab_types::presence::identity_rgb(member.colour),
                            agent: !member.id.is_person(),
                            // Idle and away dim their face everywhere else; the launcher says it
                            // in words.
                            away: here.is_some_and(away),
                            tab: here.and_then(|here| here.tab),
                        }
                    };
                    let mut people: Vec<PersonHere> =
                        workspace.members.iter().map(&here_person).collect();
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
                        .filter(|call| !joined.contains(&call.channel))
                        .filter(|call| call.workspace == workspace_id)
                        .map(|call| CallHere {
                            channel: call.channel.clone(),
                            name: call.name.clone(),
                            people: call
                                .people
                                .iter()
                                .map(|caller| {
                                    // Who they are on the desk, when it lists them: the same face
                                    // and name.
                                    workspace
                                        .present
                                        .iter()
                                        .find(|here| here.device == caller.id)
                                        .and_then(|here| {
                                            workspace
                                                .members
                                                .iter()
                                                .find(|member| member.id == here.member)
                                        })
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
                        workspace: workspace_id,
                        name: tilo_desktop_surface::names::workspace(&workspace.name),
                        active: workspace.id() == tree.active,
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

impl Shared {
    /// The calls this person is in, on any of their devices: what a Join is not offered for.
    fn calls_this_person_is_in(&self) -> Vec<String> {
        let Some(sync) = self.sync.as_ref() else {
            return Vec::new();
        };
        sync.calls()
            .iter()
            .map(|call| call.channel.clone())
            .chain(sync.own().call.map(|call| call.call.channel))
            .filter(|channel| sync.is_in_call(channel))
            .collect()
    }
}

/// Whether the desk's row for someone says they are away from their machine: what dims their face,
/// and what the launcher says in words.
fn away(here: &tilo_desktop_model::Present) -> bool {
    here.availability.dims()
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
