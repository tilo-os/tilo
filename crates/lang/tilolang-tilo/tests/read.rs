//! What a reader outside tilolang reads of a resource through the API's `read`, as a window
//! answers it: who made each item, when and from what; a page of a collection matched by its
//! items' fields or by what they were made from, read from the store however large the
//! collection; and never more than the reader's grants let them read, whichever way they ask.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use serde_json::{Map, Value as Json, json};
use tilo_action_policy::{Answer, Grant, Grantee, MemberAccess, Role, TreePath, WorkspacePolicy};
use tilo_action_types::KindSet;
use tilolang_runtime::origin::OriginWrite;
use tilolang_runtime::value::{ActionRef, ResourceRef};
use tilolang_runtime::{Actor, InvokeOutcome, Meta, Value};
use tilolang_tilo::host::Host;
use tilolang_tilo::meta::MetaExt as _;
use tilolang_tilo::naming::resource_id;
use tilolang_tilo::runtime::ids::MemberId;

use crate::fixtures::{Replica, Workspace, action_id, field_id, type_id};

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

/// `read` of `resource` as `who`, through the `read` a window answers: the resource named in the
/// arguments, as the API asks it.
fn read(host: &Host, who: &str, resource: &str, args: Json) -> Result<Json, String> {
    let mut asked: Map<String, Json> = args.as_object().cloned().unwrap_or_default();
    asked.insert("item".to_owned(), Json::String(resource.to_owned()));
    host.answer_read(&Json::Object(asked), &Actor::from(MemberId::person(who)))
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

/// Every item of `field` of `resource` in the collection's order, as `who` reads them.
fn every_item(host: &Host, who: &str, resource: &str, field: &str) -> Vec<Json> {
    let mut out = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let mut args = json!({"field": field, "limit": 200});
        if let Some(after) = &after {
            args["after"] = json!(after);
        }
        let page = read(host, who, resource, args).unwrap();
        let page = &page["fields"][field];
        out.extend(page["items"].as_array().unwrap().clone());
        match page.get("next") {
            Some(next) => after = Some(next.as_str().unwrap().to_owned()),
            None => break,
        }
    }
    out
}

/// Ada's board holding `count` posts, every hundredth pinned, written as an origin brings work in.
fn seeded(count: usize) -> (Replica, Meta, ResourceRef) {
    let (ada, packages) = Replica::new("ada", Workspace::new(), &[(SOURCE, "")]);
    let meta = packages[0].meta.clone();
    let board = ada.host.root_of(APP).unwrap();
    ada.commit();
    let post = type_id(&meta, "Post");
    let text = field_id(&meta, "Post", "text");
    let pinned = field_id(&meta, "Post", "pinned");
    let posts = field_id(&meta, "Board", "posts");
    let mut written = 0;
    while written < count {
        let end = count.min(written.saturating_add(1000));
        let writes: Vec<OriginWrite> = (written..end)
            .map(|index| OriginWrite::Insert {
                owner: board.clone(),
                field: posts.clone(),
                ty: post.clone(),
                fields: vec![
                    (text.clone(), Value::Text(format!("post {index:05}"))),
                    (pinned.clone(), Value::Bool(index % 100 == 7)),
                ],
            })
            .collect();
        ada.host
            .runtime()
            .write_origin_for("seed", Some(ada.host.viewer().clone()), None, |change| {
                change.write(writes)
            })
            .unwrap();
        written = end;
    }
    (ada, meta, board)
}

#[test]
fn a_page_of_ten_thousand_reads_the_page_it_asks_for() {
    let (ada, _meta, board) = seeded(10_000);
    let board = published(&board);
    // The collection's order, and which of its items are pinned.
    let all = every_item(&ada.host, "ada", &board, "posts");
    assert_eq!(all.len(), 10_000);
    let ordered: Vec<String> = all
        .iter()
        .map(|row| row["item"].as_str().unwrap().to_owned())
        .collect();
    let pinned: Vec<String> = (0..10_000)
        .filter(|index| index % 100 == 7)
        .map(|index| ordered[index].clone())
        .collect();
    assert_eq!(pinned.len(), 100);
    assert_ne!(pinned[..30], ordered[..30], "the page is not the first 30");

    let started = Instant::now();
    let first = read(
        &ada.host,
        "ada",
        &board,
        json!({"field": "posts", "where": {"pinned": true}, "limit": 30}),
    )
    .unwrap();
    let page = &first["fields"]["posts"];
    let found = items(page);
    assert_eq!(found, pinned[..30]);
    let next = page["next"].as_str().expect("more match than a page");
    assert_eq!(next, pinned[29]);
    let second = read(
        &ada.host,
        "ada",
        &board,
        json!({"field": "posts", "where": {"pinned": true}, "after": next, "limit": 30}),
    )
    .unwrap();
    let rest = items(&second["fields"]["posts"]);
    let elapsed = started.elapsed();
    assert_eq!(rest, pinned[30..60]);
    // A page without a match reads in the collection's order from where it starts.
    let plain = read(
        &ada.host,
        "ada",
        &board,
        json!({"field": "posts", "after": ordered[9998]}),
    )
    .unwrap();
    assert_eq!(
        items(&plain["fields"]["posts"]),
        vec![ordered[9999].clone()]
    );
    assert!(plain["fields"]["posts"].get("next").is_none());
    eprintln!("two pages of a 10k collection, matched: {elapsed:?}");
}

/// Ada's board with a post of hers, an ask Grace made from it, and the agent Scout's reply to
/// the ask; and a secret post with its own ask and reply.
struct Chain {
    ada: Replica,
    /// The access the host reads when it decides who reads what: empty until [`deny_cy`] sets it.
    access: Rc<RefCell<Option<MemberAccess>>>,
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
    // Ada's host decides by the person's access in the workspace, which a test may set once the
    // resources whose grants it names exist.
    let access: Rc<RefCell<Option<MemberAccess>>> = Rc::new(RefCell::new(None));
    let held = Rc::clone(&access);
    let (ada, packages) = Replica::following("ada", Workspace::new(), &[(SOURCE, "")], move || {
        held.borrow().clone()
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
    let ask = made(&call(
        &ada,
        "grace",
        on_board("ask", vec![text("summarise")]).made_from(Some(&post)),
    ));
    let scout = Actor::from(MemberId::agent("scout", "cy"));
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
    let secret = made(&call(
        &ada,
        "ada",
        on_board("post", vec![text("The offer is 40k")]),
    ));
    let secret_ask = made(&call(
        &ada,
        "ada",
        on_board("ask", vec![text("draft a reply")]).made_from(Some(&secret)),
    ));
    let secret_reply = made(&call(
        &ada,
        "ada",
        on_board(
            "reply",
            vec![Value::resource(&secret_ask), text("Counter at 45k")],
        ),
    ));
    Chain {
        ada,
        access,
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
    let chain = chain();
    let board = published(&chain.board);

    // A message: who wrote it, and when.
    let post = read(&chain.ada.host, "ada", &published(&chain.post), json!({})).unwrap();
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
    let reply = read(&chain.ada.host, "ada", &published(&chain.reply), json!({})).unwrap();
    assert_eq!(
        reply["created"]["who"]["actor"]["member"]["agent"]["id"], "scout",
        "{reply}"
    );
    assert_eq!(
        reply["created"]["who"]["actor"]["member"]["agent"]["person"], "cy",
        "the agent is named as its id and the person it acts for: {reply}"
    );
    let ask_id = reply["fields"]["ask"]["item"].as_str().unwrap().to_owned();
    assert_eq!(ask_id, published(&chain.ask));
    let ask = read(&chain.ada.host, "ada", &ask_id, json!({})).unwrap();
    assert_eq!(ask["created"]["who"]["actor"]["member"]["person"], "grace");
    assert_eq!(ask["created"]["from"], json!(published(&chain.post)));

    // The same chain the other way: the asks made from the message, the replies to the ask.
    let asks = read(
        &chain.ada.host,
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
        &chain.ada.host,
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
    let whole = read(&chain.ada.host, "ada", &board, json!({})).unwrap();
    let posts = whole["fields"]["posts"]["items"].as_array().unwrap();
    assert_eq!(posts.len(), 2);
    assert!(posts.iter().all(|post| post["created"]["at"].is_string()));
}

/// Ada's workspace, where Cy views and may read neither the secret post nor the ask made from
/// it, handed to the host as the session hands it the workspace its board is kept in.
fn deny_cy(chain: &Chain) {
    let node = |ty: &str, at: &ResourceRef| {
        TreePath::workspace(WORKSPACE)
            .app(APP)
            .kind(chain.meta.kind_of(ty))
            .resource(at.id_string())
    };
    let mut policy = WorkspacePolicy::new(WORKSPACE, "ada");
    policy.members.insert("cy".to_owned(), Role::Viewer.into());
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
    crate::fixtures::stage_people(&chain.ada.host, &policy);
    *chain.access.borrow_mut() = Some(MemberAccess::person(policy, "ada"));
}

#[test]
fn a_reader_gets_nothing_their_grants_keep_from_them() {
    let chain = chain();
    deny_cy(&chain);
    let board = published(&chain.board);
    let (secret, secret_ask) = (published(&chain.secret), published(&chain.secret_ask));

    // Ada reads all of it; Cy reads, through the host's own read, what is not kept from them.
    let ada = read(&chain.ada.host, "ada", &board, json!({})).unwrap();
    assert_eq!(items(&ada["fields"]["posts"]).len(), 2);
    let cy = read(&chain.ada.host, "cy", &board, json!({})).unwrap();
    assert_eq!(items(&cy["fields"]["posts"]), vec![published(&chain.post)]);
    assert_eq!(items(&cy["fields"]["asks"]), vec![published(&chain.ask)]);

    // Read straight, the secret is not there.
    let refused = read(&chain.ada.host, "cy", &secret, json!({}));
    assert!(refused.is_err(), "{refused:?}");

    // Paged one at a time, no page holds it, and none says it is there.
    let mut seen = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let mut args = json!({"field": "posts", "limit": 1});
        if let Some(after) = &after {
            args["after"] = json!(after);
        }
        let page = read(&chain.ada.host, "cy", &board, args).unwrap();
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
        &chain.ada.host,
        "cy",
        &board,
        json!({"field": "posts", "after": secret}),
    );
    assert!(after_secret.is_err(), "{after_secret:?}");

    // Matched by what was made from it, or by the ask it holds, it answers as one not there.
    let from = read(
        &chain.ada.host,
        "cy",
        &board,
        json!({"field": "asks", "where": {"from": secret}}),
    );
    assert!(from.is_err(), "{from:?}");
    let by_ask = read(
        &chain.ada.host,
        "cy",
        &board,
        json!({"field": "replies", "where": {"ask": secret_ask}}),
    );
    assert!(by_ask.is_err(), "{by_ask:?}");
    // Asked as Ada, the same matches answer.
    let by_ask = read(
        &chain.ada.host,
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
        &chain.ada.host,
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
        &chain.ada.host,
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
