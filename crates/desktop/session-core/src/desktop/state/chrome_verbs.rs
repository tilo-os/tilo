//! The chrome's published verbs that change the desktop: the session carries them out on the
//! desktop it owns, whichever shell draws it. What is left to a shell is only what it draws itself
//! (the overview, the About sheet, the bar's menus and panels), which [`Carried::Shell`] hands back.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_desktop_model::commands::DesktopCommand;
use tilo_desktop_model::{Layout, Strip, StripId, Workspace, WorkspaceId};
use ui::shortcut_table::{FixedAction, ShortcutAction};

use super::DesktopHandle;
use super::shortcuts::Performed;
use crate::desktop::host::contract::{DesktopAsk, JOIN_WORKSPACE, WORKSPACE_SETTINGS};
use crate::desktop::i18n::{t, t_args};
use crate::desktop::navigation::ChromeEffect;
use crate::desktop::{names, workspace_names};

/// What became of a chrome verb the desktop was asked to carry out.
#[derive(Clone, Debug, PartialEq)]
pub enum Carried {
    /// The desktop carried it out, or said why it could not.
    Done,
    /// Carried out, answering this (a branched workspace's id).
    Answer(serde_json::Value),
    /// A shortcut, of which this is what is left to the shell.
    Shortcut(Performed),
    /// A surface the shell draws itself: the shell carries it out.
    Shell,
}

impl DesktopHandle {
    /// Leave what the desktop asks of its host in `asks`, which the host reads.
    pub fn set_asks(&self, asks: Rc<RefCell<Vec<DesktopAsk>>>) {
        self.shared.borrow_mut().asks = Some(asks);
    }

    /// Ask the host for `asked`.
    pub fn ask(&self, asked: DesktopAsk) {
        let asks = self.shared.borrow().asks.clone();
        if let Some(asks) = asks {
            asks.borrow_mut().push(asked);
        } else {
            tracing::warn!(?asked, "a desktop with no host to ask");
        }
        self.wake();
    }

    /// Carry out the chrome's verb `verb`.
    ///
    /// # Errors
    /// Why it could not, which the action that asked answers as a refusal.
    pub fn perform_verb(&self, verb: &ChromeEffect) -> Result<Carried, String> {
        if let Some(carried) = self.perform_workspace_verb(verb) {
            return Ok(carried);
        }
        let carried = match verb {
            ChromeEffect::OpenApp { .. }
            | ChromeEffect::ShowApp { .. }
            | ChromeEffect::MoveTab { .. }
            | ChromeEffect::CloseWindow { .. }
            | ChromeEffect::SplitRight { .. }
            | ChromeEffect::NewGroup { .. }
            | ChromeEffect::SetWidth { .. }
            | ChromeEffect::Equalize { .. }
            | ChromeEffect::Maximize { .. }
            | ChromeEffect::Fullscreen { .. }
            | ChromeEffect::Float { .. }
            | ChromeEffect::Rename { .. }
            | ChromeEffect::ReopenClosed { .. }
            | ChromeEffect::ShareWindow { .. }
            | ChromeEffect::StopDesktopControl { .. }
            | ChromeEffect::SwitchWorkspace {
                workspace: Some(_), ..
            }
            | ChromeEffect::ShowStrip { .. }
            | ChromeEffect::Pan { .. }
            | ChromeEffect::Scroll { .. }
            | ChromeEffect::History { .. }
            | ChromeEffect::Activity { .. }
            | ChromeEffect::CloseActivity { .. }
            | ChromeEffect::Notifications { .. } => {
                let done = self.dispatch(verb.clone());
                if let Err(refusal) = &done {
                    self.acknowledge(refusal.clone());
                }
                done.map(|()| Carried::Done)?
            }
            ChromeEffect::SwitchWorkspace {
                delta: Some(delta), ..
            } => {
                self.step_workspace(i32::try_from(*delta).unwrap_or_default());
                Carried::Done
            }
            ChromeEffect::StepStrip { steps } => {
                self.step_strip(isize::try_from(*steps).unwrap_or_default());
                Carried::Done
            }
            // A strip made here keeps no name until the person gives it one: the shell names it by
            // its place, in the language of whoever reads it.
            ChromeEffect::NewStrip { name } => {
                if self.add_strip(name.clone().unwrap_or_default()).is_none() {
                    return Err(t("strip-switcher-refused"));
                }
                Carried::Done
            }
            ChromeEffect::MoveToNewStrip { tab } => {
                let tab: u64 = tab
                    .trim()
                    .parse()
                    .map_err(|_| format!("{tab} is no window"))?;
                self.move_to_new_strip(tab);
                Carried::Done
            }
            ChromeEffect::DeleteStrip { strip } => {
                let strip: u64 = strip
                    .trim()
                    .parse()
                    .map_err(|_| format!("{strip} is no strip"))?;
                self.delete_strip(StripId(strip))?;
                Carried::Done
            }
            ChromeEffect::NewWorkspace {
                workspace,
                name,
                background,
            } => {
                self.new_workspace(
                    workspace.as_deref(),
                    name.as_deref(),
                    background.unwrap_or(false),
                );
                Carried::Done
            }
            ChromeEffect::BranchWorkspace { source, name } => {
                let branch = self.branch_workspace(source, name.as_deref())?;
                Carried::Answer(serde_json::Value::String(branch.to_string()))
            }
            ChromeEffect::Shortcut { action } => {
                let Ok(action) = serde_json::from_value::<ShortcutAction>(
                    serde_json::Value::String(action.clone()),
                ) else {
                    return Err(format!("no shortcut is called {action}"));
                };
                let performed = self.perform_shortcut(action);
                // Not the desktop's: the host carries it out, once.
                if performed == Performed::Session {
                    self.ask(DesktopAsk::Shortcut(action));
                }
                Carried::Shortcut(performed)
            }
            ChromeEffect::FixedShortcut { action } => {
                let Some(action) = FixedAction::from_id(action) else {
                    return Err(format!("no fixed shortcut is called {action}"));
                };
                let performed = self.perform_fixed_shortcut(action);
                if performed == Performed::Session {
                    self.ask(DesktopAsk::FixedShortcut(action));
                }
                Carried::Shortcut(performed)
            }
            ChromeEffect::RevealTopBar
            | ChromeEffect::Overview { .. }
            | ChromeEffect::About { .. }
            | ChromeEffect::Recents { .. }
            | ChromeEffect::QuickSettings
            | ChromeEffect::WorkspaceMenu
            | ChromeEffect::ShowPerson { .. }
            | ChromeEffect::TogglePeople
            | ChromeEffect::ShowStrips
            | ChromeEffect::SwitchWorkspace { .. } => Carried::Shell,
            // Carried out by the workspace verbs above.
            ChromeEffect::MoveStrip { .. }
            | ChromeEffect::JoinWorkspace
            | ChromeEffect::OpenSettings { .. }
            | ChromeEffect::LeaveWorkspace { .. }
            | ChromeEffect::DeleteWorkspace { .. }
            | ChromeEffect::WorkspaceSettings => Carried::Done,
        };
        Ok(carried)
    }

    /// What the desktop does to its workspaces and strips by address: `None` for a verb of another
    /// kind. A refusal is said at once, as the person's own gesture would be.
    fn perform_workspace_verb(&self, verb: &ChromeEffect) -> Option<Carried> {
        let report = |done: Result<(), String>| {
            if let Err(refusal) = done {
                self.acknowledge(refusal);
            }
            Some(Carried::Done)
        };
        match verb {
            ChromeEffect::Rename {
                strip: Some(strip),
                name,
                ..
            } => report(self.strip_named(strip).and_then(|strip| {
                self.edit_workspace(&DesktopCommand::RenameStrip {
                    strip: strip.0,
                    name: name.clone(),
                })
                .map(drop)
            })),
            ChromeEffect::Rename {
                workspace: Some(workspace),
                name,
                ..
            } => report(self.workspace_named(workspace).and_then(|workspace| {
                let active = self.read(|tree| tree.active);
                (active == workspace)
                    .then_some(())
                    .ok_or_else(|| "Workspace is no longer active".to_owned())?;
                self.edit_workspace(&DesktopCommand::RenameWorkspace { name: name.clone() })
                    .map(drop)
            })),
            ChromeEffect::MoveStrip { strip, before } => {
                report(self.strip_named(strip).and_then(|strip| {
                    let before = before
                        .as_deref()
                        .map(|before| self.strip_named(before))
                        .transpose()?;
                    self.edit_workspace(&DesktopCommand::MoveStrip {
                        strip: strip.0,
                        before: before.map(|before| before.0),
                    })
                    .map(drop)
                }))
            }
            ChromeEffect::JoinWorkspace => {
                self.ask(DesktopAsk::HostAction(JOIN_WORKSPACE.to_owned()));
                Some(Carried::Done)
            }
            ChromeEffect::OpenSettings { link } => {
                self.ask(DesktopAsk::OpenSettings(link.clone()));
                Some(Carried::Done)
            }
            ChromeEffect::LeaveWorkspace { workspace } => report(
                workspace
                    .as_deref()
                    .map_or(Ok(None), |id| self.workspace_named(id).map(Some))
                    .and_then(|id| self.leave_workspace(id)),
            ),
            ChromeEffect::DeleteWorkspace {
                workspace,
                confirmed,
            } => report(
                workspace
                    .as_deref()
                    .map_or(Ok(None), |id| self.workspace_named(id).map(Some))
                    .and_then(|id| self.delete_workspace(id, *confirmed)),
            ),
            ChromeEffect::WorkspaceSettings => {
                self.ask(DesktopAsk::HostAction(WORKSPACE_SETTINGS.to_owned()));
                Some(Carried::Done)
            }
            _ => None,
        }
    }

    /// Run a workspace or strip `command` on the active workspace as the model's command, once the
    /// rules allow it; a refusal is said at once. The strip it made, if any.
    pub fn ruled_command(&self, command: &DesktopCommand) -> Option<StripId> {
        let rules = self.shared.borrow().rules.clone();
        let applied = rules
            .as_ref()
            .map_or(Ok(()), |rules| rules.may(command))
            .and_then(|()| self.edit_workspace(command));
        match applied {
            Ok(made) => made,
            Err(refusal) => {
                self.acknowledge(refusal);
                None
            }
        }
    }

    /// Apply a strip or workspace edit through the desktop's one command path; the strip it made,
    /// if any.
    ///
    /// # Errors
    /// Why the model refused it.
    pub fn edit_workspace(&self, command: &DesktopCommand) -> Result<Option<StripId>, String> {
        let made = self.apply_command(command)?;
        if let DesktopCommand::RenameWorkspace { name } = command {
            // The workspace's name is its governance's, which every device and list of it reads:
            // the host writes it there, and it is projected back onto this shell.
            let workspace = self.read(|tree| tree.active_workspace().map(Workspace::id));
            if let Some(workspace) = workspace {
                self.ask(DesktopAsk::RenamedWorkspace {
                    workspace,
                    name: name.trim().to_owned(),
                });
            }
        }
        Ok(made)
    }

    /// The workspace `key` names: its id, or its name.
    ///
    /// # Errors
    /// When no workspace here is called that, in words for the person.
    pub fn workspace_named(&self, key: &str) -> Result<WorkspaceId, String> {
        let key = key.trim();
        self.read(|tree| {
            tree.workspaces
                .iter()
                .find(|workspace| workspace.name.eq_ignore_ascii_case(key))
                .or_else(|| {
                    // The id it is shared under, the same on every device.
                    WorkspaceId::parse(key).and_then(|id| {
                        tree.workspaces
                            .iter()
                            .find(|workspace| workspace.id() == id)
                    })
                })
                .map(Workspace::id)
        })
        .ok_or_else(|| t_args("desktop-no-workspace", &[("name", key.into())]))
    }

    /// The strip `key` names in the workspace the person is in: its id, or the name a person
    /// reads, which is a strip nobody named by where it stands ("Strip 1").
    ///
    /// # Errors
    /// When no strip there is called that, in words for the person.
    pub fn strip_named(&self, key: &str) -> Result<StripId, String> {
        let key = key.trim();
        self.read(|tree| {
            let workspace = tree.active_workspace()?;
            workspace
                .strips
                .iter()
                .enumerate()
                .find(|(at, strip)| names::strip_named(&strip.name, *at).eq_ignore_ascii_case(key))
                .map(|(_, strip)| strip)
                .or_else(|| {
                    key.parse::<u64>()
                        .ok()
                        .and_then(|id| workspace.strips.iter().find(|strip| strip.id.0 == id))
                })
                .map(|strip| strip.id)
        })
        .ok_or_else(|| t_args("desktop-no-strip", &[("name", key.into())]))
    }

    /// Leave the workspace `id` (the one the person is in when absent): struck from this device's
    /// desktop, the person going to the next one when it was the one they were in.
    ///
    /// # Errors
    /// Refuses leaving the only workspace, or one this device does not hold.
    pub fn leave_workspace(&self, id: Option<WorkspaceId>) -> Result<(), String> {
        if self.read(|tree| tree.workspaces.len()) <= 1 {
            return Err(t("desktop-only-workspace"));
        }
        let left = self.take_workspace_off(id)?;
        self.ask(DesktopAsk::LeftWorkspace(left));
        Ok(())
    }

    /// Delete the workspace `id` (the one the person is in when absent).
    ///
    /// Deleting takes everything in it, so it is asked first: the person answers in the host's own
    /// confirm ([`DesktopAsk::DeleteWorkspace`]), and only an answer that says so deletes.
    pub fn delete_workspace(
        &self,
        id: Option<WorkspaceId>,
        confirmed: Option<bool>,
    ) -> Result<(), String> {
        let here = id.or_else(|| self.read(|tree| tree.active_workspace().map(Workspace::id)));
        let id = here.ok_or_else(|| t("desktop-no-workspace-to-delete"))?;
        let rules = self.shared.borrow().rules.clone();
        match rules {
            Some(rules) => rules.may_delete_workspace(id)?,
            None => return Err(t("desktop-cannot-delete-workspace")),
        }
        let name = self
            .read(|tree| {
                tree.workspaces
                    .iter()
                    .find(|workspace| workspace.id() == id)
                    .map(|workspace| names::workspace(&workspace.name))
            })
            .ok_or_else(|| t("desktop-workspace-gone"))?;
        if confirmed != Some(true) {
            // The person answers first: nothing of it goes until they do.
            self.ask(DesktopAsk::DeleteWorkspace {
                workspace: id,
                name,
            });
            return Ok(());
        }
        let name = self.take_workspace_off(Some(id))?;
        if self.read(|tree| tree.workspaces.is_empty()) {
            self.fresh_workspace();
        }
        // Whoever asked for it — the menu, the settings, an agent — the host hears of the deletion
        // the same way, from the desktop that made it.
        self.ask(DesktopAsk::DeletedWorkspace {
            workspace: id,
            name,
        });
        Ok(())
    }

    /// Take the workspace `id` (the one the person is in when absent) off this desktop, the person
    /// going to the next one along when it was the one they were in. Answers what it was called.
    fn take_workspace_off(&self, id: Option<WorkspaceId>) -> Result<String, String> {
        let mut tree = self.read(Clone::clone);
        let id = id.or_else(|| tree.active_workspace().map(Workspace::id));
        let id = id.ok_or_else(|| t("desktop-no-workspace-to-leave"))?;
        let left = tree
            .take_off(id)
            .ok_or_else(|| t("desktop-workspace-gone"))?;
        self.replace_tree(tree);
        Ok(left.name)
    }

    /// A new empty workspace after the others, with the strip its first window goes in: what stands
    /// on the desktop when the last one is deleted.
    fn fresh_workspace(&self) {
        let tree = self.read(Clone::clone);
        let workspace = Workspace::new(
            WorkspaceId::new(),
            workspace_names::made(None, &tree.workspaces),
        );
        self.open_workspace(tree, workspace, true, true);
    }

    /// Put `workspace` after the others and, when `go` , go there, with a strip of its own made
    /// here when `fresh_strip` — a workspace a site lets this computer into comes with the strips
    /// its site holds, and a strip made here would stand on this device only. A desktop that shows
    /// no workspace goes to the one it gets whatever `go` says: there is nowhere else to stay.
    fn open_workspace(
        &self,
        mut tree: Layout,
        mut workspace: Workspace,
        fresh_strip: bool,
        go: bool,
    ) {
        let id = workspace.id();
        let go = go || tree.active_workspace().is_none();
        if fresh_strip {
            let strip = Strip::new(StripId(self.mint_id()), "");
            let strip_id = strip.id;
            workspace.strips.push(strip);
            tree.active_strip.insert(id, strip_id);
        }
        tree.workspaces.push(workspace);
        if go {
            tree.active = id;
        }
        self.replace_tree(tree);
    }

    /// Make a workspace after the others and go there; with `join` , the workspace holding that id,
    /// joining it when this computer has none (a workspace that exists is a workspace that exists,
    /// so joining one is holding it, which needs no workspace of the person's own to make).
    /// In the `background` the workspace is put on the desktop and the person stays where they
    /// are: one their list brings from another of their devices arrives while they are somewhere
    /// else, and what they open next belongs to the workspace they are in.
    pub fn new_workspace(&self, join: Option<&str>, name: Option<&str>, background: bool) {
        let rules = self.shared.borrow().rules.clone();
        let mut tree = self.read(Clone::clone);
        let joining = join.is_some();
        let workspace = if let Some(id) = join {
            let Some(joined) = WorkspaceId::parse(id) else {
                self.acknowledge(t("desktop-joined-workspace-unknown"));
                return;
            };
            if let Some(held) = tree.workspaces.iter().find(|held| held.id() == joined) {
                // Already held, here or from another device: go there rather than make a second
                // workspace of one workspace. In the background the person stays where they are,
                // and the tree is replaced all the same: a replaced tree is one the session reads
                // and writes its workspaces' records for again, and this workspace has just taken
                // this device in.
                if !background {
                    tree.active = held.id();
                }
                self.replace_tree(tree);
                return;
            }
            // A workspace that exists comes on this desktop once this device holds it: put on bare,
            // it would be a second workspace under its id.
            if let Some(Err(refusal)) = rules.as_ref().map(|rules| rules.may_open_workspace(joined))
            {
                self.acknowledge(refusal);
                return;
            }
            Workspace::new(joined, workspace_names::made(name, &tree.workspaces))
        } else {
            if let Some(Err(refusal)) = rules.as_ref().map(|rules| rules.may_create_workspace()) {
                self.acknowledge(refusal);
                return;
            }
            Workspace::new(
                WorkspaceId::new(),
                workspace_names::made(name, &tree.workspaces),
            )
        };
        self.open_workspace(tree, workspace, !joining, !background);
    }

    /// Branch the workspace `source` names into a new one called `name` (the source's name with
    /// "branch" when none is given): the new workspace's id is made here, and the host makes the
    /// workspace with what the source holds and puts it on the desktop once it does
    /// ([`DesktopAsk::BranchWorkspace`]), so nothing opens in it before its records are there.
    /// Answers the new workspace's id.
    fn branch_workspace(&self, source: &str, name: Option<&str>) -> Result<WorkspaceId, String> {
        let source = self.workspace_named(source)?;
        let rules = self.shared.borrow().rules.clone();
        if let Some(rules) = rules {
            rules.may_branch_workspace(source)?;
        }
        let tree = self.read(Clone::clone);
        let held = tree
            .workspaces
            .iter()
            .find(|held| held.id() == source)
            .ok_or_else(|| t("desktop-workspace-gone"))?;
        let asked = name.map(str::trim).filter(|name| !name.is_empty());
        let name = asked.map_or_else(
            || {
                t_args(
                    "desktop-branch-workspace-name",
                    &[("name", held.name.clone().into())],
                )
            },
            str::to_owned,
        );
        let name = workspace_names::made(Some(&name), &tree.workspaces);
        let workspace = WorkspaceId::new();
        self.ask(DesktopAsk::BranchWorkspace {
            source,
            workspace,
            name,
        });
        Ok(workspace)
    }
}
