//! Setting a Discord server up from the app: the setup's presses reach the bridge's program
//! through the account record, the program answers them through the app's own actions, the bot's
//! token is the vault's item alone, and only the workspace's moderators set a server up.

use crate::fixtures::{Replica, Workspace};
use tilolang_runtime::value::ResourceRef;
use tilolang_runtime::{Actor, InvokeOutcome, Value};

const DISCORD: &str = include_str!("../../../../apps/tl/discord/app.tl");
const DISCORD_IDS: &str = include_str!("../../../../apps/tl/discord/app.ids");
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
