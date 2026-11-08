//! The community a computer is set up for (`community.toml`), joined as its session starts: nobody
//! opens a directory, and the site's door is asked once, through the same join the sheet's Join
//! asks.
//! What the person decided is theirs, and is kept with their workspaces: a later session here, and
//! every other computer of theirs, joins nothing on its own once they joined the community — or
//! joined it and left. A seat that could not be reached is asked once more when the machine's
//! network comes back, and never in a loop.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use test_http::{Reply, Responder};
use tilo_compositor::OutputConfig;
use tilo_desktop::desk_verbs::CHROME_RESOURCE;
use tilo_session::{Options, Session};

use crate::{
    COMMUNITY_WORKSPACE, HEIGHT, WIDTH, a_device, community_site, held, invitation_for,
    this_computer, wait_until,
};

/// A computer's session, keeping the desk it is left with beside `state` : the same session that
/// starts where the last one ended, beside what the person decided there.
fn session_keeping(state: &std::path::Path) -> Session {
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            state: Some(state.to_path_buf()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session")
}

/// Let the session run `turns` turns: what a session has had the chance to do by, and what shows a
/// thing it never does.
fn run_turns(session: &mut Session, turns: u32) {
    for _ in 0..turns {
        session
            .step(Duration::from_millis(1))
            .expect("the session steps");
    }
}

/// The community's workspace on this computer's desk, under the name its site calls it, when it is
/// there.
fn community_here(session: &Session) -> Option<String> {
    held(session)
        .into_iter()
        .find(|(_, _, workspace)| workspace == COMMUNITY_WORKSPACE)
        .map(|(_, name, _)| name)
}

/// What it takes for the person's decisions to be on disk: a session writes them out as it goes,
/// and one that ends here stands for the session ending.
fn written() {
    tilo_durable_writer::global().flush();
}

/// A community site: its entry names the home workspace `name`, and its door
/// admits `person` into [`COMMUNITY_WORKSPACE`] while `admits` says so,
/// counting every join asked of it.
fn community_lounge(
    name: &'static str,
    person: [u8; 32],
    admits: Arc<AtomicBool>,
    asked: Arc<AtomicU32>,
) -> Responder {
    let invitation = invitation_for(COMMUNITY_WORKSPACE, person);
    Responder::handle(move |request| {
        let body = match (request.method.as_str(), request.target.as_str()) {
            ("GET", "/entry.json") => serde_json::json!({
                "home": {
                    "name": name,
                    "host": { "origin": "https://rooms.tilo.coffee/" },
                    "admission": "/directory/",
                },
            })
            .to_string(),
            ("POST", "/directory/join") => {
                asked.fetch_add(1, Ordering::SeqCst);
                if !admits.load(Ordering::SeqCst) {
                    return Some(
                        Reply::status(503)
                            .header("content-type", "application/json")
                            .body(r#"{"error":"the lounge is full"}"#),
                    );
                }
                invitation.clone()
            }
            _ => return Some(Reply::status(404)),
        };
        Some(
            Reply::status(200)
                .header("content-type", "application/json")
                .body(body),
        )
    })
}

/// The connection this machine is on, as the connectivity service reports it: what the test plugs
/// in.
struct Cable {
    network: Rc<RefCell<tilo_connectivity_service::model::Network>>,
    /// Whether the next turn brings news of it, as the service publishes a connection that changed.
    news: Rc<Cell<bool>>,
}

impl Cable {
    /// A machine with nothing plugged in.
    fn offline() -> Self {
        let network = tilo_connectivity_service::model::Network {
            link: Some(tilo_connectivity_service::model::Link {
                kind: tilo_connectivity_service::model::LinkKind::Offline,
                name: None,
                signal: 0,
            }),
            ..tilo_connectivity_service::model::Network::default()
        };
        Self {
            network: Rc::new(RefCell::new(network)),
            news: Rc::new(Cell::new(false)),
        }
    }

    /// The service the session reaches, reporting this connection.
    fn service(&self) -> Self {
        Self {
            network: Rc::clone(&self.network),
            news: Rc::clone(&self.news),
        }
    }

    /// A cable is plugged in, which the service has news of.
    fn plugged_in(&self) {
        self.network.borrow_mut().link = Some(tilo_connectivity_service::model::Link {
            kind: tilo_connectivity_service::model::LinkKind::Wired,
            name: Some("Home".to_owned()),
            signal: 0,
        });
        self.news.set(true);
    }
}

impl tilo_session::Service for Cable {
    fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        vec![tilo_connectivity_service::network_resource(
            &self.network.borrow(),
        )]
    }

    fn read(&self, resource: &str) -> Option<serde_json::Value> {
        (resource == tilo_connectivity_service::NETWORK)
            .then(|| serde_json::to_value(&*self.network.borrow()).ok())
            .flatten()
    }

    fn invoke(
        &mut self,
        _: &str,
        _: &str,
        _: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_service::Started, tilo_actions::DoorError> {
        Ok(tilo_service::Started::Accepted)
    }

    fn follow(&mut self) -> Result<bool, String> {
        Ok(self.news.replace(false))
    }
}

/// The person `person` 's other computer: another device of theirs, certified for the same person
/// and in use from here on. Returns the root's directory.
fn their_other_computer(env: &mut tilo_test_env::ScopedEnv) -> tempfile::TempDir {
    let person = ed25519_dalek::SigningKey::from_bytes(&[0x21; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[0x24; 32]);
    a_device(env, &person, &device, "The desktop")
}

/// A computer set up for a community joins it as its session starts, with nobody opening a
/// directory: the same door, and the same verified invitation, the sheet's Join asks.
#[test]
fn a_computer_set_up_for_a_community_joins_it_as_its_session_starts() {
    let _settled = ui::motion::testing::settled();
    let mut env = tilo_test_env::ScopedEnv::new();
    let (_identity, person) = this_computer(&mut env);
    let asked = Arc::new(AtomicU32::new(0));
    let responder = community_lounge(
        "tilo.coffee",
        person,
        Arc::new(AtomicBool::new(true)),
        Arc::clone(&asked),
    );
    let _config = community_site(&mut env, &responder.url());
    let state = tempfile::tempdir().expect("a directory for this computer's state");
    let mut session = session_keeping(state.path());
    wait_until(
        &mut session,
        "the community's workspace is on the desk",
        |session| community_here(session).is_some(),
    );
    assert_eq!(
        community_here(&session).as_deref(),
        Some("tilo.coffee"),
        "the site's home workspace, under the name the site calls it"
    );
    assert_eq!(
        asked.load(Ordering::SeqCst),
        1,
        "the site's door was asked once, and not again"
    );
    let joins: Vec<test_http::Request> = responder
        .drain_requests()
        .into_iter()
        .filter(|request| request.method == "POST" && request.target.ends_with("/join"))
        .collect();
    assert_eq!(joins.len(), 1, "one join was asked");
    assert_eq!(
        joins[0].target, "/directory/join",
        "at the door the site names"
    );
    let body: serde_json::Value =
        serde_json::from_slice(&joins[0].body).expect("the join request a door reads");
    assert_eq!(
        body,
        serde_json::json!({
            "userPublicKey": hex::encode(person),
            "displayName": "Ada",
        }),
        "as this person, by the name they go by rather than their device's"
    );
}

/// What the person decided is theirs, kept beside their workspaces: a later session on the same
/// computer joins nothing again, and neither does it put back a community they left.
#[test]
fn a_community_that_was_left_is_not_joined_again() {
    let _settled = ui::motion::testing::settled();
    let mut env = tilo_test_env::ScopedEnv::new();
    let (_identity, person) = this_computer(&mut env);
    let asked = Arc::new(AtomicU32::new(0));
    let responder = community_lounge(
        "tilo.coffee",
        person,
        Arc::new(AtomicBool::new(true)),
        Arc::clone(&asked),
    );
    let _config = community_site(&mut env, &responder.url());
    let state = tempfile::tempdir().expect("a directory for this computer's state");
    let mut session = session_keeping(state.path());
    wait_until(
        &mut session,
        "the community's workspace is on the desk",
        |session| community_here(session).is_some(),
    );
    // They leave it as they leave any workspace, from its own menu.
    let (slot, name, _) = held(&session)
        .into_iter()
        .find(|(_, _, workspace)| workspace == COMMUNITY_WORKSPACE)
        .expect("the community's workspace is on the desk");
    assert_eq!(name, "tilo.coffee");
    let arguments = serde_json::json!({ "workspace": slot })
        .as_object()
        .expect("an object")
        .clone();
    session
        .invoke(CHROME_RESOURCE, "leave-workspace", &arguments)
        .expect("the person leaves the community's workspace");
    assert_eq!(
        community_here(&session),
        None,
        "the community's workspace is off the desk"
    );
    written();
    drop(session);
    let mut again = session_keeping(state.path());
    run_turns(&mut again, 20);
    assert_eq!(
        asked.load(Ordering::SeqCst),
        1,
        "leaving is a decision of theirs: the next session asks for nothing"
    );
    assert_eq!(
        community_here(&again),
        None,
        "and the workspace they left stays off the desk"
    );
}

/// A person's other computer holds what they decided, as their devices hold their workspaces: it
/// does not ask the community's door for them again.
#[test]
fn a_persons_other_computer_does_not_join_their_community_again() {
    let _settled = ui::motion::testing::settled();
    let mut env = tilo_test_env::ScopedEnv::new();
    let (_identity, person) = this_computer(&mut env);
    let asked = Arc::new(AtomicU32::new(0));
    let responder = community_lounge(
        "tilo.coffee",
        person,
        Arc::new(AtomicBool::new(true)),
        Arc::clone(&asked),
    );
    let _config = community_site(&mut env, &responder.url());
    let state = tempfile::tempdir().expect("a directory for this computer's state");
    let mut session = session_keeping(state.path());
    wait_until(
        &mut session,
        "the community's workspace is on the desk",
        |session| community_here(session).is_some(),
    );
    written();
    drop(session);
    // Their desktop: another device of theirs, holding the list their laptop agreed with them, as
    // their devices hand each other their workspaces.
    let _root = their_other_computer(&mut env);
    let elsewhere = tempfile::tempdir().expect("a directory for their other computer's state");
    std::fs::create_dir_all(elsewhere.path().join("tilo")).expect("the state's directory");
    std::fs::copy(
        state
            .path()
            .join("tilo")
            .join("personal-workspaces.postcard"),
        elsewhere
            .path()
            .join("tilo")
            .join("personal-workspaces.postcard"),
    )
    .expect("what their laptop decided reaches their desktop");
    let mut other = session_keeping(elsewhere.path());
    run_turns(&mut other, 20);
    assert_eq!(
        asked.load(Ordering::SeqCst),
        1,
        "their other computer asks nobody: they decided already"
    );
    assert_eq!(
        community_here(&other),
        None,
        "and it joins nothing on its own"
    );
}

/// A computer whose administrator names no community asks nobody: an empty site is a decision not
/// to have one.
#[test]
fn a_computer_whose_administrator_names_no_community_joins_nothing() {
    let _settled = ui::motion::testing::settled();
    let mut env = tilo_test_env::ScopedEnv::new();
    let (_identity, _person) = this_computer(&mut env);
    let _config = community_site(&mut env, "");
    let state = tempfile::tempdir().expect("a directory for this computer's state");
    let mut session = session_keeping(state.path());
    run_turns(&mut session, 20);
    assert_eq!(
        community_here(&session),
        None,
        "no site is named, so no community's workspace is joined"
    );
}

/// A seat that cannot be reached is not hammered: the join is asked once, and once more when the
/// machine's network comes back, which is when a join that never reached its site can land.
#[test]
fn a_community_that_could_not_be_reached_is_asked_again_when_a_network_comes_back() {
    let _settled = ui::motion::testing::settled();
    let mut env = tilo_test_env::ScopedEnv::new();
    let (_identity, person) = this_computer(&mut env);
    let asked = Arc::new(AtomicU32::new(0));
    let admits = Arc::new(AtomicBool::new(false));
    let responder = community_lounge(
        "tilo.coffee",
        person,
        Arc::clone(&admits),
        Arc::clone(&asked),
    );
    let _config = community_site(&mut env, &responder.url());
    let state = tempfile::tempdir().expect("a directory for this computer's state");
    let cable = Cable::offline();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            state: Some(state.path().to_path_buf()),
            services: vec![Box::new(cable.service())],
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    wait_until(&mut session, "the community's door was asked", |_| {
        asked.load(Ordering::SeqCst) == 1
    });
    run_turns(&mut session, 10);
    assert_eq!(
        asked.load(Ordering::SeqCst),
        1,
        "a door that refused is not asked again in a loop"
    );
    assert_eq!(
        community_here(&session),
        None,
        "and the community's workspace is nowhere yet"
    );
    // The cable is plugged in, and the seat has room again.
    admits.store(true, Ordering::SeqCst);
    cable.plugged_in();
    wait_until(
        &mut session,
        "the community's workspace is on the desk",
        |session| community_here(session).is_some(),
    );
    run_turns(&mut session, 10);
    assert_eq!(
        asked.load(Ordering::SeqCst),
        2,
        "asked once more when a network came back, not again after that"
    );
    assert_eq!(
        community_here(&session).as_deref(),
        Some("tilo.coffee"),
        "and the join that waited landed"
    );
}
