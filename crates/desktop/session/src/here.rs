//! Who is here, and where: the members a workspace lists, the window each is in, and the calls in
//! progress in its workspace. This is what the empty launcher lists first.

#![cfg_attr(
    target_arch = "wasm32",
    allow(dead_code, reason = "the page's collaboration feeds this next")
)]

use tilo_desktop_search::providers::happening::{CallHere, PersonHere, WindowHere, WorkspaceHere};

use crate::Shared;

impl Shared {
    /// The person's layout, with who else is here in the workspace in front as the live session lists
    /// them: what the shell's bar shows.
    #[cfg(target_os = "linux")]
    pub(crate) fn layout_reading(&self) -> tilo_session_core::shell::LayoutReading {
        let mut reading = self.layout.borrow().reading();
        if let Some(sync) = &self.sync {
            reading.people = self
                .handle
                .read(|tree| {
                    sync.people(tree, &self.notes, &|app: &str| {
                        Some(crate::app_names::app_name(app))
                    })
                })
                .into_iter()
                .map(|person| tilo_session_core::shell::LayoutPersonReading {
                    key: person.id,
                    name: person.label,
                })
                .collect();
        }
        reading
    }

    /// Who is in each workspace now, and the calls in progress there, as the shell's member list and
    /// the presence say: what the empty palette lists first, by workspace, the one in front first.
    pub(crate) fn here_now(&self) -> Vec<WorkspaceHere> {
        let calls = self
            .sync
            .as_ref()
            .map(|sync| sync.calls())
            .unwrap_or_default();
        let joined = self.calls_this_person_is_in();
        // The tree as the loop holds it lists nobody: who is here is placed into the copy a
        // surface reads, as the bar places it into its own.
        let tree = self.handle.read(Clone::clone);
        #[cfg(target_os = "linux")]
        let tree = {
            let mut tree = tree;
            if let Some(sync) = &self.sync {
                sync.show(&mut tree, &self.notes, &|app: &str| {
                    Some(crate::app_names::app_name(app))
                });
            }
            tree
        };
        let mut workspaces: Vec<WorkspaceHere> = tree
            .workspaces
            .iter()
            .map(|workspace| {
                workspace_here(workspace, workspace.id() == tree.active, &calls, &joined)
            })
            .filter(|workspace| !workspace.people.is_empty() || !workspace.calls.is_empty())
            .collect();
        workspaces.sort_by_key(|workspace| !workspace.active);
        workspaces
    }
}

/// Who is in `workspace` now, the windows they are in and the calls in progress there that this
/// person is not in (`joined`); `active` when it is the workspace in front.
fn workspace_here(
    workspace: &tilo_desktop_model::Workspace,
    active: bool,
    calls: &[crate::sync::LiveCall],
    joined: &[String],
) -> WorkspaceHere {
    let workspace_id = workspace.id().to_string();
    let here_person = |member: &collab_types::Member| {
        let here = workspace
            .present
            .iter()
            .find(|here| here.member == member.id);
        PersonHere {
            // An agent by whose it is as well as which: another person's agent of the same id
            // is another member.
            id: if member.id.is_agent() {
                crate::search::recipient_id(&member.id)
            } else {
                member.id.id().to_owned()
            },
            name: member.name.clone(),
            colour: collab_types::presence::identity_rgb(member.colour),
            agent: !member.id.is_person(),
            // Idle and away dim their face everywhere else; the launcher says it
            // in words.
            away: here.is_some_and(away),
            tab: here.and_then(|here| here.tab),
        }
    };
    // The members here now: the person at this desktop and those away from
    // the workspace are not.
    let mut people: Vec<PersonHere> = workspace
        .members
        .iter()
        .filter(|member| {
            workspace
                .present
                .iter()
                .any(|here| here.member == member.id)
        })
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
        .filter(|call| !joined.contains(&call.channel))
        .filter(|call| call.workspace == workspace_id)
        .map(|call| CallHere {
            channel: call.channel.clone(),
            name: call.name.clone(),
            people: call
                .people
                .iter()
                .map(|caller| {
                    // Who they are on the shell, when it lists them: the same face
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
        name: tilo_session_core::desktop::names::workspace(&workspace.name),
        active,
        people,
        windows,
        calls,
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

/// Whether the shell's row for someone says they are away from their machine: what dims their face,
/// and what the launcher says in words.
fn away(here: &tilo_desktop_model::Present) -> bool {
    here.availability.dims()
}

/// What window `tab` of `workspace` is called, by the one rule every surface names a window by,
/// when the workspace shows it.
fn tab_title(workspace: &tilo_desktop_model::Workspace, tab: u64) -> String {
    workspace
        .strips
        .iter()
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .find(|candidate| candidate.id == tab)
        .map(crate::app_names::tab_name)
        .unwrap_or_default()
}
