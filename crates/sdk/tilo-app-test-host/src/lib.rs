//! Testing a Tilo app the way a person's device hosts it: the app's window, opened headlessly
//! ([`tilo_app::framework::testing::HeadlessApp`]), with the workspace broker beside it.
//!
//! A window decides no permission itself. The actions of the records it shows that another
//! workspace holds (a person's profile records, held in their own workspace wherever they are
//! shown) are decided by its host, with the broker's ordinary check in the workspace holding the
//! record. [`TestApp`] answers them as that host does, so a test drives those controls as a
//! person would.
//!
//! The person's settings are kept as the session keeps them, as resources of their workspace
//! (here in memory), so what a window sets is stepped back and forth from that history.

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
    fn keeps_settings(root: &std::path::Path) {
        settings_store::keep_profile(root, || settings_sections::in_memory::attached().0);
    }

    fn opened(driver: &mut Driver, author: &str) {
        let own = workspace_of_person(author).and_then(|id| {
            let mut workspace = tilo_desktop_model::Workspace::new(
                tilo_desktop_model::WorkspaceId::parse(&id)?,
                "",
            );
            workspace.policy = WorkspacePolicy::new(id.clone(), author);
            Some((id, workspace))
        });
        driver.decide_shown_actions(Box::new(move |actor, actions| {
            let now = tilo_app::time::unix_now();
            let actor = broker_actor(actor);
            actions
                .iter()
                .map(|action| ActionDecision {
                    action: action.clone(),
                    verdict: match &actor {
                        Some(actor) => actor.shown_verdict(
                            own.as_ref()
                                .filter(|(id, _)| action.workspace.as_ref() == Some(id))
                                .map(|(_, workspace)| workspace),
                            action,
                            now,
                        ),
                        None => tilo_app::shown_actions::ActionVerdict::Refused(
                            "only a member or their agent acts here".into(),
                        ),
                    },
                })
                .collect()
        }));
    }
}

/// Who `acting` is as the broker decides for them: a person by their own grants; an agent by
/// the grants it holds there and the delegation its person's device handed it, acting for its
/// own person and never past what they may. `None` for anyone else, whom this host admits
/// nothing.
fn broker_actor(acting: &tilo_app::tilo_actions::Acting) -> Option<tilo_broker_core::BrokerActor> {
    use tilo_app::tilo_actions::MemberId;
    match acting.member_id()? {
        MemberId::Person(person) => Some(tilo_broker_core::BrokerActor::person(person.clone())),
        MemberId::Agent { id, person } => {
            let agent = tilo_broker_core::BrokerActor::agent(id.clone(), person.clone())
                .for_person(person.as_str());
            Some(if acting.handed {
                agent.session_delegated()
            } else {
                agent
            })
        }
        MemberId::Bot(_) => None,
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
