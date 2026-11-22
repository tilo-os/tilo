//! What a reader outside tilolang reads of a resource through the API's `read`, as the session
//! answers it: who made each item, when and from what; a page of a collection matched by its
//! items' fields or by what they were made from, read from the index however large the
//! collection; and never more than the reader's grants let them read, whichever way they ask.

use std::rc::Rc;
use std::time::Instant;

use automerge::AutoCommit;
use serde_json::{Map, Value as Json, json};
use tilo_action_policy::{
    Answer, Grant, Grantee, KindSet, MemberAccess, Role, TreePath, WorkspacePolicy,
};
use tilo_actions::ActionInvoker;
use tilo_service::{Service, Started};
use tilolang_runtime::value::{ActionRef, ResourceRef};
use tilolang_runtime::{Actor, InvokeOutcome, Meta, Value};
use tilolang_tilo::meta::MetaExt as _;
use tilolang_tilo::publish::resource_id;
use tilolang_tilo::resource_service::ResourceService;
use tilolang_tilo::runtime::ids::MemberId;

use crate::fixtures::{Replica, action_id, field_id, type_id};

const SOURCE: &str = r#"app test.read "Read"

type Board {
    posts: Collection<Post>
    asks: Collection<Ask>
    replies: Collection<Reply>
}

type Post {
    text: Text
    pinned: Bool = false
}

type Ask {
    about: Text
}

type Reply {
    ask: Ask
    text: Text
}

root Board

default view board for Board

view board(b: Board) {
    page {
        for p in b.posts {
            text(p.text)
        }
    }
}

action post(b: Board, text: Text) -> Post {
    b.posts.insert(Post(text: text))
}

action ask(b: Board, about: Text) -> Ask {
    b.asks.insert(Ask(about: about))
}

action reply(b: Board, a: Ask, text: Text) -> Reply {
    b.replies.insert(Reply(ask: a, text: text))
}

"#;

const APP: &str = "test.read";
const WORKSPACE: &str = "workspace";

fn call(replica: &Replica, who: &str, action: ActionRef) -> Value {
    match replica
        .host
        .runtime()
        .invoke_action(&Actor::from(MemberId::person(who)), action)
        .unwrap()
    {
        InvokeOutcome::Done(value) => value,
        other => panic!("{other:?}"),
    }
}

fn made(value: &Value) -> ResourceRef {
    match value {
        Value::Ref(made) => (*made.target).clone(),
        other => panic!("the action makes a resource: {other:?}"),
    }
}

/// `read` of `resource` as `who`, through the session's published `read`.
fn read(
    service: &mut ResourceService,
    who: &str,
    resource: &str,
    args: Json,
) -> Result<Json, String> {
    let args: Map<String, Json> = args.as_object().cloned().unwrap_or_default();
    match service.invoke_as(&ActionInvoker::person(who), resource, "read", &args) {
        Ok(Started::Done(output)) => output.value.ok_or_else(|| "a read answers".to_owned()),
        Ok(other) => Err(format!("a read is done at once: {other:?}")),
        Err(error) => Err(error.to_string()),
    }
}

fn published(at: &ResourceRef) -> String {
    resource_id(APP, &at.id_string())
}

/// The published ids of a page's items.
fn items(page: &Json) -> Vec<String> {
    page["items"]
        .as_array()
        .unwrap_or_else(|| panic!("a page lists items: {page}"))
        .iter()
        .map(|item| item["item"].as_str().unwrap().to_owned())
        .collect()
}

/// Ada's board holding `count` posts, every hundredth pinned, made in one change.
fn seeded(count: usize) -> (Replica, Meta, ResourceRef) {
    use tilo_resource::{Collection, Id, Model as _};
    use tilolang_tilo::action::{DynAction, DynEdit};
    use tilolang_tilo::resource::{DynResource, DynValue};

    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(SOURCE, "")]);
    let meta = packages[0].meta.clone();
    let board = ada.host.root_of(APP).unwrap();
    ada.commit();
    let post = tilolang_tilo::schema::lookup(&type_id(&meta, "Post")).unwrap();
    let mut posts = Collection::default();
    for index in 0..count {
        let mut item = DynResource::empty(post).unwrap();
        if let Some(DynValue::Text(text)) = item.get_mut(&field_id(&meta, "Post", "text")) {
            *text = format!("post {index:05}");
        }
        if let Some(DynValue::Register(pinned)) = item.get_mut(&field_id(&meta, "Post", "pinned")) {
            *pinned = json!(index % 100 == 7);
        }
        posts.push(Id::from_raw(format!("post-{index:05}")), item);
    }
    let mut root =
        DynResource::empty(tilolang_tilo::schema::lookup(&type_id(&meta, "Board")).unwrap())
            .unwrap();
    if let Some(DynValue::Collection(slot)) = root.get_mut(&field_id(&meta, "Board", "posts")) {
        *slot = posts;
    }
    ada.apply(&DynAction {
        app: APP.to_owned(),
        kind: format!("tl.{APP}.seed"),
        action_id: "$create".to_owned(),
        args: Json::Null,
        authorship: tilolang_tilo::Authorship::default(),
        edits: vec![DynEdit::Create {
            id: board.id_string(),
            record: root.to_json(),
        }],
    });
    ada.commit();
    (ada, meta, board)
}

#[test]
fn a_page_of_ten_thousand_is_read_from_the_index() {
    let (ada, _meta, board) = seeded(10_000);
    let mut service = ada.into_service("ada");
    let board = published(&board);
    let before = service.host().store().read_stats();
    let started = Instant::now();
    let first = read(
        &mut service,
        "ada",
        &board,
        json!({"field": "posts", "where": {"pinned": true}, "limit": 30}),
    )
    .unwrap();
    let page = &first["fields"]["posts"];
    let found = items(page);
    assert_eq!(found.len(), 30, "{page}");
    assert_eq!(found[0], format!("{board}/post-00007"));
    assert_eq!(found[1], format!("{board}/post-00107"));
    let next = page["next"].as_str().expect("more match than a page");
    assert_eq!(next, format!("{board}/post-02907"));
    let second = read(
        &mut service,
        "ada",
        &board,
        json!({"field": "posts", "where": {"pinned": true}, "after": next, "limit": 30}),
    )
    .unwrap();
    let rest = items(&second["fields"]["posts"]);
    let elapsed = started.elapsed();
    assert_eq!(rest.len(), 30);
    assert_eq!(rest[0], format!("{board}/post-03007"));
    // A page without a match reads in the collection's order from where it starts.
    let plain = read(
        &mut service,
        "ada",
        &board,
        json!({"field": "posts", "after": format!("{board}/post-09998")}),
    )
    .unwrap();
    assert_eq!(
        items(&plain["fields"]["posts"]),
        vec![format!("{board}/post-09999")]
    );
    assert!(plain["fields"]["posts"].get("next").is_none());
    let after = service.host().store().read_stats();
    assert_eq!(
        after.evaluated, before.evaluated,
        "the column of `pinned` answers the match: no item is read to test it"
    );
    assert_eq!(
        after.versions, before.versions,
        "who made each item is read from the index, not derived from the log"
    );
    eprintln!("two pages of a 10k collection, matched: {elapsed:?}");
}

/// Ada's board with a post of hers, an ask Grace made from it, and the agent Scout's reply to
/// the ask; and a secret post with its own ask and reply.
struct Chain {
    service: ResourceService,
    meta: Meta,
    board: ResourceRef,
    post: ResourceRef,
    ask: ResourceRef,
    reply: ResourceRef,
    secret: ResourceRef,
    secret_ask: ResourceRef,
    secret_reply: ResourceRef,
}

fn chain() -> Chain {
    // The session keeps the board in a host of its own, deciding by the person's access in the
    // workspace the session keeps the board in, as a seeded app's host is made.
    let access = tilo_service::WorkspaceAccess::default();
    let source = Rc::clone(&access);
    let (mut ada, packages) =
        Replica::following("ada", AutoCommit::new(), &[(SOURCE, "")], move || {
            source.borrow().as_ref().and_then(|access| access())
        });
    let meta = packages[0].meta.clone();
    let board = ada.host.root_of(APP).unwrap();
    ada.commit();
    let on_board = |action: &str, args: Vec<Value>| {
        let mut all = vec![Value::resource(&board)];
        all.extend(args);
        ActionRef::new(action_id(&meta, action), all)
    };
    let text = |text: &str| Value::Text(text.to_owned());
    let post = made(&call(
        &ada,
        "ada",
        on_board("post", vec![text("Lunch at noon?")]),
    ));
    ada.commit_as("ada");
    let ask = made(&call(
        &ada,
        "grace",
        on_board("ask", vec![text("summarise")]).made_from(Some(&post)),
    ));
    ada.commit_as("grace");
    let scout = Actor::from(MemberId::agent("scout"));
    let reply = match ada
        .host
        .runtime()
        .invoke_action(
            &scout,
            on_board("reply", vec![Value::resource(&ask), text("Lunch, noon.")]),
        )
        .unwrap()
    {
        InvokeOutcome::Done(value) => made(&value),
        other => panic!("{other:?}"),
    };
    ada.commit_as(&scout);
    let secret = made(&call(
        &ada,
        "ada",
        on_board("post", vec![text("The offer is 40k")]),
    ));
    ada.commit_as("ada");
    let secret_ask = made(&call(
        &ada,
        "ada",
        on_board("ask", vec![text("draft a reply")]).made_from(Some(&secret)),
    ));
    ada.commit_as("ada");
    let secret_reply = made(&call(
        &ada,
        "ada",
        on_board(
            "reply",
            vec![Value::resource(&secret_ask), text("Counter at 45k")],
        ),
    ));
    ada.commit_as("ada");
    Chain {
        service: ResourceService::of_app(APP, ada.host, "ada", access),
        meta,
        board,
        post,
        ask,
        reply,
        secret,
        secret_ask,
        secret_reply,
    }
}

#[test]
fn every_item_says_who_made_it_when_and_from_what() {
    let mut chain = chain();
    let board = published(&chain.board);

    // A message: who wrote it, and when.
    let post = read(
        &mut chain.service,
        "ada",
        &published(&chain.post),
        json!({}),
    )
    .unwrap();
    assert_eq!(post["fields"]["text"], "Lunch at noon?");
    assert_eq!(
        post["created"]["who"]["actor"]["member"]["person"], "ada",
        "{post}"
    );
    assert!(
        post["created"]["at"]
            .as_str()
            .is_some_and(|at| at.ends_with('Z')),
        "{post}"
    );
    assert!(post["created"].get("from").is_none(), "{post}");

    // The reply names its ask; the ask says what it was made from, and who asked.
    let reply = read(
        &mut chain.service,
        "ada",
        &published(&chain.reply),
        json!({}),
    )
    .unwrap();
    assert_eq!(
        reply["created"]["who"]["actor"]["member"]["agent"], "scout",
        "{reply}"
    );
    let ask_id = reply["fields"]["ask"]["item"].as_str().unwrap().to_owned();
    assert_eq!(ask_id, published(&chain.ask));
    let ask = read(&mut chain.service, "ada", &ask_id, json!({})).unwrap();
    assert_eq!(ask["created"]["who"]["actor"]["member"]["person"], "grace");
    assert_eq!(ask["created"]["from"], json!(published(&chain.post)));

    // The same chain the other way: the asks made from the message, the replies to the ask.
    let asks = read(
        &mut chain.service,
        "ada",
        &board,
        json!({"field": "asks", "where": {"from": published(&chain.post)}}),
    )
    .unwrap();
    assert_eq!(items(&asks["fields"]["asks"]), vec![ask_id.clone()]);
    assert_eq!(
        asks["fields"]["asks"]["items"][0]["created"]["from"],
        json!(published(&chain.post))
    );
    let replies = read(
        &mut chain.service,
        "ada",
        &board,
        json!({"field": "replies", "where": {"ask": ask_id}}),
    )
    .unwrap();
    assert_eq!(
        items(&replies["fields"]["replies"]),
        vec![published(&chain.reply)]
    );
    // A transcript orders by when each was made: the whole board's items say it.
    let whole = read(&mut chain.service, "ada", &board, json!({})).unwrap();
    let posts = whole["fields"]["posts"]["items"].as_array().unwrap();
    assert_eq!(posts.len(), 2);
    assert!(posts.iter().all(|post| post["created"]["at"].is_string()));
}

/// Ada's workspace, where Cy views and may read neither the secret post nor the ask made from
/// it, handed to the service as the session hands it the workspace its board is kept in.
fn deny_cy(chain: &Chain) {
    let node = |ty: &str, at: &ResourceRef| {
        TreePath::workspace(WORKSPACE)
            .app(APP)
            .kind(chain.meta.kind_of(ty))
            .resource(at.id_string())
    };
    let mut policy = WorkspacePolicy::new(WORKSPACE, "ada");
    policy.members.insert("cy".to_owned(), Role::Viewer);
    for (id, at) in [
        ("hide-secret", node("Post", &chain.secret)),
        ("hide-secret-ask", node("Ask", &chain.secret_ask)),
    ] {
        policy.grants.push(Grant::new(
            id,
            Grantee::person("cy"),
            at,
            KindSet::ALL,
            Answer::Deny,
            Grantee::person("ada"),
        ));
    }
    let access = MemberAccess::person(policy, "ada");
    let kept = chain.service.workspace_document().unwrap();
    *kept.access.borrow_mut() = Some(Box::new(move || Some(access.clone())));
}

#[test]
fn a_reader_gets_nothing_their_grants_keep_from_them() {
    let mut chain = chain();
    deny_cy(&chain);
    let board = published(&chain.board);
    let (secret, secret_ask) = (published(&chain.secret), published(&chain.secret_ask));

    // Ada reads all of it; Cy reads, through the session's own read, what is not kept from them.
    let ada = read(&mut chain.service, "ada", &board, json!({})).unwrap();
    assert_eq!(items(&ada["fields"]["posts"]).len(), 2);
    let cy = read(&mut chain.service, "cy", &board, json!({})).unwrap();
    assert_eq!(items(&cy["fields"]["posts"]), vec![published(&chain.post)]);
    assert_eq!(items(&cy["fields"]["asks"]), vec![published(&chain.ask)]);

    // Read straight, the secret is not there.
    let refused = read(&mut chain.service, "cy", &secret, json!({}));
    assert!(refused.is_err(), "{refused:?}");

    // Paged one at a time, no page holds it, and none says it is there.
    let mut seen = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let mut args = json!({"field": "posts", "limit": 1});
        if let Some(after) = &after {
            args["after"] = json!(after);
        }
        let page = read(&mut chain.service, "cy", &board, args).unwrap();
        let page = &page["fields"]["posts"];
        seen.extend(items(page));
        match page.get("next") {
            Some(next) => after = Some(next.as_str().unwrap().to_owned()),
            None => break,
        }
    }
    assert_eq!(seen, vec![published(&chain.post)]);
    // Nor does a page start after it.
    let after_secret = read(
        &mut chain.service,
        "cy",
        &board,
        json!({"field": "posts", "after": secret}),
    );
    assert!(after_secret.is_err(), "{after_secret:?}");

    // Matched by what was made from it, or by the ask it holds, it answers as one not there.
    let from = read(
        &mut chain.service,
        "cy",
        &board,
        json!({"field": "asks", "where": {"from": secret}}),
    );
    assert!(from.is_err(), "{from:?}");
    let by_ask = read(
        &mut chain.service,
        "cy",
        &board,
        json!({"field": "replies", "where": {"ask": secret_ask}}),
    );
    assert!(by_ask.is_err(), "{by_ask:?}");
    // Asked as Ada, the same matches answer.
    let by_ask = read(
        &mut chain.service,
        "ada",
        &board,
        json!({"field": "replies", "where": {"ask": secret_ask}}),
    )
    .unwrap();
    assert_eq!(
        items(&by_ask["fields"]["replies"]),
        vec![published(&chain.secret_reply)]
    );

    // A reply Cy reads names the ask it answers, and holds nothing of it.
    let reply = read(
        &mut chain.service,
        "cy",
        &published(&chain.secret_reply),
        json!({}),
    )
    .unwrap();
    let ask = &reply["fields"]["ask"];
    assert_eq!(ask["item"], json!(secret_ask));
    assert!(ask.get("fields").is_none(), "{reply}");
    assert!(!reply.to_string().contains("draft a reply"), "{reply}");
    // Nor does what an item was made from name what the reader may not read.
    let asks = read(
        &mut chain.service,
        "ada",
        &board,
        json!({"field": "asks", "where": {"from": secret}}),
    )
    .unwrap();
    assert_eq!(
        asks["fields"]["asks"]["items"][0]["created"]["from"],
        json!(secret)
    );
}
