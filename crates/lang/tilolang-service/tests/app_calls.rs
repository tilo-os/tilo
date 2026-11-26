//! What an app the session's service runs calls over the connection its daemon vouches for as the
//! app: a person's own press runs the app's code in place, and a call that has to ask goes out, its
//! invocation answered once the dispatch decided it, a refusal in the dispatch's words.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_actions::remote::{ActionCallError, InvokeRequest, PendingPoll, ReceiptStatus};
use tilo_service::Service as _;

use crate::fixtures::{Replica, Workspace};

const APP: &str = "test.player";
const SOURCE: (&str, &str) = (
    r#"app test.player "Player"

type Player {
    plays: Int merge counter
}

root Player

default view player for Player

view player(p: Player) {
    text("{p.plays} plays")
}

/// Asks the service for the stream: reaching outside, which the code of `play` does for it.
action fetch(p: Player) {
    p.plays += 1
    outside("youtube.stream", ("key", "720p"))
}

action play(p: Player) {
    fetch(p)
}
"#,
    "",
);

const WORKSPACE: &str = "6f1d9c2e-4b3a-4f8e-9a51-0c2d7e8b1a44";

/// What the dispatch was handed, each call with how to end it.
type Sent = Rc<RefCell<Vec<(InvokeRequest, Box<dyn FnOnce(ReceiptStatus) + Send>)>>>;

/// The app's connection, as the daemon's dispatch would take its calls: kept for the test to end.
struct Dispatch(Sent);

impl tilo_service::AppCalls for Dispatch {
    fn send(
        &self,
        request: InvokeRequest,
        ended: Box<dyn FnOnce(ReceiptStatus) + Send>,
    ) -> Result<(), String> {
        self.0.borrow_mut().push((request, ended));
        Ok(())
    }
}

/// The player's service, its calls reaching `sent`, the app's resource, and the id its count of plays reads
/// under.
fn player() -> (tilolang_service::ResourceService, Sent, String, String) {
    let (ada, loaded) = Replica::new("ada", Workspace::new(), &[SOURCE]);
    let field = crate::fixtures::field_id(&loaded.last().expect("the app").meta, "Player", "plays");
    let mut service = tilolang_service::ResourceService::of_app(APP, ada.host, "ada");
    let sent = Sent::default();
    let calls = Rc::clone(&sent);
    let connect: tilo_service::ConnectApp = Rc::new(move |from| {
        assert_eq!(
            from.app, APP,
            "the connection is vouched for as the app itself"
        );
        Ok(Rc::new(Dispatch(Rc::clone(&calls))) as Rc<dyn tilo_service::AppCalls>)
    });
    service.reach_workspace(&connect, WORKSPACE, std::sync::Arc::new(|| ()));
    // The app's resource, which its root is made under on the first action that reaches it.
    let root = format!("tl:{APP}");
    (service, sent, root, field)
}

/// How many times the player's root says it played, its count read under `field`: none before
/// the root is made.
fn plays(service: &tilolang_service::ResourceService, field: &str) -> i64 {
    service
        .resources()
        .iter()
        .flat_map(|resource| std::iter::once(resource).chain(&resource.children))
        .filter(|resource| resource.kind == format!("tl.{APP}.Player"))
        .filter_map(|resource| service.read(resource.id.as_str()))
        .filter_map(|reading| reading.get(field).and_then(serde_json::Value::as_i64))
        .sum()
}

tilo_test_env::scenario! {
    fn an_apps_own_code_runs_in_place_on_a_press_and_asks_through_the_dispatch_otherwise;

    phase a_press_runs_what_the_code_calls_of_its_own_in_place;
    {
        let (mut service, sent, root, field) = player();
        let started = tilolang_tilo::page::answering((None, true), || {
            service.invoke(&root, "play", &serde_json::Map::new())
        })
        .expect("the press plays");
        assert!(
            matches!(started, tilo_service::Started::Done(_)),
            "the press is done at once: {started:?}"
        );
        assert!(sent.borrow().is_empty(), "nothing went through the dispatch");
        assert_eq!(plays(&service, &field), 1, "the stream was asked for once");
    }

    phase a_call_the_dispatch_refuses_ends_in_its_words;
    {
        let (mut service, sent, root, field) = player();
        let started = service
            .invoke(&root, "play", &serde_json::Map::new())
            .expect("the call is taken");
        let tilo_service::Started::Later(mut answer) = started else {
            panic!("a call that asks is answered once the dispatch decided it: {started:?}");
        };
        let (request, ended) = sent.borrow_mut().pop().expect("the ask went to the dispatch");
        assert_eq!(request.target.address.action, "fetch", "what the code asked for");
        assert!(matches!(answer.poll_ready(), PendingPoll::Waiting));
        ended(ReceiptStatus::Rejected {
            error: ActionCallError::Refused {
                reason: "Player may not reach the stream service".to_owned(),
            },
        });
        service.poll();
        let PendingPoll::Ready(Err(error)) = answer.poll_ready() else {
            panic!("the refusal ends the invocation");
        };
        assert!(
            error.to_string().contains("Player may not reach the stream service"),
            "the person reads the dispatch's words: {error}"
        );
        assert_eq!(plays(&service, &field), 0, "what the refused run wrote is taken back");
    }

    phase a_question_the_person_declines_ends_the_call;
    {
        let (mut service, sent, root, field) = player();
        let started = service
            .invoke(&root, "play", &serde_json::Map::new())
            .expect("the call is taken");
        let tilo_service::Started::Later(mut answer) = started else {
            panic!("a call that asks is answered once the person answered: {started:?}");
        };
        let (_, ended) = sent.borrow_mut().pop().expect("the ask went to the dispatch");
        ended(ReceiptStatus::Cancelled {
            message: Some("declined".to_owned()),
        });
        service.poll();
        let PendingPoll::Ready(Err(error)) = answer.poll_ready() else {
            panic!("the decline ends the invocation");
        };
        assert!(error.to_string().contains("declined"), "{error}");
        assert_eq!(plays(&service, &field), 0, "nothing of the declined run stays");
    }
}
