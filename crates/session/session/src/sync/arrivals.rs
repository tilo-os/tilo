//! What the person sees of who is here with them: the bar's faces for whoever is in the workspace
//! in front, the name that slides out when they come where people are, and a wave at them.

use super::Client;
use tilo_desktop::chrome::{ArrivalNotice, WaveNotice};
use tilo_desktop::here::LiveMember;

impl Client {
    /// Take the waves at this person heard since the last call: the bar says who waved, for a
    /// moment.
    pub(super) fn show_waves(&self) {
        let heard = self.here.borrow_mut().take_waves();
        if let Some(last) = heard.last() {
            *self.notice.borrow_mut() = Some(WaveNotice {
                from: last.name.clone(),
                at: ui::time::now(),
            });
        }
    }

    /// The last wave at this person, for the bar to say.
    #[must_use]
    pub fn wave_notice(&self) -> Option<WaveNotice> {
        self.notice.borrow().clone()
    }

    /// Hand the desktop this person and whoever is here with them in the workspace in front.
    pub(super) fn show_here(&self) {
        let now = super::now_ms();
        let (own, members, workspace) = {
            let plane = self.plane.borrow();
            let in_front = plane
                .active
                .and_then(|ws| plane.workspaces.iter().find(|held| held.ws == ws));
            (
                self.own_member(),
                in_front.map(|held| held.members(now)).unwrap_or_default(),
                in_front.map(|held| held.id.clone()).unwrap_or_default(),
            )
        };
        self.here
            .borrow_mut()
            .set_members(Some(own), Some(members.clone()));
        self.show_arrival(&workspace, &members);
    }

    /// The person came to a workspace where people already are: the bar's faces pulse and a short
    /// name slides out of them, once for the workspace they came to. A workspace nobody else is in
    /// says nothing.
    fn show_arrival(&self, workspace: &str, members: &[LiveMember]) {
        let people: Vec<&LiveMember> = members
            .iter()
            .filter(|member| member.kind != "agent" && !member.label.trim().is_empty())
            .collect();
        if people.is_empty() || self.arrived_in.borrow().as_deref() == Some(workspace) {
            return;
        }
        *self.arrived_in.borrow_mut() = Some(workspace.to_owned());
        *self.arrival.borrow_mut() = Some(ArrivalNotice {
            people: people
                .iter()
                .map(|member| member.publisher.clone())
                .collect(),
            names: people
                .iter()
                .map(|member| member.label.trim().to_owned())
                .collect(),
            seen: None,
            at: ui::time::now(),
        });
    }

    /// The arrival the bar shows, while its moment lasts.
    #[must_use]
    pub fn arrival_notice(&self) -> Option<ArrivalNotice> {
        self.arrival.borrow().clone()
    }
}
