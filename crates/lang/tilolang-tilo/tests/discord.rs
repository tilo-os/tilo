//! Setting a Discord server up from the app: the setup's presses reach the bridge's program
//! through the account record, the program answers them through the app's own actions, the bot's
//! token is the vault's item alone, and only the workspace's moderators set a server up.

use serde_json::{Value as Json, json};
use tilolang_runtime::value::{EnumValue, ResourceRef};
use tilolang_runtime::{Actor, InvokeOutcome, MemorySink, Value};

use crate::fixtures::{self, Replica, Workspace};

const DISCORD: &str = include_str!("../../../../apps/tl/discord/app.tl");
const DISCORD_IDS: &str = include_str!("../../../../apps/tl/discord/app.ids");
const CHAT: &str = include_str!("../../../../apps/tl/chat/app.tl");
const CHAT_IDS: &str = include_str!("../../../../apps/tl/chat/app.ids");
const CHAT_APP: &str = "so.tilo.chat";
const APP: &str = "so.tilo.discord.preview";
/// The item the person's token was put in, as the vault addresses it: 32 hexadecimal digits.
const ITEM: &str = "vault.item:0123456789abcdef0123456789abcdef";

/// The bridge as the workspace sees it: the app's own program, which alone answers the setup.
fn bridge() -> Actor {
    Actor::App(tilo_app_id::AppId::new(APP))
}

fn person(name: &str) -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person(name))
}

/// The field `name` of `at`, as the app declares it.
fn read(host: &tilolang_tilo::host::Host, at: &ResourceRef, name: &str) -> Value {
    let package = host.package(APP).expect("the Discord package");
    let declared = package
        .meta
        .types
        .iter()
        .find(|t| t.id == at.ty)
        .expect("the type");
    let field = declared
        .fields
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("the type declares {name}"));
    host.runtime().read(at, &field.id).expect("the field")
}

fn text(host: &tilolang_tilo::host::Host, at: &ResourceRef, name: &str) -> String {
    match read(host, at, name) {
        Value::Text(text) => text,
        other => panic!("{name} is text: {other:?}"),
    }
}

/// The action `name`'s id, as the package publishes it.
fn action(replica: &Replica, name: &str) -> String {
    crate::fixtures::action_id(&replica.host.package(APP).expect("the package").meta, name)
}

/// Run `name` as `who`, answering what it returned or why it was refused.
fn run(
    replica: &Replica,
    who: &Actor,
    name: &str,
    args: Vec<Value>,
) -> Result<InvokeOutcome, String> {
    let action = action(replica, name);
    replica
        .host
        .runtime()
        .invoke_action(who, (action.as_str(), args))
        .map_err(|error| format!("{error:?}"))
}

/// A workspace with Ada its owner and Bo an editor, the app's record started, and that account.
fn workspace() -> (Replica, ResourceRef) {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[(DISCORD, DISCORD_IDS)]);
    ada.host.runtime().set_members(
        vec![
            tilolang_runtime::members::Member::person("ada", tilo_actions::policy::Role::Owner),
            tilolang_runtime::members::Member::person("bo", tilo_actions::policy::Role::Editor),
        ],
        None,
    );
    let _ = ada.view(APP);
    let root = ada.host.root_of(APP).expect("the app's own root");
    ada.commit();
    match run(
        &ada,
        &person("ada"),
        "begin_setup",
        vec![Value::resource(&root)],
    ) {
        Ok(InvokeOutcome::Done(_)) => {}
        other => panic!("the setup starts: {other:?}"),
    }
    let accounts = match read(&ada.host, &root, "accounts") {
        Value::List(items) => items,
        other => panic!("accounts is a collection: {other:?}"),
    };
    assert_eq!(accounts.len(), 1, "one account");
    let account = match accounts.into_iter().next().expect("the account") {
        Value::Ref(reference) => (*reference.target).clone(),
        other => panic!("an account is a record: {other:?}"),
    };
    ada.commit();
    (ada, account)
}

/// A press of `find_servers` asks the bridge's program for the bot's servers and says nothing
/// else happened yet; only the program's answer to that press writes them, and only the bridge
/// answers.
#[test]
fn finding_the_servers_is_asked_for_and_answered_by_the_bridge() {
    let (ada, account) = workspace();
    // The token is the vault's first: nothing finds servers without it.
    let refused = run(
        &ada,
        &person("ada"),
        "find_servers",
        vec![Value::resource(&account)],
    );
    assert!(
        matches!(&refused, Err(why) if why.contains("Add the bot's token first")),
        "{refused:?}"
    );

    // Ada asks for the token: the person types it into their vault, and the program answers
    // where it was kept.
    match run(
        &ada,
        &person("ada"),
        "ask_token",
        vec![Value::resource(&account)],
    ) {
        Ok(InvokeOutcome::Done(_)) => {}
        other => panic!("ada asks for the token: {other:?}"),
    }
    ada.commit();
    let token_press = text(&ada.host, &account, "setup_ask");
    match run(
        &ada,
        &bridge(),
        "keep_token",
        vec![
            Value::resource(&account),
            Value::text(token_press.as_str()),
            Value::text(ITEM),
        ],
    ) {
        Ok(InvokeOutcome::Done(_)) => {}
        other => panic!("the bridge keeps the token's item: {other:?}"),
    }
    // A person never writes the item.
    let refused = run(
        &ada,
        &person("ada"),
        "keep_token",
        vec![
            Value::resource(&account),
            Value::text(token_press.as_str()),
            Value::text(ITEM),
        ],
    );
    assert!(
        matches!(&refused, Err(why) if why.contains("Only the bridge keeps the bot's token")),
        "{refused:?}"
    );
    assert_eq!(
        text(&ada.host, &account, "token_item"),
        ITEM,
        "the item kept"
    );

    // Ada presses: the record names the step and the press, and waits.
    match run(
        &ada,
        &person("ada"),
        "find_servers",
        vec![Value::resource(&account)],
    ) {
        Ok(InvokeOutcome::Done(_)) => {}
        other => panic!("ada finds the servers: {other:?}"),
    }
    ada.commit();
    assert_eq!(text(&ada.host, &account, "setup_wants"), "find");
    let press = text(&ada.host, &account, "setup_ask");
    assert!(!press.is_empty(), "the press has its own id");
    assert!(
        text(&ada.host, &account, "setup_person").contains("ada"),
        "the press names the person who made it"
    );

    // An answer to an older press changes nothing, and the person is never the one who answers.
    let refused = run(
        &ada,
        &bridge(),
        "setup_failed",
        vec![
            Value::resource(&account),
            Value::text("another-press"),
            Value::text("Discord could not be reached."),
        ],
    );
    assert!(
        matches!(&refused, Err(why) if why.contains("no press this setup waits on")),
        "{refused:?}"
    );
    let refused = run(
        &ada,
        &person("ada"),
        "setup_failed",
        vec![
            Value::resource(&account),
            Value::text(press.as_str()),
            Value::text("Discord could not be reached."),
        ],
    );
    assert!(
        matches!(&refused, Err(why) if why.contains("Only the bridge")),
        "{refused:?}"
    );

    // The bridge answers the press that waits: a bot in no server is said, and nothing waits.
    match run(
        &ada,
        &bridge(),
        "found_servers",
        vec![
            Value::resource(&account),
            Value::text(press.as_str()),
            Value::List(vec![]),
        ],
    ) {
        Ok(InvokeOutcome::Done(_)) => {}
        other => panic!("the bridge answers the press: {other:?}"),
    }
    ada.commit();
    assert_eq!(
        text(&ada.host, &account, "setup_problem"),
        "The bot is in no server. Add it to one in Discord first."
    );
    assert_eq!(
        text(&ada.host, &account, "setup_wants"),
        "",
        "nothing waits"
    );
}

/// A member who does not moderate the workspace is refused every step of the setup, with the
/// reason, and nothing of theirs is written; the owner's stands.
#[test]
fn only_the_workspaces_moderators_set_a_server_up() {
    let (ada, account) = workspace();
    for step in [
        "ask_token",
        "find_servers",
        "bridge_server",
        "back_to_servers",
    ] {
        let refused = run(&ada, &person("bo"), step, vec![Value::resource(&account)]);
        assert!(
            matches!(
                &refused,
                Err(why) if why.contains("Only the workspace's moderators")
            ),
            "{step}: {refused:?}"
        );
    }
    refused_bridge_steps(&ada, &account);
    assert_eq!(text(&ada.host, &account, "token_item"), "", "nothing kept");
    assert_eq!(
        text(&ada.host, &account, "setup_wants"),
        "",
        "nothing asked"
    );

    // The owner's press asks, and the program it waits on is the only one that answers it.
    match run(
        &ada,
        &person("ada"),
        "ask_token",
        vec![Value::resource(&account)],
    ) {
        Ok(InvokeOutcome::Done(_)) => {}
        other => panic!("the owner asks for the token: {other:?}"),
    }
    ada.commit();
    assert_eq!(text(&ada.host, &account, "setup_wants"), "token");

    // A token's item that is no item is refused: what the record keeps is the vault's address.
    let press = text(&ada.host, &account, "setup_ask");
    let refused = run(
        &ada,
        &bridge(),
        "keep_token",
        vec![
            Value::resource(&account),
            Value::text(press.as_str()),
            Value::text("the token itself"),
        ],
    );
    assert!(
        matches!(&refused, Err(why) if why.contains("A vault item is named by its address")),
        "{refused:?}"
    );
}

/// Every setup answer is the bridge's alone: a person's call of one is refused with the reason.
fn refused_bridge_steps(ada: &Replica, account: &ResourceRef) {
    let digest = "0".repeat(64);
    let steps: [(&str, Vec<Value>); 5] = [
        ("keep_token", vec![Value::text(ITEM)]),
        ("found_servers", vec![Value::List(vec![])]),
        ("found_channels", vec![Value::List(vec![])]),
        (
            "setup_failed",
            vec![Value::text("Discord could not be reached.")],
        ),
        (
            "start_sign_in",
            vec![
                Value::text("https://discord.com/oauth2/authorize?client_id=1"),
                Value::text(digest.as_str()),
            ],
        ),
    ];
    for (step, args) in steps {
        let mut invited = vec![Value::resource(account), Value::text("press")];
        invited.extend(args);
        let refused = run(ada, &person("ada"), step, invited);
        assert!(
            matches!(&refused, Err(why) if why.contains("Only the bridge")),
            "{step}: {refused:?}"
        );
    }
}

/// The SHA-256 of `text` in lowercase hex, as the bridge and the person's own side make it.
fn digest(text: &str) -> String {
    tilolang::sha256_hex(text.as_bytes())
}

/// The records a collection field holds.
fn records(host: &tilolang_tilo::host::Host, at: &ResourceRef, name: &str) -> Vec<ResourceRef> {
    match read(host, at, name) {
        Value::List(items) => items
            .into_iter()
            .map(|item| match item {
                Value::Ref(reference) => (*reference.target).clone(),
                other => panic!("{name} holds records: {other:?}"),
            })
            .collect(),
        other => panic!("{name} is a collection: {other:?}"),
    }
}

/// The bridge offers `/link`'s account, keeping only the digest of the proof it handed out.
fn offer(ada: &Replica, account: &ResourceRef, proof: &str, user: &str, name: &str) {
    match run(
        ada,
        &bridge(),
        "offer_link",
        vec![
            Value::resource(account),
            Value::text(digest(proof).as_str()),
            Value::text(user),
            Value::text(name),
        ],
    ) {
        Ok(InvokeOutcome::Done(_)) => {}
        other => panic!("the bridge offers the link: {other:?}"),
    }
    ada.commit();
}

/// A `/link` offer opens for the person holding its code, once, until it runs out; the record
/// keeps the code's digest and never the code, and an offer started by one person is no other's
/// to take.
#[test]
fn a_link_offer_opens_once_for_its_holder_until_it_runs_out() {
    let (ada, account) = workspace();
    ada.host.set_clock(|| "2026-11-24T09:00:00Z".to_owned());

    // `/link` answered with a one-time code; the person's side hands over its digest, which is
    // all the bridge keeps.
    let code = "the-one-time-code";
    let proof = digest(code);
    offer(&ada, &account, &proof, "1000", "ada_on_discord");
    let offers = records(&ada.host, &account, "link_offers");
    assert_eq!(offers.len(), 1, "the offer is kept");
    let kept = offers.first().expect("the offer");
    assert_eq!(
        text(&ada.host, kept, "discord_user"),
        "1000",
        "the offer keeps the account it is for"
    );

    // A code nobody offered opens nothing.
    let stray = digest(&digest("another code"));
    let refused = run(
        &ada,
        &person("ada"),
        "complete_link",
        vec![Value::resource(&account), Value::text(stray.as_str())],
    );
    assert!(
        matches!(&refused, Err(why) if why.contains("was already used or has run out")),
        "{refused:?}"
    );

    // The holder links: the Discord account is theirs here, and the code and its proof are not
    // written anywhere.
    match run(
        &ada,
        &person("ada"),
        "complete_link",
        vec![Value::resource(&account), Value::text(proof.as_str())],
    ) {
        Ok(InvokeOutcome::Done(_)) => {}
        other => panic!("ada links: {other:?}"),
    }
    ada.commit();
    let links = records(&ada.host, &account, "links");
    assert_eq!(links.len(), 1, "one account linked");
    let link = links.first().expect("the link");
    assert_eq!(text(&ada.host, link, "discord_user"), "1000");
    assert!(
        text(&ada.host, link, "person").contains("ada"),
        "linked to ada"
    );
    let package = ada.host.package(APP).expect("the package");
    let declared = package
        .meta
        .types
        .iter()
        .find(|t| t.id == link.ty)
        .expect("the link type");
    for field in &declared.fields {
        if let Value::Text(said) = read(&ada.host, link, &field.name) {
            assert_ne!(said, code, "the code is never kept");
            assert_ne!(
                said, proof,
                "nor is its proof: only the digest of the proof"
            );
        }
    }
    assert!(
        records(&ada.host, &account, "link_offers").is_empty(),
        "the offer is used up"
    );

    // The same code again opens nothing.
    let refused = run(
        &ada,
        &person("ada"),
        "complete_link",
        vec![Value::resource(&account), Value::text(proof.as_str())],
    );
    assert!(
        matches!(&refused, Err(why) if why.contains("was already used or has run out")),
        "{refused:?}"
    );

    // An offer is good for ten minutes: a later code has run out.
    let late = digest("a later code");
    offer(&ada, &account, &late, "1001", "bo_on_discord");
    ada.host.set_clock(|| "2026-11-24T09:11:00Z".to_owned());
    let refused = run(
        &ada,
        &person("ada"),
        "complete_link",
        vec![Value::resource(&account), Value::text(late.as_str())],
    );
    assert!(
        matches!(&refused, Err(why) if why.contains("ran out after ten minutes")),
        "{refused:?}"
    );

    // An offer that started in Tilo belongs to whoever started it: another member is refused.
    ada.host.set_clock(|| "2026-11-24T09:20:00Z".to_owned());
    match run(
        &ada,
        &person("ada"),
        "ask_token",
        vec![Value::resource(&account)],
    ) {
        Ok(InvokeOutcome::Done(_)) => {}
        other => panic!("ada asks for the token: {other:?}"),
    }
    ada.commit();
    let press = text(&ada.host, &account, "setup_ask");
    let state = digest("the consent page's state");
    match run(
        &ada,
        &bridge(),
        "start_sign_in",
        vec![
            Value::resource(&account),
            Value::text(press.as_str()),
            Value::text("https://discord.com/oauth2/authorize?client_id=1&state=x"),
            Value::text(state.as_str()),
        ],
    ) {
        Ok(InvokeOutcome::Done(_)) => {}
        other => panic!("the bridge starts the sign-in: {other:?}"),
    }
    let theirs = digest("ada's consent code");
    match run(
        &ada,
        &bridge(),
        "offer_sign_in",
        vec![
            Value::resource(&account),
            Value::text(digest(&theirs).as_str()),
            Value::text("2000"),
            Value::text("ada_on_discord"),
            Value::text(state.as_str()),
        ],
    ) {
        Ok(InvokeOutcome::Done(_)) => {}
        other => panic!("the bridge offers the signed-in account: {other:?}"),
    }
    ada.commit();
    let refused = run(
        &ada,
        &person("bo"),
        "complete_link",
        vec![Value::resource(&account), Value::text(theirs.as_str())],
    );
    assert!(
        matches!(&refused, Err(why) if why.contains("started by another account")),
        "{refused:?}"
    );
}

/// The bridge's setup, the server it connects and what it reports of that server are the
/// workspace's moderators' to change, and the bridge's own program while it carries a change
/// there: a member may read the page but cannot connect a server, point the sign-in anywhere,
/// unlink someone else's account or report a layout.
#[test]
fn the_servers_setup_and_layout_are_the_workspaces_moderators_to_change() {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[(DISCORD, DISCORD_IDS)]);
    ada.host.runtime().set_members(
        vec![
            tilolang_runtime::members::Member::person("ada", tilo_actions::policy::Role::Owner),
            tilolang_runtime::members::Member::person("ben", tilo_actions::policy::Role::Editor),
            tilolang_runtime::members::Member::person("mo", tilo_actions::policy::Role::Moderator),
        ],
        None,
    );
    let accounts = Value::resource(&ada.host.root_of(APP).unwrap());
    let meta = ada.host.package(APP).unwrap().meta;
    let as_who = |who: &Actor, action: &str, args: Vec<Value>| {
        ada.host.runtime().invoke_action(
            who,
            (crate::fixtures::action_id(&meta, action).as_str(), args),
        )
    };
    let refused = |result: &Result<InvokeOutcome, _>, why: &str| {
        assert!(
            !matches!(result, Ok(InvokeOutcome::Done(_))),
            "the call is refused: {result:?}"
        );
        assert!(
            format!("{result:?}").contains(why),
            "with the reason `{why}`: {result:?}"
        );
    };

    let ben = person("ben");
    let setup = as_who(&ben, "begin_setup", vec![accounts.clone()]);
    refused(
        &setup,
        "Only the workspace's moderators connect a Discord server",
    );

    let opened = as_who(&person("ada"), "begin_setup", vec![accounts]);
    let discord = match opened {
        Ok(InvokeOutcome::Done(discord)) => discord,
        other => panic!("the owner connects a Discord account: {other:?}"),
    };
    ada.commit();

    let connected = as_who(
        &person("ada"),
        "connect",
        vec![discord.clone(), Value::text("123"), Value::text("Server")],
    );
    assert!(
        matches!(connected, Ok(InvokeOutcome::Done(_))),
        "the owner connects one server: {connected:?}"
    );
    ada.commit();

    let joined = as_who(
        &ben,
        "connect",
        vec![
            discord.clone(),
            Value::text("456"),
            Value::text("Elsewhere"),
        ],
    );
    refused(
        &joined,
        "Only the workspace's moderators connect a Discord server",
    );

    let sign_in = as_who(
        &ben,
        "set_sign_in",
        vec![
            discord.clone(),
            Value::text("https://discord.com/oauth2/authorize?client_id=1&scope=identify"),
        ],
    );
    refused(
        &sign_in,
        "Only the workspace's moderators set up signing in with Discord",
    );

    let mo = as_who(
        &person("mo"),
        "set_sign_in",
        vec![
            discord.clone(),
            Value::text("https://discord.com/oauth2/authorize?client_id=1&scope=identify"),
        ],
    );
    assert!(
        matches!(mo, Ok(InvokeOutcome::Done(_))),
        "a moderator sets the consent page: {mo:?}"
    );

    let unlink = as_who(&ben, "unlink", vec![discord.clone(), Value::text("3001")]);
    refused(
        &unlink,
        "Only the workspace's moderators, or the person it is with, unlink a Discord account",
    );

    // The bridge itself reports the layout and blocks a persona: it acts as the app, which no
    // member is.
    let bridge = bridge();
    let layout = as_who(
        &bridge,
        "observe_layout",
        vec![
            discord.clone(),
            Value::text("1"),
            Value::List(Vec::new()),
            Value::List(Vec::new()),
            Value::List(Vec::new()),
        ],
    );
    assert!(
        matches!(layout, Ok(InvokeOutcome::Done(_))),
        "the bridge reports the layout: {layout:?}"
    );
    let blocked = as_who(
        &bridge,
        "block_persona",
        vec![discord.clone(), Value::text("3001")],
    );
    assert!(
        matches!(blocked, Ok(InvokeOutcome::Done(_))),
        "the bridge blocks a persona: {blocked:?}"
    );
    let by_member = as_who(
        &ben,
        "observe_layout",
        vec![
            discord,
            Value::text("1"),
            Value::List(Vec::new()),
            Value::List(Vec::new()),
            Value::List(Vec::new()),
        ],
    );
    refused(&by_member, "Only the bridge keeps the server's layout");
}

// ---- the bridge's calls, and the one channel it reaches --------------------------------

/// A value of the case `name` of the enum `declared` of `meta`.
fn case(meta: &tilolang_runtime::meta::Meta, declared: &str, name: &str) -> Value {
    let ty = meta
        .enums
        .iter()
        .find(|ty| ty.name == declared)
        .unwrap_or_else(|| panic!("the enum {declared}"));
    let variant = ty
        .variants
        .iter()
        .find(|variant| variant.name == name)
        .unwrap_or_else(|| panic!("the case {name}"));
    Value::Enum(EnumValue {
        ty: ty.id.clone(),
        variant: variant.id.clone(),
        fields: std::collections::BTreeMap::new(),
    })
}

/// Call `action` directly as `who`, with `args`.
fn as_who(
    r: &Replica,
    who: &Actor,
    action: &str,
    args: Vec<Value>,
) -> Result<InvokeOutcome, tilolang_runtime::RuntimeError> {
    let meta = r.host.package(APP).unwrap().meta;
    r.host
        .runtime()
        .invoke_action(who, (fixtures::action_id(&meta, action).as_str(), args))
}

/// Call an outbound action as `who`, with the call the bridge's program writes as its JSON.
fn carry_as(
    r: &Replica,
    who: &Actor,
    account: &ResourceRef,
    action: &str,
    args: Json,
) -> Result<InvokeOutcome, tilolang_runtime::RuntimeError> {
    as_who(
        r,
        who,
        action,
        vec![Value::resource(account), Value::Json(Box::new(args))],
    )
}

/// Call an outbound action as the bridge's own process, and let what it queued reach Discord.
fn settled(
    r: &Replica,
    account: &ResourceRef,
    action: &str,
    args: Json,
) -> Result<InvokeOutcome, tilolang_runtime::RuntimeError> {
    let outcome = carried_out(r, account, action, args);
    if outcome.is_ok() {
        r.commit();
        // The first settle moves the invocation's effect into the document's outbox; the second
        // hands it to the sink.
        r.host.settle_outbox().expect("the call is carried");
        r.host.settle_outbox().expect("the call is carried");
    }
    outcome
}

/// Let every effect the calls queued reach the sink: a settle moves one invocation's effect on,
/// so settle until the sink stops growing.
fn drain(r: &Replica, _sink: &MemorySink) {
    r.commit();
    for _ in 0..16 {
        r.host.settle_outbox().expect("the calls are carried");
    }
}

/// Call an outbound action as the bridge's own process.
fn carried_out(
    r: &Replica,
    account: &ResourceRef,
    action: &str,
    args: Json,
) -> Result<InvokeOutcome, tilolang_runtime::RuntimeError> {
    carry_as(r, &bridge(), account, action, args)
}

/// A call expected to be refused: one that did not run, with `why` in its reason.
fn was_refused<T: std::fmt::Debug, E: std::fmt::Debug>(result: &Result<T, E>, why: &str) {
    match result {
        Err(error) => assert!(
            format!("{error:?}").contains(why),
            "with the reason `{why}`: {error:?}"
        ),
        Ok(value) => panic!("the call is refused, not done: {value:?}"),
    }
}

fn allowed<T: std::fmt::Debug, E: std::fmt::Debug>(result: &Result<T, E>) {
    if let Err(error) = result {
        panic!("the call is allowed: {error:?}");
    }
}

/// The `discord.perform` calls the sink was handed, each with its op decoded.
fn carried(sink: &MemorySink) -> Vec<Json> {
    sink.delivered()
        .into_iter()
        .filter(|(capability, _)| capability == "discord.perform")
        .filter_map(|(_, payload)| payload.as_str().map(str::to_owned))
        .map(|payload| {
            let perform: Json = serde_json::from_str(&payload).expect("a Discord call");
            serde_json::from_str(perform["op"].as_str().expect("the call's op")).expect("the op")
        })
        .collect()
}

/// The first account of the app's root: the one connected server.
fn account_of(r: &Replica) -> ResourceRef {
    let root = r.host.root_of(APP).unwrap();
    match r.read(&root, "accounts") {
        Value::List(items) => match items.first() {
            Some(Value::Ref(reference)) => (*reference.target).clone(),
            other => panic!("an account is a resource: {other:?}"),
        },
        other => panic!("the accounts are a list: {other:?}"),
    }
}

/// A Discord app in a workspace of an owner, an editor and a moderator, its one server
/// connected (to one channel when `only` names one) with the channel 456 and its webhook 555
/// mapped to a conversation, and a sink taking what it carries to Discord.
fn connected(only: Option<&str>) -> (Replica, MemorySink, ResourceRef) {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[(DISCORD, DISCORD_IDS)]);
    ada.host.runtime().set_members(
        vec![
            tilolang_runtime::members::Member::person("ada", tilo_actions::policy::Role::Owner),
            tilolang_runtime::members::Member::person("ben", tilo_actions::policy::Role::Editor),
            tilolang_runtime::members::Member::person("mo", tilo_actions::policy::Role::Moderator),
        ],
        None,
    );
    let sink = MemorySink::new();
    ada.host.runtime().set_sink(sink.clone());
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    let meta = ada.host.package(APP).unwrap().meta;
    let done = |action: &str, args: Vec<Value>| match ada.host.runtime().invoke_action(
        &person("ada"),
        (fixtures::action_id(&meta, action).as_str(), args),
    ) {
        Ok(InvokeOutcome::Done(value)) => value,
        other => panic!("{action}: {other:?}"),
    };
    done("begin_setup", vec![all]);
    ada.commit();
    let discord = Value::resource(&account_of(&ada));
    done(
        "connect",
        vec![
            discord,
            Value::text("123"),
            Value::text("Server"),
            Value::text(only.unwrap_or("")),
            Value::Bool(false),
            Value::Bool(true),
        ],
    );
    let discord = Value::resource(&account_of(&ada));
    for (kind, tilo, discord_id) in [
        ("channel", "tl:so.tilo.chat:design", "456"),
        ("webhook", "tl:so.tilo.chat:design", "555"),
    ] {
        done(
            "map",
            vec![
                discord.clone(),
                case(&meta, "MappedKind", kind),
                Value::text(tilo),
                Value::text(discord_id),
                case(&meta, "OriginSide", "discord"),
            ],
        );
    }
    ada.commit();
    let account = account_of(&ada);
    (ada, sink, account)
}

/// A bridged conversation is said on Discord through the bridge's own actions: a message goes
/// there with the channel, the webhook and the person's name, an edit, a reaction, a deletion and
/// a thread follow it, and a message over Discord's 2000 characters is refused. Each call is made
/// on its own bridge, since one device hands on the effect of one invocation per settle.
#[test]
fn what_a_bridged_conversation_says_is_carried_to_discord() {
    let carried_op = |action: &str, args: Json| -> Json {
        let (ada, sink, account) = connected(None);
        allowed(&carried_out(&ada, &account, action, args));
        drain(&ada, &sink);
        let ops = carried(&sink);
        assert_eq!(ops.len(), 1, "one call reached Discord: {ops:?}");
        ops.into_iter().next().expect("the call")
    };

    let send = carried_op(
        "send_there",
        json!({
            "op": "send",
            "channel": "456",
            "webhook": "555",
            "username": "ada",
            "content": "hello",
            "attachments": [],
            "tilo": "tl:so.tilo.chat:m1",
        }),
    );
    assert_eq!(send["op"], "send", "{send}");
    assert_eq!(send["channel"], "456", "{send}");
    assert_eq!(send["webhook"], "555", "{send}");
    assert_eq!(send["content"], "hello", "{send}");
    assert_eq!(send["tilo"], "tl:so.tilo.chat:m1", "{send}");
    assert_eq!(send["username"], "ada", "{send}");
    assert!(
        send.get("thread").is_none_or(Json::is_null),
        "a channel's send names no thread: {send}"
    );

    let edit = carried_op(
        "edit_there",
        json!({
            "op": "edit",
            "channel": "456",
            "webhook": "555",
            "message": "999",
            "content": "hello again",
        }),
    );
    assert_eq!(edit["message"], "999", "{edit}");
    assert_eq!(edit["content"], "hello again", "{edit}");
    assert_eq!(edit["webhook"], "555", "{edit}");

    let react = carried_op(
        "react_there",
        json!({
            "op": "react",
            "channel": "456",
            "message": "999",
            "emoji": "👍",
        }),
    );
    assert_eq!(react["message"], "999", "{react}");
    assert_eq!(react["emoji"], "👍", "{react}");

    let delete = carried_op(
        "delete_there",
        json!({
            "op": "delete",
            "channel": "456",
            "webhook": "555",
            "message": "999",
        }),
    );
    assert_eq!(delete["message"], "999", "{delete}");
    assert_eq!(delete["webhook"], "555", "{delete}");

    let thread = carried_op(
        "start_thread_there",
        json!({
            "op": "start-thread",
            "channel": "456",
            "message": "999",
            "name": "topic",
            "tilo": "tl:so.tilo.chat:topic",
        }),
    );
    assert_eq!(thread["message"], "999", "{thread}");
    assert_eq!(thread["channel"], "456", "{thread}");

    // Discord takes 2000 characters: the bridge's program cuts to it, and a call over it is
    // refused before Discord hears of it.
    let (ada, _sink, account) = connected(None);
    was_refused(
        &carried_out(
            &ada,
            &account,
            "send_there",
            json!({
                "op": "send",
                "channel": "456",
                "webhook": "555",
                "username": "ada",
                "content": "word ".repeat(401),
                "attachments": [],
                "tilo": "tl:so.tilo.chat:long",
            }),
        ),
        "A Discord message is 1 to 2000 characters",
    );
}

/// A bridge of one channel reaches that channel, its threads and nothing else: messages in
/// another channel, the server's layout, roles, members and emoji are refused before Discord
/// hears of them.
#[test]
fn a_bridge_of_one_channel_reaches_that_channel_and_nothing_else() {
    let (ada, sink, account) = connected(Some("456"));
    let meta = ada.host.package(APP).unwrap().meta;
    let discord = Value::resource(&account);
    // A thread grown from a message in the bridged channel.
    allowed(&as_who(
        &ada,
        &person("ada"),
        "map",
        vec![
            discord.clone(),
            case(&meta, "MappedKind", "thread"),
            Value::text("tl:so.tilo.chat:topic"),
            Value::text("777"),
            case(&meta, "OriginSide", "discord"),
        ],
    ));
    // A second channel the bridge maps, so a send to it reaches the one-channel rule rather
    // than the channel's own absence.
    allowed(&as_who(
        &ada,
        &person("ada"),
        "map",
        vec![
            discord,
            case(&meta, "MappedKind", "channel"),
            Value::text("tl:so.tilo.chat:elsewhere"),
            Value::text("999"),
            case(&meta, "OriginSide", "discord"),
        ],
    ));
    ada.commit();

    let send = |channel: &str| {
        json!({
            "op": "send",
            "channel": channel,
            "webhook": "555",
            "username": "ada",
            "content": "hello",
            "attachments": [],
            "tilo": "tl:so.tilo.chat:made",
        })
    };
    allowed(&settled(&ada, &account, "send_there", send("456")));
    was_refused(
        &carried_out(&ada, &account, "send_there", send("999")),
        "This bridge reaches only its one Discord channel",
    );
    allowed(&settled(
        &ada,
        &account,
        "start_thread_there",
        json!({
            "op": "start-thread",
            "channel": "456",
            "message": "9001",
            "name": "topic",
            "tilo": "tl:so.tilo.chat:topic",
        }),
    ));

    was_refused(
        &carried_out(
            &ada,
            &account,
            "apply_role",
            json!({
                "op": "apply-role",
                "name": "Bridge",
                "tilo": "bridge",
            }),
        ),
        "A bridge of one channel changes nothing else in the server",
    );
    was_refused(
        &carried_out(
            &ada,
            &account,
            "kick",
            json!({"op": "kick", "user": "3001"}),
        ),
        "A bridge of one channel changes nothing else in the server",
    );
    was_refused(
        &carried_out(
            &ada,
            &account,
            "apply_emoji",
            json!({
                "op": "apply-emoji",
                "name": "party",
                "tilo": "party",
            }),
        ),
        "A bridge of one channel changes nothing else in the server",
    );
    was_refused(
        &as_who(
            &ada,
            &bridge(),
            "delete_channel_there",
            vec![Value::resource(&account), Value::text("456")],
        ),
        "A bridge of one channel changes nothing else in the server",
    );

    ada.commit();
    ada.host.settle_outbox().expect("the last call is carried");
    let ops = carried(&sink);
    assert_eq!(
        ops.iter().filter(|op| op["op"] == "send").count(),
        1,
        "only the send in its one channel reached Discord: {ops:?}"
    );
}

/// Run `name` of `app` as `who`, with `args`.
fn run_in(
    replica: &Replica,
    app: &str,
    who: &Actor,
    name: &str,
    args: Vec<Value>,
) -> Result<InvokeOutcome, String> {
    let meta = replica.host.package(app).expect("the package").meta;
    let action = fixtures::action_id(&meta, name);
    replica
        .host
        .runtime()
        .invoke_action(who, (action.as_str(), args))
        .map_err(|error| format!("{error:?}"))
}

/// What `name` of `app` answered, run as `who`, or a panic saying what it refused.
fn did(replica: &Replica, app: &str, who: &Actor, name: &str, args: Vec<Value>) -> Value {
    match run_in(replica, app, who, name, args) {
        Ok(InvokeOutcome::Done(value)) => value,
        other => panic!("{name}: {other:?}"),
    }
}

/// The field `name` of `at` of `app`, as `app` declares it.
fn read_in(host: &tilolang_tilo::host::Host, app: &str, at: &ResourceRef, name: &str) -> Value {
    let package = host.package(app).expect("the package");
    let declared = package
        .meta
        .types
        .iter()
        .find(|t| t.id == at.ty)
        .expect("the type");
    let field = declared
        .fields
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("the type declares {name}"));
    host.runtime().read(at, &field.id).expect("the field")
}

/// The record a message's `outside` field holds, its fields by name.
fn outside_author(
    host: &tilolang_tilo::host::Host,
    app: &str,
    at: &ResourceRef,
) -> Option<std::collections::BTreeMap<String, Value>> {
    let outside = match read_in(host, app, at, "outside") {
        Value::Opt(Some(inner)) => *inner,
        Value::Opt(None) => return None,
        other => panic!("a message's outside author: {other:?}"),
    };
    let Value::Rec(author) = outside else {
        panic!("an outside author is a record: {outside:?}")
    };
    let package = host.package(app).expect("the package");
    let declared = package
        .meta
        .types
        .iter()
        .find(|t| t.id == author.ty)
        .expect("the outside author's type");
    Some(
        declared
            .fields
            .iter()
            .filter_map(|field| {
                author
                    .fields
                    .get(&field.id)
                    .map(|value| (field.name.clone(), value.clone()))
            })
            .collect(),
    )
}

/// A Discord account a member linked speaks here as that member: the message wears their face and
/// colour beside the account's name. One nobody linked carries the name Discord shows alone.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one linked account and one nobody linked, each relayed and read back"
)]
fn a_linked_discord_account_speaks_as_its_member_and_an_unlinked_one_as_itself() {
    let (ada, _) = Replica::new(
        "ada",
        Workspace::new(),
        &[(CHAT, CHAT_IDS), (DISCORD, DISCORD_IDS)],
    );
    ada.host.runtime().set_members(
        vec![
            tilolang_runtime::members::Member::person("ada", tilo_actions::policy::Role::Owner),
            tilolang_runtime::members::Member::person("bo", tilo_actions::policy::Role::Editor),
        ],
        None,
    );
    let _ = ada.root(CHAT_APP);
    ada.commit();
    let design = did(
        &ada,
        CHAT_APP,
        &person("ada"),
        "new_channel",
        vec![Value::resource(&ada.root(CHAT_APP)), Value::text("design")],
    );
    let all = Value::resource(&ada.root(APP));
    did(&ada, APP, &person("ada"), "begin_setup", vec![all]);
    ada.commit();
    let account = account_of(&ada);
    did(
        &ada,
        APP,
        &person("ada"),
        "connect",
        vec![
            Value::resource(&account),
            Value::text("123"),
            Value::text("Server"),
            Value::text(""),
            Value::Bool(false),
            Value::Bool(true),
        ],
    );
    ada.commit();
    // The bridge's `/link` offer, taken by ada: the record joins Discord's account to her.
    offer(&ada, &account, "proof", "3001", "the account");
    did(
        &ada,
        APP,
        &person("ada"),
        "complete_link",
        vec![Value::resource(&account), Value::text("proof")],
    );
    ada.commit();
    let linked = records(&ada.host, &account, "links");
    let link = linked.first().expect("the account is linked");
    assert_eq!(
        text(&ada.host, link, "person"),
        serde_json::to_string(&person("ada")).expect("an actor's JSON"),
        "the link's person"
    );
    assert_eq!(
        text(&ada.host, link, "discord_user"),
        "3001",
        "the account linked"
    );

    let body = tilolang_runtime::rich::RichValue::from_text("hello");
    let relay = |member: Value, name: &str, avatar: &str| {
        did(
            &ada,
            CHAT_APP,
            &bridge(),
            "relay",
            vec![
                design.clone(),
                Value::Rich(body.clone()),
                Value::text(name),
                Value::text("Discord"),
                Value::Int(0),
                Value::List(Vec::new()),
                Value::Opt(None),
                Value::text(avatar),
                Value::text("Discord"),
                Value::List(Vec::new()),
                member,
            ],
        )
    };
    let avatar = "https://cdn.discordapp.com/avatars/3001/abc.png?size=128";
    let hers = fixtures::target(&relay(
        Value::Opt(Some(Box::new(Value::Actor(person("ada"))))),
        "the account",
        avatar,
    ));
    let theirs = fixtures::target(&relay(Value::Opt(None), "zoe", ""));
    ada.commit();

    let her_author = outside_author(&ada.host, CHAT_APP, &hers).expect("a message from Discord");
    assert_eq!(her_author.get("name"), Some(&Value::text("the account")));
    assert_eq!(
        her_author.get("service"),
        Some(&Value::text("Discord")),
        "the place she wrote from"
    );
    match her_author.get("member") {
        Some(Value::Opt(Some(inner))) => {
            assert_eq!(**inner, Value::Actor(person("ada")), "her linked account");
        }
        other => panic!("the linked member: {other:?}"),
    }

    let their_author =
        outside_author(&ada.host, CHAT_APP, &theirs).expect("a message from Discord");
    assert_eq!(their_author.get("name"), Some(&Value::text("zoe")));
    assert!(
        matches!(their_author.get("member"), Some(Value::Opt(None)) | None),
        "nobody linked that account: {their_author:?}"
    );

    // A member of the workspace who linked no account is refused when named as one.
    was_refused(
        &run_in(
            &ada,
            CHAT_APP,
            &bridge(),
            "relay",
            vec![
                design,
                Value::Rich(body),
                Value::text("eve"),
                Value::text("Discord"),
                Value::Int(0),
                Value::List(Vec::new()),
                Value::Opt(None),
                Value::text(""),
                Value::text("Discord"),
                Value::List(Vec::new()),
                Value::Opt(Some(Box::new(Value::Actor(person("eve"))))),
            ],
        ),
        "The person a relayed message is linked to is not in this workspace",
    );
}

/// What the bridge hands the worker is the shape it reads: an absent thread, avatar and webhook
/// are none rather than empty text, a file is its bare content hash, a permission overwrite says
/// whether it is a role's or a member's, a fetched picture may come back inline, and the time a
/// message was written here goes with it.
#[test]
fn what_the_bridge_carries_to_discord_is_the_shape_the_worker_reads() {
    let (ada, sink, account) = connected(None);
    let hash = "b".repeat(64);

    allowed(&settled(
        &ada,
        &account,
        "fetch_asset",
        json!({
            "op": "fetch-asset",
            "url": "https://cdn.discordapp.com/avatars/3001/abc.png",
            "max_bytes": 1_048_576,
            "inline": true,
        }),
    ));
    allowed(&settled(
        &ada,
        &account,
        "send_there",
        json!({
            "op": "send",
            "channel": "456",
            "webhook": "555",
            "username": "Ada",
            "content": "look",
            "attachments": [{"blob": hash, "name": "shot.png", "size": 12}],
            "created_ms": 1_700_000_000_123_i64,
            "tilo": "tl:so.tilo.chat:made",
        }),
    ));
    allowed(&settled(
        &ada,
        &account,
        "edit_there",
        json!({
            "op": "edit",
            "channel": "456",
            "webhook": "555",
            "message": "9001",
            "content": "look again",
        }),
    ));
    allowed(&settled(
        &ada,
        &account,
        "delete_there",
        json!({
            "op": "delete",
            "channel": "456",
            "message": "9001",
        }),
    ));
    let meta = ada.host.package(APP).unwrap().meta;
    allowed(&as_who(
        &ada,
        &person("ada"),
        "map",
        vec![
            Value::resource(&account),
            case(&meta, "MappedKind", "role"),
            Value::text("bridge"),
            Value::text("700"),
            case(&meta, "OriginSide", "tilo"),
        ],
    ));
    allowed(&settled(
        &ada,
        &account,
        "set_overwrite",
        json!({
            "op": "set-overwrite",
            "channel": "456",
            "target": "700",
            "target_kind": "role",
        }),
    ));
    ada.commit();
    ada.host.settle_outbox().expect("the last call is carried");
    let ops = carried(&sink);
    let send = ops.iter().find(|op| op["op"] == "send").expect("a send");
    assert!(send["thread"].is_null(), "an absent thread is none: {send}");
    assert!(
        send["avatar_url"].is_null() && send["avatar_blob"].is_null(),
        "an absent avatar is none: {send}"
    );
    assert_eq!(send["created_ms"], 1_700_000_000_123_i64, "{send}");
    assert_eq!(
        send["attachments"][0]["blob"], hash,
        "a bare content hash: {send}"
    );
    assert_eq!(send["attachments"][0]["name"], "shot.png", "{send}");

    let edit = ops.iter().find(|op| op["op"] == "edit").expect("an edit");
    assert!(edit["thread"].is_null(), "an absent thread is none: {edit}");
    let delete = ops
        .iter()
        .find(|op| op["op"] == "delete")
        .expect("a delete");
    assert!(
        delete["webhook"].is_null(),
        "an absent webhook is none: {delete}"
    );
    let overwrite = ops
        .iter()
        .find(|op| op["op"] == "set-overwrite")
        .expect("an overwrite");
    assert_eq!(overwrite["target_kind"], "role", "{overwrite}");
    let asset = ops
        .iter()
        .find(|op| op["op"] == "fetch-asset")
        .unwrap_or_else(|| panic!("a fetched asset: {ops:?} all: {:?}", sink.delivered()));
    assert_eq!(asset["inline"], true, "{asset}");
}
