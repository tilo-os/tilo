//! Letting a program running in an app control the desktop beyond that
//! app's workspace — `tilo do` typed in a terminal, say.
//!
//! In its own workspace such a program needs no one's leave
//! (`compositor::workspace_door`). The first time it reaches further, the
//! person is asked, once, in trusted chrome: "Let this terminal control your
//! desktop?" — for **this tab** until it closes, **always** for that app in
//! that workspace, or not at all. A tab grant belongs to the process tree
//! of the tab (its shell, for a Terminal tab) and is held here, in memory:
//! it ends when the tab does, or when the person stops it. "Always" is the
//! workspace's own capability grant ([`Capability::DesktopControl`]), which
//! the workspace's Permissions and the device's Privacy & Security list and
//! revoke like any other.
//!
//! Identity, security, updates and rollback, deleting data — anything whose
//! contract marks it risky or asks for confirmation — is confirmed every
//! time, whatever was allowed before.

use crate::shell::i18n::t;
use crate::shell::i18n::t_args;
use crate::shell::state::{Capability, GRANT_ALWAYS, WorkspaceTree};
use ui::widget::PromptChoice;

/// One process, by pid and start time, so a recycled pid is a different
/// process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Holder {
    pub pid: u32,
    pub start: u64,
}

impl Holder {
    /// Whether the process still runs.
    pub fn is_live(self) -> bool {
        crate::shell::host_runtime::proc_stat(self.pid)
            .is_some_and(|(_, start)| start == self.start)
    }
}

/// The app id whose programs are "this terminal" to the person.
const TERMINAL: &str = "so.tilo.terminal";

/// What the person is asked: may `package`, in the workspace of `room`, do
/// `action` beyond that workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopControlAsk {
    pub package: String,
    /// The app as the person knows it.
    pub app_label: String,
    pub room: String,
    pub action: String,
    /// Confirmed every time: offers only "Allow" and "Don't allow", and
    /// grants nothing beyond this one action.
    pub sensitive: bool,
    /// The tab a "this tab" answer lasts for.
    pub holder: Holder,
    /// The desktop's tabs it was asked from: where a "this tab" grant shows.
    pub tabs: Vec<u64>,
}

impl DesktopControlAsk {
    /// What the prompt says is asked.
    #[must_use]
    pub fn label(&self) -> String {
        let terminal = self.package == TERMINAL;
        let key = match (self.sensitive, terminal) {
            (false, true) => "desktop-control-ask-terminal",
            (false, false) => "desktop-control-ask",
            (true, true) => "desktop-control-confirm-terminal",
            (true, false) => "desktop-control-confirm",
        };
        t_args(
            key,
            &[
                ("app", self.app_label.clone().into()),
                ("action", self.action.clone().into()),
            ],
        )
    }

    /// The answers offered, in order, as the person reads them.
    #[must_use]
    pub fn choices(&self) -> Vec<(PromptChoice, String)> {
        if self.sensitive {
            vec![
                (PromptChoice::Allow, t("desktop-control-allow")),
                (PromptChoice::Deny, t("desktop-control-deny")),
            ]
        } else {
            vec![
                (PromptChoice::Session, t("desktop-control-this-tab")),
                (PromptChoice::Always, t("desktop-control-always")),
                (PromptChoice::Deny, t("desktop-control-deny")),
            ]
        }
    }
}

/// A "this tab" grant: `package` in the workspace of `room` may control the
/// desktop for as long as `holder` runs. It shows on `tabs`, the desktop's
/// tabs it was asked from, and nowhere else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabGrant {
    pub holder: Holder,
    pub package: String,
    pub room: String,
    pub tabs: Vec<u64>,
    /// What the person reads the tab as (its title when it asked).
    pub label: String,
}

/// The live "this tab" grants of this session.
#[derive(Debug, Default)]
pub struct DesktopControlGrants {
    tabs: Vec<TabGrant>,
    /// Moves whenever a grant is added or ends, so what shows the grants
    /// is read again only then.
    generation: u64,
}

impl DesktopControlGrants {
    /// Allow `grant` until its tab closes.
    pub fn allow_tab(&mut self, grant: TabGrant) {
        if !self.tabs.contains(&grant) {
            self.tabs.push(grant);
            self.generation = self.generation.wrapping_add(1);
        }
    }

    #[must_use]
    pub fn tab_allowed(&self, holder: Holder) -> bool {
        self.tabs
            .iter()
            .any(|grant| grant.holder == holder && holder.is_live())
    }

    /// Stop the tab grant of `holder`. Whether there was one.
    pub fn revoke_tab(&mut self, holder: Holder) -> bool {
        self.retain(|grant| grant.holder != holder)
    }

    /// Stop the tab grants that show on the desktop's tab `tab`. Whether
    /// there was one.
    pub fn revoke_on_tab(&mut self, tab: u64) -> bool {
        self.retain(|grant| !grant.tabs.contains(&tab))
    }

    /// Drop the grants of tabs that have closed. Whether any did.
    pub fn forget_closed(&mut self) -> bool {
        self.retain(|grant| grant.holder.is_live())
    }

    fn retain(&mut self, keep: impl FnMut(&TabGrant) -> bool) -> bool {
        let before = self.tabs.len();
        self.tabs.retain(keep);
        let changed = self.tabs.len() != before;
        if changed {
            self.generation = self.generation.wrapping_add(1);
        }
        changed
    }

    /// Changes whenever a grant is added or ends.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The live tab grants.
    pub fn tabs(&self) -> impl Iterator<Item = &TabGrant> {
        self.tabs.iter().filter(|grant| grant.holder.is_live())
    }
}

/// What the person reads the first of `tabs` as: its title.
#[must_use]
pub fn tab_label(tree: &WorkspaceTree, tabs: &[u64]) -> Option<String> {
    let first = *tabs.first()?;
    tree.desktop
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .find(|tab| tab.id == first)
        .map(|tab| tab.title.clone())
        .filter(|title| !title.is_empty())
}

/// Whether `package` holds the "always" grant in the workspace of `room`.
#[must_use]
pub fn always_allowed(tree: &WorkspaceTree, room: &str, package: &str) -> bool {
    let now = tilo_clock::unix_now();
    tree.desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room().to_string() == room)
        .is_some_and(|workspace| {
            workspace
                .app_capabilities_at(package, now)
                .into_iter()
                .any(|cap| {
                    matches!(
                        Capability::from_capability(cap),
                        Some(Capability::DesktopControl { .. })
                    )
                })
        })
}

/// Record the "always" grant for `package` in the workspace of `room`, as
/// the workspace's capability (mirrored to peers and disk). Whether it was
/// recorded.
pub fn allow_always(tree: &mut WorkspaceTree, room: &str, package: &str) -> bool {
    let Some(workspace) = tree
        .desktop
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.room().to_string() == room)
    else {
        return false;
    };
    let id = workspace.id;
    let ids = workspace.grant_app(
        package,
        &Capability::DesktopControl {
            scope: GRANT_ALWAYS,
        },
    );
    tree.doc_write_policy(
        id,
        &crate::shell::state::PolicyEdit {
            sandbox_granted: ids,
            ..crate::shell::state::PolicyEdit::default()
        },
    );
    tree.queue_save();
    true
}

/// Take back the "always" grant of `package` in the workspace of `room`.
/// Whether there was one.
pub fn revoke_always(tree: &mut WorkspaceTree, room: &str, package: &str) -> bool {
    let Some(workspace) = tree
        .desktop
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.room().to_string() == room)
    else {
        return false;
    };
    let id = workspace.id;
    let want = collab_types::caps::Subject::app(package);
    let removed = workspace.revoke_sandbox_matching(|cap| {
        cap.subject == want
            && matches!(
                Capability::from_capability(cap),
                Some(Capability::DesktopControl { .. })
            )
    });
    if removed.is_empty() {
        return false;
    }
    tree.doc_write_policy(
        id,
        &crate::shell::state::PolicyEdit {
            sandbox_revoked: removed,
            ..crate::shell::state::PolicyEdit::default()
        },
    );
    tree.queue_save();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> Holder {
        let pid = std::process::id();
        let (_, start) = crate::shell::host_runtime::proc_stat(pid).expect("own stat");
        Holder { pid, start }
    }

    #[test]
    fn a_tab_grant_ends_when_its_tab_closes() {
        let mut grants = DesktopControlGrants::default();
        let mut shell = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("a tab's shell");
        let pid = shell.id();
        let (_, start) = crate::shell::host_runtime::proc_stat(pid).expect("its stat");
        let tab = Holder { pid, start };
        grants.allow_tab(TabGrant {
            holder: tab,
            package: TERMINAL.into(),
            room: "home".into(),
            tabs: vec![7],
            label: "claude".into(),
        });
        assert!(grants.tab_allowed(tab));
        assert!(!grants.tab_allowed(me()), "another tab holds nothing");
        // This test's own child: the tab closes.
        let _ = shell.kill();
        let _ = shell.wait();
        assert!(!grants.tab_allowed(tab), "the grant ended with the tab");
        assert!(grants.forget_closed());
        assert_eq!(grants.tabs().count(), 0);
    }

    #[test]
    fn a_tab_grant_is_revoked_at_once() {
        let mut grants = DesktopControlGrants::default();
        let tab = me();
        grants.allow_tab(TabGrant {
            holder: tab,
            package: TERMINAL.into(),
            room: "home".into(),
            tabs: vec![7],
            label: "claude".into(),
        });
        assert!(grants.tab_allowed(tab));
        assert!(grants.revoke_tab(tab));
        assert!(!grants.tab_allowed(tab));
        grants.allow_tab(TabGrant {
            holder: tab,
            package: TERMINAL.into(),
            room: "home".into(),
            tabs: vec![7],
            label: "claude".into(),
        });
        assert!(!grants.revoke_on_tab(8), "another tab shows no grant");
        assert!(grants.revoke_on_tab(7));
        assert!(!grants.tab_allowed(tab));
    }

    #[test]
    fn always_is_the_workspaces_own_grant_and_is_revoked_like_one() {
        let mut tree = WorkspaceTree::bootstrap_default();
        let room = tree
            .desktop
            .active_workspace()
            .expect("a workspace")
            .room()
            .to_string();
        assert!(!always_allowed(&tree, &room, TERMINAL));
        assert!(allow_always(&mut tree, &room, TERMINAL));
        assert!(always_allowed(&tree, &room, TERMINAL));
        assert!(
            !always_allowed(&tree, &room, "org.example.other"),
            "for that app alone"
        );
        let listed = tree
            .desktop
            .active_workspace()
            .expect("a workspace")
            .app_grants_snapshot_at(tilo_clock::unix_now());
        assert!(
            listed
                .get(TERMINAL)
                .is_some_and(|caps| caps.contains(&Capability::DesktopControl {
                    scope: GRANT_ALWAYS
                })),
            "the workspace's Permissions list it: {listed:?}"
        );
        assert!(revoke_always(&mut tree, &room, TERMINAL));
        assert!(!always_allowed(&tree, &room, TERMINAL));
    }

    #[test]
    fn the_person_is_asked_in_plain_words_with_three_answers_or_two() {
        let ask = DesktopControlAsk {
            package: TERMINAL.into(),
            app_label: "Terminal".into(),
            room: "home".into(),
            action: "Show overview".into(),
            sensitive: false,
            holder: me(),
            tabs: vec![7],
        };
        assert!(ask.label().contains("Show overview"), "{}", ask.label());
        let offered: Vec<_> = ask
            .choices()
            .into_iter()
            .map(|(choice, _)| choice)
            .collect();
        assert_eq!(
            offered,
            [
                PromptChoice::Session,
                PromptChoice::Always,
                PromptChoice::Deny
            ]
        );
        let confirm = DesktopControlAsk {
            sensitive: true,
            ..ask
        };
        let offered: Vec<_> = confirm
            .choices()
            .into_iter()
            .map(|(choice, _)| choice)
            .collect();
        assert_eq!(offered, [PromptChoice::Allow, PromptChoice::Deny]);
    }
}
