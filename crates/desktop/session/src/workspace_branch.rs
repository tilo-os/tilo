//! Branching a workspace: a new workspace of the person's own that starts with everything the
//! source holds and goes on apart from it (`docs/architecture/workspace.md`, "Branching a
//! workspace").
//!
//! The shell names the branch and its id the moment it is asked (`branch-workspace`,
//! [`tilo_session_core::desktop::host::contract::DesktopAsk::BranchWorkspace`]). The workspace is made by this
//! device's daemon, where the person's workspaces are kept (`account/workspaces#branch`, which
//! asks the workspace service's `workspace.branch-own`): it copies the source's records, their
//! content and its machine, all or nothing, and leaves behind what is tied to other people. The
//! session waits for that answer off its loop, then puts the branch on the shell with the
//! source's strips and a window for each of the source's windows of this device, which open again
//! in the branch. Nothing opens in it before its records are there, so no app founds an empty
//! record the copy would then collide with.

use std::path::Path;
use std::sync::Arc;

use tilo_desktop_model::{
    Layout, Strip, StripId, Tab, TileContent, WindowGroup, Workspace, WorkspaceId,
};
use tilo_session_core::desktop::notifications::NotificationIconKind;

use crate::engine_write::Polled;
use crate::platform::notice::Notice;

/// Whether `request` is a branch of a workspace other than `workspace` , or of one the session does
/// not know: `workspaces` are the session's, each by its id and its name. An agent reads only the workspace it works in, so it branches that one or none.
pub(crate) fn branches_elsewhere(
    request: &tilo_actions::remote::InvokeRequest,
    workspaces: &[[String; 2]],
    workspace: &str,
) -> bool {
    let address = &request.target.address;
    if address.resource != tilo_session_core::desktop::verbs::CHROME_RESOURCE
        || address.action != tilo_session_core::desktop::host::contract::BRANCH_WORKSPACE
    {
        return false;
    }
    let source = match &request.argument {
        tilo_actions::remote::InvokeArgument::Json(arguments) => arguments
            .get("source")
            .and_then(serde_json::Value::as_str)
            .map(str::trim),
        _ => None,
    };
    let Some(source) = source else {
        return true;
    };
    // As the session resolves it: by name first, then by id.
    workspaces
        .iter()
        .find(|[_, name]| name.eq_ignore_ascii_case(source))
        .or_else(|| workspaces.iter().find(|[id, _]| id == source))
        .is_none_or(|[id, _]| id != workspace)
}

/// A branch being made: what the shell asked, and the daemon's answer under way.
pub(crate) struct Branching {
    source: WorkspaceId,
    source_name: String,
    workspace: WorkspaceId,
    name: String,
    made: Polled<Result<(), String>>,
}

/// The branches being made, followed on every pass.
#[derive(Default)]
pub(crate) struct Branches(std::cell::RefCell<Vec<Branching>>);

/// The branch's own session: `source`'s strips, and a window for each of the windows this device
/// shows in it — opened again in the branch by its app — with ids of its own (`mint`). Other
/// people's windows stay theirs. What the workspace grants its apps comes along, its own files
/// named as the branch's.
pub(crate) fn shell_of(
    source: &Workspace,
    workspace: WorkspaceId,
    name: &str,
    files: Option<&Path>,
    mint: &mut dyn FnMut() -> u64,
) -> Workspace {
    let mut branch = Workspace::new(workspace, name);
    let trees = files.map(|files| {
        (
            tilo_paths::workspace_tree(files, &source.id().to_string()),
            tilo_paths::workspace_tree(files, &workspace.to_string()),
        )
    });
    branch.sandbox = source
        .sandbox
        .iter()
        .cloned()
        .map(|mut grant| {
            if let (collab_types::caps::Scope::FsPath(path), Some((Some(from), Some(to)))) =
                (&grant.scope, &trees)
                && let Ok(rest) = Path::new(path).strip_prefix(from)
            {
                grant.scope =
                    collab_types::caps::Scope::FsPath(to.join(rest).to_string_lossy().into_owned());
            }
            grant
        })
        .collect();
    branch.presets.clone_from(&source.presets);
    branch.default_apps = source.default_apps.clone();
    for strip in &source.strips {
        let mut made = Strip::new(StripId(mint()), strip.name.clone());
        for group in &strip.window_groups {
            let tabs: Vec<Tab> = group
                .tabs
                .iter()
                .filter_map(|tab| {
                    let app = match &tab.tile {
                        TileContent::Foreign { app_id, .. }
                        | TileContent::LocalApp { app_id, .. } => app_id.clone(),
                        _ => return None,
                    };
                    (!app.trim().is_empty()).then(|| Tab {
                        id: mint(),
                        title: tab.title.clone(),
                        tile: TileContent::LocalApp {
                            app_id: app,
                            label: tab.title.clone(),
                            note: String::new(),
                        },
                        app_title: None,
                        view: tab.view.clone(),
                        pinned: tab.pinned,
                    })
                })
                .collect();
            let Some(front) = tabs.first().map(|tab| tab.id) else {
                continue;
            };
            made.window_groups.push(WindowGroup {
                id: mint(),
                tabs,
                active_tab: front,
                width: group.width,
                maximized: false,
                focus_history: Vec::new(),
            });
        }
        branch.strips.push(made);
    }
    if branch.strips.is_empty() {
        branch.strips.push(Strip::new(StripId(mint()), "Main"));
    }
    branch
}

impl crate::Shared {
    /// The shell branched the workspace `source` into `workspace` , called `name` : ask this
    /// device's daemon to make it, without holding the loop.
    pub(crate) fn branch_workspace(
        &self,
        source: WorkspaceId,
        workspace: WorkspaceId,
        name: String,
    ) {
        let Some(source_name) = self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .find(|held| held.id() == source)
                .map(|held| held.name.clone())
        }) else {
            self.handle.acknowledge(tilo_session_core::desktop::i18n::t(
                "desktop-workspace-gone",
            ));
            return;
        };
        let asked = self.view.as_ref().map(|view| {
            let answer = view.ask_account(
                "account/workspaces",
                "branch",
                serde_json::Map::from_iter([
                    ("workspace".to_owned(), source.to_string().into()),
                    ("name".to_owned(), name.clone().into()),
                    ("id".to_owned(), workspace.to_string().into()),
                ]),
            );
            let view = Arc::clone(view);
            async move {
                answer.await?;
                // The branch goes on the shell once the daemon's reading of it is here.
                view.arrived(workspace).await;
                Ok(())
            }
        });
        let made = Polled::new(
            Arc::clone(&self.wake_handle),
            async move {
                match asked {
                    Some(asked) => asked.await,
                    None => Err("this session reaches no device daemon".to_owned()),
                }
            },
            || Err("the branch stopped before it answered".to_owned()),
        );
        self.telling().show(
            &Notice::new(
                String::new(),
                1,
                tilo_session_core::desktop::i18n::t_args(
                    "session-branching-workspace",
                    &[("name", source_name.clone().into())],
                ),
                tilo_session_core::desktop::i18n::t_args(
                    "session-branching-workspace-body",
                    &[("branch", name.clone().into())],
                ),
            )
            .about(NotificationIconKind::Workspace),
        );
        self.branches.0.borrow_mut().push(Branching {
            source,
            source_name,
            workspace,
            name,
            made,
        });
    }

    /// Put each branch the daemon made on the shell, with the source's strips and windows, and go
    /// there when the person is still where they asked for it; say why one was not made.
    pub(crate) fn follow_branches(&self) {
        let mut answered = Vec::new();
        self.branches
            .0
            .borrow_mut()
            .retain_mut(|branching| match branching.made.done() {
                Some(made) => {
                    answered.push((
                        branching.source,
                        branching.source_name.clone(),
                        branching.workspace,
                        branching.name.clone(),
                        made,
                    ));
                    false
                }
                None => true,
            });
        for (source, source_name, workspace, name, made) in answered {
            let (title, body) = match made {
                Ok(()) => {
                    self.land_branch(source, workspace, &name);
                    (
                        tilo_session_core::desktop::i18n::t_args(
                            "session-branched-workspace",
                            &[("branch", name.into())],
                        ),
                        tilo_session_core::desktop::i18n::t_args(
                            "session-branched-workspace-body",
                            &[("name", source_name.into())],
                        ),
                    )
                }
                Err(error) => {
                    tracing::warn!(%source, %workspace, %error, "a workspace was not branched");
                    (
                        tilo_session_core::desktop::i18n::t_args(
                            "session-branch-failed",
                            &[("branch", name.into())],
                        ),
                        error,
                    )
                }
            };
            self.telling().show(
                &Notice::new(String::new(), 1, title, body).about(NotificationIconKind::Workspace),
            );
        }
    }

    /// Put the branch `workspace` , called `name` , of `source` on the shell.
    fn land_branch(&self, source: WorkspaceId, workspace: WorkspaceId, name: &str) {
        let mut tree: Layout = self.handle.read(Clone::clone);
        let files = tilo_paths::workspace_root().or_else(|| {
            let mount = std::path::PathBuf::from(tilo_paths::WORKSPACE_MOUNT);
            mount.is_dir().then_some(mount)
        });
        let made = tree
            .workspaces
            .iter()
            .find(|held| held.id() == source)
            .map(|held| {
                shell_of(held, workspace, name, files.as_deref(), &mut || {
                    self.handle.mint_id()
                })
            });
        if let Some(mut branch) = made
            && !tree.workspaces.iter().any(|held| held.id() == workspace)
        {
            branch.policy.set_owner(self.person.clone());
            let first = branch.strips.first().map(|strip| strip.id);
            tree.workspaces.push(branch);
            if let Some(first) = first {
                tree.active_strip.insert(workspace, first);
            }
            // The person goes to the branch they asked for, unless they went elsewhere meanwhile.
            if tree.active == source {
                tree.active = workspace;
            }
            self.handle.replace_tree(tree.clone());
            #[cfg(target_os = "linux")]
            self.restore
                .borrow_mut()
                .add(&tree, workspace, &self.desktops.window_items());
        }
        tracing::info!(%source, %workspace, "a workspace was branched");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The branch's session has the source's strips and a window for each of this device's, made
    /// again by their apps; another person's window stays theirs.
    #[test]
    fn the_branchs_shell_has_the_sources_strips_and_this_devices_windows() {
        let from = WorkspaceId::parse("7a6b5c4d-0000-4000-8000-000000000001").unwrap();
        let to = WorkspaceId::parse("7a6b5c4d-0000-4000-8000-000000000002").unwrap();
        let mut source = Workspace::new(from, "Research");
        let tab = |id, tile| Tab {
            id,
            title: format!("window {id}"),
            tile,
            app_title: None,
            view: String::new(),
            pinned: false,
        };
        let mut strip = Strip::new(StripId(2), "Reading");
        strip.window_groups.push(WindowGroup {
            id: 3,
            tabs: vec![
                tab(
                    4,
                    TileContent::Foreign {
                        app_id: "so.tilo.notes".into(),
                        surface_id: 9,
                    },
                ),
                tab(
                    5,
                    TileContent::HostedWindow {
                        app_id: "org.gnome.TextEditor".into(),
                        label: "theirs".into(),
                        host: "ab".repeat(32),
                        host_label: "Laptop".into(),
                    },
                ),
            ],
            active_tab: 4,
            width: tilo_desktop_model::WindowWidth::Wide,
            maximized: true,
            focus_history: Vec::new(),
        });
        source.strips.push(strip);
        source.strips.push(Strip::new(StripId(6), "Writing"));
        let mut next = 100;
        let branch = shell_of(&source, to, "Research branch", None, &mut || {
            next += 1;
            next
        });
        assert_eq!(branch.id(), to);
        assert_eq!(branch.name, "Research branch");
        let names: Vec<&str> = branch
            .strips
            .iter()
            .map(|strip| strip.name.as_str())
            .collect();
        assert_eq!(names, ["Reading", "Writing"]);
        let tabs: Vec<&Tab> = branch.tabs().collect();
        assert_eq!(
            tabs.len(),
            1,
            "a window that runs on another device stays there"
        );
        assert!(matches!(
            &tabs[0].tile,
            TileContent::LocalApp { app_id, .. } if app_id == "so.tilo.notes"
        ));
        assert!(tabs[0].id > 100, "ids of its own");
        assert!(branch.strips[0].id.0 > 100);
    }

    /// An agent branches the workspace it works in, by any name the session knows it by, and no
    /// other: what it would copy of another is not its to read.
    #[test]
    fn an_agent_branches_its_own_workspace_and_no_other() {
        let workspaces = [
            ["studio-id".to_owned(), "Studio".to_owned()],
            ["garden-id".to_owned(), "Garden".to_owned()],
        ];
        let branch = |source: Option<&str>| tilo_actions::remote::InvokeRequest {
            argument: source.map_or(tilo_actions::remote::InvokeArgument::None, |source| {
                tilo_actions::remote::InvokeArgument::Json(serde_json::Map::from_iter([(
                    "source".to_owned(),
                    serde_json::Value::from(source),
                )]))
            }),
            ..tilo_actions::remote::InvokeRequest::plain(
                tilo_actions::remote::RemoteActionTarget {
                    provider: tilo_actions::remote::session_actions::PROVIDER.to_owned(),
                    address: tilo_actions::ActionAddress::new(
                        tilo_session_core::desktop::verbs::CHROME_RESOURCE,
                        tilo_session_core::desktop::host::contract::BRANCH_WORKSPACE,
                    ),
                },
                serde_json::Map::new(),
            )
        };
        for own in ["Studio", "studio", "studio-id"] {
            assert!(
                !branches_elsewhere(&branch(Some(own)), &workspaces, "studio-id"),
                "its own workspace, as {own}"
            );
        }
        for other in ["Garden", "garden-id", "nowhere"] {
            assert!(
                branches_elsewhere(&branch(Some(other)), &workspaces, "studio-id"),
                "{other} is not its own"
            );
        }
        assert!(
            branches_elsewhere(&branch(None), &workspaces, "studio-id"),
            "a branch that names nothing names nothing of its own"
        );
        let mut other_verb = branch(Some("Garden"));
        other_verb.target.address.action = "new-workspace".to_owned();
        assert!(!branches_elsewhere(&other_verb, &workspaces, "studio-id"));
    }
}
