//! What the overview shows, as this desktop knows it: its workspace cards with where each runs,
//! and the overview of the person's Tilo below them ([`crate::overview_tilo`]).

use super::DesktopChrome;
use crate::overview_panel::OverviewDesk;
use tilo_desktop_model::WorkspaceSlot;
use tilo_desktop_surface::HostActionSource;

impl DesktopChrome {
    pub(super) fn overview_desk(&self) -> OverviewDesk {
        let mut desk = OverviewDesk::of(self.cards());
        desk.can_join = self.may_join_workspace();
        let ids: std::collections::HashMap<WorkspaceSlot, String> = self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .map(|workspace| (workspace.slot, workspace.id().to_string()))
                .collect()
        });
        for card in &mut desk.cards {
            card.hosting = ids
                .get(&card.id)
                .and_then(|id| crate::overview_tilo::usage_line(&self.tilo.hosts, id));
        }
        let host_actions = self.handle.host_actions();
        desk.tilo = crate::overview_tilo::TiloDesk {
            // The person's workspaces the host knows that this desk does not hold: going to one is
            // the host's to carry out.
            elsewhere: host_actions
                .iter()
                .filter(|action| action.source == Some(HostActionSource::Workspaces))
                .map(|action| (action.id.clone(), action.title.clone()))
                .collect(),
            actions: self
                .tilo_menu_actions()
                .into_iter()
                .map(|action| (action.id, action.label))
                .collect(),
            feedback: self.tilo_menu.offers_feedback(),
            ..self.tilo.clone()
        };
        desk
    }

    /// What the overview of the person's Tilo shows below their workspaces: their hosts' answers
    /// for their account, the agent door's, and who they are. Shown at once while it is open.
    pub fn set_tilo(&mut self, tilo: crate::overview_tilo::TiloDesk) {
        if self.tilo == tilo {
            return;
        }
        self.tilo = tilo;
        if self.overview.is_open() {
            let desk = self.overview_desk();
            self.overview.set_desk(desk);
        }
    }
}
