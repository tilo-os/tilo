//! Testing a Tilo app the way a person's device hosts it: the app's window, opened headlessly
//! ([`tilo_app::framework::testing::HeadlessApp`]), with the workspace broker beside it.
//!
//! A window decides no permission itself. The actions of the records it shows that another
//! workspace holds (a person's profile records, held in their own workspace wherever they are
//! shown) are decided by its host, with the broker's ordinary check in the workspace holding the
//! record. [`TestApp`] answers them as that host does, so a test drives those controls as a
//! person would.

use tilo_app::framework::testing::{HeadlessApp, Host};
use tilo_app::framework::{ResourceApp, testing::check_scenarios_reach_every_action};
use tilo_app::headless::Driver;
use tilo_app::shown_actions::ActionDecision;
use tilo_app::tilo_actions::policy::{WorkspacePolicy, roles::workspace_of_person};

/// A framework app opened on a fresh profile, hosted as the person's device hosts it
/// ([`Broker`]).
pub type TestApp<A> = HeadlessApp<A, Broker>;

/// The workspace broker beside a test's window: this host holds the person's own workspace,
/// founded with them as its owner, which holds their profile records; any other workspace is not
/// held here.
pub struct Broker;

impl Host for Broker {
    fn opened(driver: &mut Driver, author: &str) {
        let actor = tilo_broker_core::BrokerActor::person(author);
        let own = workspace_of_person(author).and_then(|id| {
            let mut workspace = tilo_desktop_model::Workspace::new(
                tilo_desktop_model::WorkspaceId::parse(&id)?,
                "",
            );
            workspace.policy = WorkspacePolicy::new(id.clone(), author);
            Some((id, workspace))
        });
        driver.decide_shown_actions(Box::new(move |actions| {
            let now = tilo_app::time::unix_now();
            actions
                .iter()
                .map(|action| ActionDecision {
                    action: action.clone(),
                    verdict: actor.shown_verdict(
                        own.as_ref()
                            .filter(|(id, _)| action.workspace.as_ref() == Some(id))
                            .map(|(_, workspace)| workspace),
                        action,
                        now,
                    ),
                })
                .collect()
        }));
    }
}

/// Drive the `share`th of `shares` parts of the scenarios `A` stages, and each of the `long`
/// ones, in a [`TestApp`], and say which published actions a drive could not reach
/// ([`check_scenarios_reach_every_action`]); what [`reach_tests!`] runs.
///
/// # Errors
/// Each scenario that did not open, or whose drive left published actions unreachable.
pub fn reach_every_action<A: ResourceApp>(
    share: usize,
    shares: usize,
    long: &[&str],
) -> Result<(), String> {
    check_scenarios_reach_every_action::<A, Broker>(share, shares, long)
}

/// Declare the test that a drive of every scenario of an app reaches every
/// action it publishes ([`reach_every_action`]): none is advertised yet
/// handled only by a click. The scenarios stage fixtures; the app someone
/// opens acts on this machine (its files, its processes), so it is not walked.
///
/// `#[cfg(test)] tilo_app_test_host::reach_tests!(Shell);` declares one test.
/// An app whose scenarios take long to walk names one test per share,
/// `shares: [share_0, share_1]`, which run side by side and together walk
/// every scenario; `long: ["scenario"]` names scenarios publishing so many
/// actions (one set per row of a long list) that every share asks its own
/// part of them.
#[macro_export]
macro_rules! reach_tests {
    ($app:ty) => {
        $crate::reach_tests!($app, shares: [a_drive_of_every_scenario_reaches_every_published_action]);
    };
    ($app:ty, shares: [$($test:ident),+ $(,)?] $(, long: [$($long:literal),* $(,)?])? $(,)?) => {
        mod reach_tests {
            #[allow(unused_imports, reason = "the app type may be named by a path")]
            use super::*;

            const SHARES: &[&str] = &[$(stringify!($test)),+];
            const LONG: &[&str] = &[$($($long),*)?];

            $(
                #[test]
                fn $test() {
                    let share = SHARES
                        .iter()
                        .position(|test| *test == stringify!($test))
                        .unwrap_or_default();
                    if let Err(failures) =
                        $crate::reach_every_action::<$app>(share, SHARES.len(), LONG)
                    {
                        panic!("{failures}");
                    }
                }
            )+
        }
    };
}
