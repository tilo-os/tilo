//! An app the person installed, held at its first start in a workspace while they are asked
//! what network it may reach ([`crate::network_reach`]).

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::future::Future as _;

use tilo_app_launch::{Launch, Workspace};
use tilo_service::{Answer, Asked, Questions};

use super::Apps;

/// What [`Apps`] asks about apps' network, and what it asked.
#[derive(Default)]
pub(super) struct Reach {
    /// Where the person is asked, once the session lent it ([`Apps::lend_questions`]).
    questions: RefCell<Option<Questions>>,
    /// The questions up for the person, each with the launch it holds.
    waiting: RefCell<Vec<(Asked, ReachFirst)>>,
    /// The apps, by workspace and app, whose network this session has looked at: each is asked
    /// about at most once a session, so an app the person declined starts without it.
    seen: RefCell<BTreeSet<(String, String)>>,
}

/// A launch held until the person answered what the app it starts may reach.
pub(crate) struct ReachFirst {
    pub(crate) app: String,
    pub(crate) workspace: String,
    /// The hosts the app declares, `*` for any.
    pub(crate) hosts: Vec<String>,
    launch: Launch,
    new_window: bool,
    instance: Option<u64>,
    reveal: Option<String>,
}

impl Apps {
    /// Lend where the person is asked what an app may reach before it first starts.
    pub(crate) fn lend_questions(&self, questions: Questions) {
        *self.reach.questions.borrow_mut() = Some(questions);
    }

    /// Hold `launch` while the person is asked what its app may reach, when it is an app they
    /// installed whose declared network nobody in `workspace` answered yet
    /// ([`Workspace::unanswered_reach`]). Whether it was held.
    pub(super) fn held_for_reach(
        &self,
        launch: &Launch,
        workspace: &Workspace,
        new_window: bool,
        instance: Option<u64>,
        reveal: Option<&str>,
    ) -> bool {
        let (Some(app), true) = (launch.package(), workspace.is_workspace()) else {
            return false;
        };
        let Some(questions) = self.reach.questions.borrow().clone() else {
            return false;
        };
        if !self
            .reach
            .seen
            .borrow_mut()
            .insert((workspace.id.clone(), app.to_owned()))
        {
            return false;
        }
        let Some(installed) = app_catalogue::apps::read_one_from_env(app).ok().flatten() else {
            return false;
        };
        let Some(hosts) = workspace.unanswered_reach(
            app,
            &installed.manifest.sandbox.net,
            package_store::layout::image_shipped(&installed.payload_root),
        ) else {
            return false;
        };
        let first = ReachFirst {
            app: app.to_owned(),
            workspace: workspace.id.clone(),
            hosts,
            launch: launch.clone(),
            new_window,
            instance,
            reveal: reveal.map(str::to_owned),
        };
        let asked = crate::network_reach::ask(&questions, &first);
        self.reach.waiting.borrow_mut().push((asked, first));
        true
    }

    /// The held launches the person answered since the last look, with their answers: `None`
    /// for one they declined, or one someone else in the workspace answered meanwhile. The
    /// question about that one is withdrawn, as the answer is the workspace's: the member who
    /// allowed it on their own device answered it for everyone, and the launch starts with
    /// what the workspace grants now.
    pub(crate) fn answered_reach(&self) -> Vec<(Option<Answer>, ReachFirst)> {
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let mut answered = Vec::new();
        let asked = std::mem::take(&mut *self.reach.waiting.borrow_mut());
        for (mut asked, first) in asked {
            match std::pin::Pin::new(&mut asked).poll(&mut context) {
                std::task::Poll::Ready(answer) => answered.push((answer, first)),
                std::task::Poll::Pending
                    if self
                        .workspace_named(&first.workspace)
                        .answers_reach(&first.app) =>
                {
                    drop(asked);
                    answered.push((None, first));
                }
                std::task::Poll::Pending => self.reach.waiting.borrow_mut().push((asked, first)),
            }
        }
        answered
    }

    /// Start the launch held for `first` , now that the person answered: with what the workspace
    /// grants its app now.
    pub(crate) fn start_reached(&self, first: &ReachFirst) {
        let workspace = self.workspace_named(&first.workspace);
        if let Err(error) = self.start_window(
            &first.launch,
            &workspace,
            first.new_window,
            first.instance,
            first.reveal.as_deref(),
        ) {
            tracing::warn!(app = %first.app, ?error, "the app did not start after its network was answered");
        }
    }
}
