//! Whom this page's person follows, told to the windows' apps: the same event a native shell's
//! apps hear, so a window shows where the person they follow works.

use ui::collaboration::CollabEvent;
use ui::headless::FrameHarness;
use ui::permissions::MemberId;

/// Whom this page's person follows, as its windows' apps are to hear it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Told {
    /// Nobody.
    Nobody,
    /// This member's view is theirs.
    Member(MemberId),
}

impl Told {
    /// What the apps are to hear of `member` (`None`: nobody).
    pub(crate) fn of(member: Option<MemberId>) -> Self {
        member.map_or(Self::Nobody, Self::Member)
    }
}

/// Tell the windows' apps whom this page's person follows, where the shell worked it out since
/// the last frame (`pending`).
pub(crate) fn tell(pending: &mut Option<Told>, frame: &mut FrameHarness) {
    let Some(told) = pending.take() else {
        return;
    };
    let peer_id = match told {
        Told::Nobody => None,
        Told::Member(member) => Some(member),
    };
    frame.handle_collaboration_event(&CollabEvent::FollowChanged { peer_id });
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::rc::Rc;

    use super::*;
    use tilo_session_core::presence::{Follow, Place, Step};
    use ui::taffy::{NodeId, Style, TaffyTree};
    use ui::widget::{TaffyTreeExt as _, Widget};
    use ui::{State, Theme};

    const WORKSPACE: &str = "work";
    const THEM: &str = "b0b0";

    /// An app of a window, as the page mounts one: every follow event it hears, kept.
    struct Probe {
        told: Rc<std::cell::RefCell<Vec<Option<MemberId>>>>,
    }

    impl ui::widget::Collaborative for Probe {
        fn handle_collab_event(&mut self, event: &CollabEvent) {
            if let CollabEvent::FollowChanged { peer_id } = event {
                self.told.borrow_mut().push(peer_id.clone());
            }
        }
    }

    impl Widget for Probe {
        ui::provides!(dyn ui::widget::Collaborative);

        fn name(&self) -> &'static str {
            "follow probe"
        }

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.widget_leaf(Style::default(), node_map, self)
        }
    }

    /// A frame of a window whose app keeps what it hears, and what it heard.
    fn window() -> (FrameHarness, Rc<std::cell::RefCell<Vec<Option<MemberId>>>>) {
        let told = Rc::default();
        let frame = FrameHarness::new(
            State::new(),
            Box::new(Probe {
                told: Rc::clone(&told),
            }),
            (240, 160),
        );
        (frame, told)
    }

    /// Where the person followed is: their window here, and the member they are.
    fn they_are_here() -> (Place<'static>, Option<MemberId>) {
        (
            Place {
                id: THEM,
                tab: Some(7),
                app: None,
            },
            Some(MemberId::person(THEM)),
        )
    }

    /// Following someone reaches the windows' apps, and so does stopping: the app of the window
    /// the page follows them in shows where they work, and theirs again once it ends.
    #[test]
    fn following_someone_tells_the_windows_apps_whose_view_it_is() {
        let (mut frame, told) = window();
        let mut follow = Follow::default();
        let (place, member) = they_are_here();
        let here = [place];

        // The person follows B: their window comes forward, and its app hears whose view is theirs.
        let owed = follow
            .owe(Some(THEM.into()), false, false, &here, member, WORKSPACE)
            .expect("the pass owes following");
        assert_eq!(
            owed.step,
            Step::Bring(tilo_session_core::presence::Target::Window(7))
        );
        let (_, member) = owed.tell.expect("the apps hear it");
        let mut pending = Some(Told::of(member));
        tell(&mut pending, &mut frame);
        assert_eq!(
            *told.borrow(),
            [Some(MemberId::person(THEM))],
            "the app hears the member the person follows"
        );
        tell(&mut pending, &mut frame);
        assert_eq!(
            *told.borrow(),
            [Some(MemberId::person(THEM))],
            "nothing more was worked out"
        );

        // They stop: the app hears that the person follows nobody, and its view is theirs again.
        let owed = follow
            .owe(None, false, false, &here, None, WORKSPACE)
            .expect("stopping is owed");
        assert_eq!(owed.step, Step::Rest);
        let (_, member) = owed.tell.expect("the apps hear it");
        let mut pending = Some(Told::of(member));
        tell(&mut pending, &mut frame);
        assert_eq!(*told.borrow(), [Some(MemberId::person(THEM)), None]);
    }
}
