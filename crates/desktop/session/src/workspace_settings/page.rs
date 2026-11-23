//! The Agents tab of the workspace settings on a page: the agents of Chat's record, made, bounded
//! and retired with the record's own actions as on a computer, read in the records the page's
//! windows keep. The page draws the sheet itself; this builds the tab and answers its asks.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_desktop::people::PeopleNames;
use tilo_desktop_model::UserId;
use tilo_desktop_surface::DesktopHandle;
use tilo_sheets::workspace_settings as sheet;

use super::Directory;
use super::agents::{Agents, Answered, Host};

/// The workspace's agents as a page shows them.
pub struct PageAgents {
    agents: Agents,
    handle: DesktopHandle,
    person: UserId,
    /// The agent whose review the tab shows, if any.
    review: RefCell<Option<String>>,
}

impl PageAgents {
    /// The agents of the workspace the shell `handle` shows, for `person`, read in the records
    /// `host` opens; `wake` asks for the page's next pass when another replica's change to them
    /// lands.
    #[must_use]
    pub fn new(
        handle: DesktopHandle,
        person: UserId,
        host: Rc<dyn Host>,
        wake: tilo_service::Wake,
    ) -> Self {
        Self {
            agents: Agents::new(host, wake),
            handle,
            person,
            review: RefCell::default(),
        }
    }

    /// The Agents tab as the workspace in front stands, on the review the person opened.
    #[must_use]
    pub fn pane(&self) -> sheet::Agents {
        let review = self.review.borrow().clone();
        self.handle.read(|tree| {
            let names = PeopleNames::of(tree, &self.person, None);
            self.agents.pane(
                tree,
                &self.person,
                (&names, &Directory::default()),
                review.as_deref(),
            )
        })
    }

    /// Carry out `effect` when it is the Agents tab's, saying over the sheet `state` why the
    /// workspace refused it. Whether it was the tab's.
    pub fn ask(
        &self,
        state: &sheet::WorkspaceSettingsState,
        effect: &sheet::WorkspaceSettingsEffect,
    ) -> bool {
        use sheet::WorkspaceSettingsEffect as Ask;
        match effect {
            Ask::ReviewAgent { agent } => *self.review.borrow_mut() = Some(agent.clone()),
            Ask::EndReview => *self.review.borrow_mut() = None,
            asked => {
                let workspace = self.handle.read(|tree| tree.active_workspace().cloned());
                match self.agents.answer(workspace.as_ref(), &self.person, asked) {
                    Some(Answered::Show(review)) => *self.review.borrow_mut() = review,
                    Some(Answered::Done) => {}
                    Some(Answered::Refused(reason)) => {
                        state.refuse(crate::workspace_people::refusal(&reason));
                    }
                    None => return false,
                }
            }
        }
        true
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a failed step in a test is the assertion"
    )]

    use std::rc::Rc;

    use tilo_desktop_model::{Layout, UserId, Workspace, WorkspaceId};
    use tilo_desktop_surface::{DesktopSurface, TileSource};
    use tilo_sheets::workspace_settings::{WorkspaceSettingsEffect as Ask, WorkspaceSettingsState};
    use ui::permissions::{LivePermissions, MemberId};
    use ui::resource::ResourceOwners;
    use ui::resource::workspace::WorkspaceResources;
    use ui::widget::{TileContent, TileHost};

    use super::PageAgents;

    struct Blank;

    impl TileSource for Blank {
        fn tile(&mut self, _: &Workspace, tab: &tilo_desktop_model::Tab) -> TileHost {
            TileHost::new(TileContent::Notice {
                title: tab.title.clone(),
                detail: String::new(),
            })
        }
    }

    /// A page's records of its workspace, as its windows keep them.
    struct Page(WorkspaceResources);

    impl super::Host for Page {
        fn open(&self, _: &Workspace, _: &UserId) -> (WorkspaceResources, Option<LivePermissions>) {
            (self.0.clone(), None)
        }

        fn manages(&self, _: &Workspace, _: &UserId) -> bool {
            true
        }

        /// The page's workspace service, standing in: the record's own action, applied to the
        /// records the page keeps.
        fn change(
            &self,
            _: &Workspace,
            change: super::super::AgentsChange,
        ) -> tilo_actions::remote::Pending<Result<String, String>> {
            use tilo_app::framework::WorkspaceResource;
            use tilo_app::tilo_workspace_agent::{OWNER, WorkspaceAgents, workspace_agents};
            let record = WorkspaceResource::<WorkspaceAgents>::open(
                &self.0,
                OWNER,
                ("tilo-session", "Workspace settings"),
                None,
                None,
            )
            .expect("the page's agents record opens");
            let (invoke, id) = match change {
                super::super::AgentsChange::Create(mut create) => {
                    let id = create
                        .id
                        .get_or_insert_with(|| record.get().free_id(create.name.as_str()))
                        .clone();
                    (WorkspaceAgents::CREATE_AGENT.with(create), id)
                }
                super::super::AgentsChange::Update(update) => (update, String::new()),
                super::super::AgentsChange::Retire(agent) => (
                    WorkspaceAgents::RETIRE_AGENT.with(workspace_agents::RetireAgent {
                        agent: tilo_resource::Id::from_raw(agent),
                    }),
                    String::new(),
                ),
            };
            let (done, answer) = tilo_actions::remote::pending();
            done.complete(
                record
                    .invoke(&invoke)
                    .map(|()| id)
                    .map_err(|error| error.to_string()),
            );
            answer
        }

        fn took(&self, _: &Workspace) {}
    }

    /// The names the tab lists.
    fn listed(agents: &PageAgents) -> Vec<String> {
        agents
            .pane()
            .agents
            .iter()
            .map(|agent| agent.name.clone())
            .collect()
    }

    #[test]
    fn a_page_makes_bounds_and_retires_the_workspaces_agents() {
        let kept = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(WorkspaceId::new(), "Studio");
        let records = WorkspaceResources::in_workspace(
            ResourceOwners::new(kept.path().to_owned()),
            workspace.id().to_string(),
            MemberId::person("ada".to_owned()),
        );
        let (_shell, handle) = DesktopSurface::new(
            Layout {
                active: workspace.id(),
                workspaces: vec![workspace],
                ..Layout::default()
            },
            7,
            Box::new(Blank),
        );
        let agents = PageAgents::new(
            handle,
            "ada".to_owned(),
            Rc::new(Page(records)),
            std::sync::Arc::new(|| {}),
        );
        let state = WorkspaceSettingsState::default();
        let pane = agents.pane();
        assert!(pane.manage);
        assert!(listed(&agents).is_empty());

        assert!(agents.ask(
            &state,
            &Ask::AddAgent {
                name: "Scout".to_owned(),
                model: String::new(),
            }
        ));
        assert_eq!(listed(&agents), ["Scout"]);
        let key = agents.pane().agents[0].key.clone();

        agents.ask(&state, &Ask::ReviewAgent { agent: key.clone() });
        agents.ask(
            &state,
            &Ask::SetAgentSpendLimit {
                agent: key.clone(),
                dollars: "5".to_owned(),
            },
        );
        let review = agents.pane().review.expect("the agent's review");
        let limit = review.limit.expect("its daily limit");
        assert!(limit.summary.contains('5'), "{}", limit.summary);
        assert!(limit.can_choose, "its person sets its limit");
        assert!(review.editable, "a page bounds the agent in its list");
        assert!(review.can_retire);
        agents.ask(
            &state,
            &Ask::SetAgentAnswer {
                agent: key.clone(),
                scope: "app:so.tilo.notes".to_owned(),
                kind: "change".to_owned(),
                answer: "deny".to_owned(),
            },
        );
        let notes = agents
            .pane()
            .review
            .expect("the agent's review")
            .apps
            .into_iter()
            .find(|app| app.scope == "app:so.tilo.notes")
            .expect("the review lists the app its bound names");
        assert!(notes.customised, "the bound sets what it may do in Notes");
        assert_eq!(
            notes
                .kinds
                .iter()
                .find(|line| line.kind == "change")
                .map(|line| line.answer),
            Some(2),
            "it may change nothing there"
        );

        agents.ask(
            &state,
            &Ask::SetAgentSpendLimit {
                agent: key.clone(),
                dollars: "lots".to_owned(),
            },
        );
        assert!(
            state.notice().is_some(),
            "a limit that is no amount is refused"
        );

        agents.ask(&state, &Ask::RemoveAgent { agent: key });
        assert!(
            listed(&agents).is_empty(),
            "a retired agent leaves the roster"
        );
        assert!(agents.pane().review.is_none());
        assert!(
            !agents.ask(&state, &Ask::Invite),
            "the tab answers only its own asks"
        );
    }
}
