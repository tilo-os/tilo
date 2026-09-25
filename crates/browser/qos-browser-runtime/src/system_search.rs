//! Browser search identities derived only from the current admitted desktop.
use crate::{
    desktop,
    desktop_host::DesktopHandle,
    desktop_navigation::{Command, Route},
};
use std::collections::BTreeMap;
use ui::headless::{
    FrameHarness,
    system_search::{
        GlobalSearchApplication, GlobalSearchIntent, GlobalSearchSourceData, GlobalSearchTab,
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct AdmittedTab {
    route: Route,
    app: String,
    record: String,
    title: String,
    active_workspace: bool,
}

#[derive(Default)]
pub(crate) struct SearchState {
    pub data: GlobalSearchSourceData,
    generation: String,
    admitted: Vec<AdmittedTab>,
    targets: BTreeMap<String, Route>,
    installed_revision: String,
    serial: u64,
}
impl SearchState {
    pub fn refresh(
        &mut self,
        generation: &str,
        _heads: &[[u8; 32]],
        tree: &qos_desktop::WorkspaceTree,
        routes: &BTreeMap<u64, String>,
        active: Option<&Route>,
    ) {
        let mut admitted = Vec::new();
        for workspace in &tree.workspaces {
            for strip in &workspace.strips {
                for group in &strip.window_groups {
                    for tab in &group.tabs {
                        let Some(app) = qos_desktop::resolver::canonical_app_id(&tab.tile) else {
                            continue;
                        };
                        let Some((durable_app_id, definition)) =
                            desktop::installed_application(&app)
                        else {
                            continue;
                        };
                        let Ok(checkout) = desktop::checkout(definition, tab.id, routes) else {
                            continue;
                        };
                        admitted.push(AdmittedTab {
                            route: Route {
                                workspace: workspace.id.0.to_string(),
                                strip: strip.id.0.to_string(),
                                group: group.id.to_string(),
                                tab: tab.id.to_string(),
                            },
                            app: durable_app_id.into(),
                            record: checkout.id.as_str().into(),
                            title: tab.title.clone(),
                            active_workspace: workspace.id == tree.active,
                        });
                    }
                }
            }
        }
        let active_instance_id = active.and_then(|r| r.ids().ok()).map(|(_, _, _, tab)| tab);
        // Room heads move with every admitted publication (including this
        // peer's own echoed edits) while the searchable tabs stay identical.
        // Targets resolve by route under `generation`, not heads, so a
        // heads-only change keeps the installed data (and its revision): a
        // new revision replaces the search data and relayouts the desktop.
        if self.generation == generation
            && self.admitted == admitted
            && self.data.active_instance_id == active_instance_id
        {
            return;
        }
        self.serial = self.serial.wrapping_add(1);
        self.generation = generation.into();
        self.admitted = admitted;
        self.targets.clear();
        self.data = GlobalSearchSourceData {
            revision: format!("{generation}:{}", self.serial),
            applications: desktop::installed_applications()
                .into_iter()
                .map(|(app, definition)| GlobalSearchApplication {
                    app_id: app.into(),
                    title: definition.title.into(),
                    icon: None,
                    action: tilo_actions::ActionAddress::new(
                        "desktop:requests",
                        format!("open-{app}"),
                    ),
                })
                .collect(),
            tabs: self
                .admitted
                .iter()
                .map(|tab| {
                    let target = uuid::Uuid::new_v4().to_string();
                    self.targets.insert(target.clone(), tab.route.clone());
                    GlobalSearchTab {
                        target,
                        instance_id: tab.route.ids().expect("canonical Rust route").3,
                        app_id: tab.app.clone(),
                        record_id: tab.record.clone(),
                        title: tab.title.clone(),
                        in_active_workspace: tab.active_workspace,
                    }
                })
                .collect(),
            active_instance_id,
        };
    }
    pub(crate) fn resolve(&self, revision: &str, target: &str) -> Result<Command, String> {
        if revision != self.data.revision {
            return Err("Stale search target".into());
        }
        let route = self
            .targets
            .get(target)
            .cloned()
            .ok_or("Search target is no longer available")?;
        Ok(Command::Select {
            generation: self.generation.clone(),
            route,
        })
    }
}
impl DesktopHandle {
    pub fn sync_global_search(&self, frame: &mut FrameHarness) {
        let data = {
            let mut search = self.search.borrow_mut();
            if search.installed_revision == search.data.revision {
                return;
            }
            search.installed_revision = search.data.revision.clone();
            search.data.clone()
        };
        frame.replace_global_search_data(data);
    }
    pub fn open_global_search(
        &self,
        frame: &mut FrameHarness,
        generation: &str,
        query: &str,
    ) -> Result<(), String> {
        if generation != self.generation {
            return Err("Stale desktop surface".into());
        }
        // Apply any queued admission before installing/searching its current identities.
        frame.relayout();
        self.sync_global_search(frame);
        frame.open_global_search_all(query)
    }
    pub fn process_global_search(&self, frame: &mut FrameHarness) -> Result<(), String> {
        self.sync_global_search(frame);
        match frame.take_global_search_intent() {
            Some(GlobalSearchIntent::CallerAction(address)) => {
                frame.handoff_caller_action(&address)
            }
            Some(GlobalSearchIntent::SelectHostTarget { revision, target }) => {
                let command = self.search.borrow().resolve(&revision, &target)?;
                let result = self.navigate(frame, command);
                self.sync_global_search(frame);
                result.map(|_| ())
            }
            None => Ok(()),
        }
    }
}
