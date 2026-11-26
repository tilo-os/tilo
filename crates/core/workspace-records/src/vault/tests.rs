#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use super::*;
use tilo_actions::ParamType;
use tilo_actions::remote::takes_item;

const ID: &str = "0123456789abcdef0123456789abcdef";

/// `arguments` as the object a call carries.
fn object(arguments: Value) -> Map<String, Value> {
    match arguments {
        Value::Object(object) => object,
        other => panic!("{other} is no object"),
    }
}

/// The file a crate that does not depend on this one binds its calls of the vault to is what
/// this crate declares. `UPDATE_SNAPSHOTS=1` writes it.
#[test]
fn the_vault_surface_file_is_what_this_crate_publishes() {
    let text = tilo_surface_compile::canonical(&serde_json::json!({ "resources": [surface()] }));
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("vault-surface.json");
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|value| value == "1") {
        std::fs::write(&path, &text).expect("the surface file is written");
        return;
    }
    assert_eq!(
        std::fs::read_to_string(&path).ok().as_deref(),
        Some(text.as_str()),
        "crates/core/workspace-records/vault-surface.json is not what this crate \
         publishes; regenerate it with UPDATE_SNAPSHOTS=1"
    );
}

/// What the action registry knows of the vault without this crate names it as it is declared
/// here: the provider it answers under, the actions it says write in the vault's record, the
/// ones it says answer a secret, and the release it asks for a sign-in.
#[test]
fn the_registry_names_the_declared_actions() {
    use tilo_actions::remote::{
        VAULT_DEVICE_KEY, VAULT_FIND, VAULT_FORGET_PAGE_LOGIN, VAULT_ITEM_KIND, VAULT_KEEP,
        VAULT_KEEP_PAGE_LOGIN, VAULT_LIST, VAULT_PAGE_FILL, VAULT_PAGE_LOGINS, VAULT_PROVIDER,
        VAULT_RELEASE, VAULT_RESOURCE,
    };
    assert_eq!(
        (PROVIDER, RESOURCE, ITEM_KIND),
        (VAULT_PROVIDER, VAULT_RESOURCE, VAULT_ITEM_KIND)
    );
    for (named, declared) in [
        (VAULT_RELEASE, RELEASE),
        (VAULT_LIST, LIST),
        (VAULT_FIND, FIND),
        (VAULT_KEEP, KEEP),
        (VAULT_DEVICE_KEY, DEVICE_KEY),
        (VAULT_PAGE_LOGINS, PAGE_LOGINS),
        (VAULT_KEEP_PAGE_LOGIN, KEEP_PAGE_LOGIN),
        (VAULT_FORGET_PAGE_LOGIN, FORGET_PAGE_LOGIN),
        (VAULT_PAGE_FILL, PAGE_FILL),
    ] {
        assert_eq!(named, declared);
        assert!(declaration(named).is_some(), "{named} is declared nowhere");
    }
    for (action, _) in VAULT_STEPS {
        assert!(
            declaration(action).is_some(),
            "{action} is declared nowhere"
        );
    }
    for action in VAULT_SECRET_ANSWERS {
        assert!(
            declaration(action).is_some(),
            "{action} is declared nowhere"
        );
    }
    // A secret typed in is declared one, so no record of a call keeps it.
    for (action, secret) in [
        (ADD, "secret"),
        (ADD, "notes"),
        (ADD, "fields"),
        (KEEP, "value"),
        (KEEP_PAGE_LOGIN, "secret"),
    ] {
        let params = params_of(action).unwrap();
        let param = params.iter().find(|param| param.name == secret).unwrap();
        assert_eq!(param.ty, ParamType::Secret, "{action} {secret}");
    }
    // An item is named by reference wherever one is taken.
    for (action, item) in [
        (REMOVE, "item"),
        (USES, "item"),
        (TAKE_BACK, "item"),
        (COPY, "item"),
        (REVEAL, "item"),
        (FILL_PAGE, "item"),
        (WEB_CALL, "key"),
    ] {
        let params = params_of(action).unwrap();
        let param = params.iter().find(|param| param.name == item).unwrap();
        assert!(takes_item(param), "{action} {item}");
    }
}

/// A web call takes its key by reference and never as text, says how the reference is come
/// by, and is listed apart from the uses an "Always" may stand for.
#[test]
fn a_web_call_takes_its_key_by_reference_and_is_listed_apart() {
    let rows = call_rows();
    assert_eq!(rows.len(), 1);
    let target = target(WEB_CALL);
    assert_eq!(use_word(&target), "tilo.vault/web-call");
    let action = rows[0].action.clone().expect("the row is an action");
    assert_eq!(rows[0].provider, PROVIDER);
    assert_eq!(action.address, target.address);
    assert_eq!(action.contract.kind, tilo_actions::ActionKind::Outside);
    assert_eq!(
        action
            .params
            .iter()
            .filter(|param| takes_item(param))
            .count(),
        1
    );
    assert!(
        action
            .params
            .iter()
            .all(|param| param.ty != ParamType::Secret),
        "no parameter takes a secret's text"
    );
    let described = action.description.unwrap_or_default();
    for said in [
        "vault's `ask`",
        "sentIn",
        "`reference`",
        "asked first",
        "struck",
    ] {
        assert!(described.contains(said), "{said}: {described}");
    }
    assert_eq!(
        described_use(&target).map(|found| found.address),
        Some(target.address.clone())
    );
    assert!(use_of(&target).is_none(), "no Always stands for a call");
    assert!(
        use_rows()
            .iter()
            .all(|row| described_use(&RemoteActionTarget {
                provider: row.provider.clone(),
                address: row.action.as_ref().unwrap().address.clone(),
            })
            .is_some()),
        "the uses are described as before"
    );
    assert!(described_use(&super::target(LIST)).is_none());
}

/// A call's arguments read as declared, and nothing unknown rides along.
#[test]
fn a_calls_arguments_are_typed() {
    let reference = format!("vault.item:{}", "0".repeat(32));
    let call = |arguments: Value| match decoded(WEB_CALL, &object(arguments)) {
        Ok(VaultAction::WebCall { call }) => Ok(call),
        Ok(other) => panic!("{other:?} is no web call"),
        Err(why) => Err(why),
    };
    let plain = call(serde_json::json!({
        "url": "https://api.example.test/v1/balance",
        "key": reference,
    }))
    .unwrap();
    assert!(plain.method.is_empty() && plain.headers.is_empty() && plain.body.is_empty());
    let whole = call(serde_json::json!({
        "url": "https://api.example.test/v1/notes",
        "method": "POST",
        "headers": {"Content-Type": "application/json"},
        "body": "{}",
        "key": reference,
        "reason": "To file the note.",
    }))
    .unwrap();
    assert_eq!(whole.headers["Content-Type"], "application/json");
    let parts = call(serde_json::json!({
        "url": "https://api.example.test/v1/upload",
        "method": "POST",
        "bodyParts": [
            {"name": "payload_json", "json": {"content": "hi"}},
            {"name": "files[0]", "content": {
                "app": "so.tilo.chat",
                "hash": "ab".repeat(32),
                "filename": "a.png",
                "contentType": "image/png",
            }},
            {"name": "note", "text": "one line"},
        ],
        "key": reference,
    }))
    .unwrap();
    assert_eq!(parts.body_parts.len(), 3);
    assert_eq!(
        parts.body_parts[0].json,
        Some(serde_json::json!({"content": "hi"}))
    );
    let content = parts.body_parts[1].content.clone().unwrap();
    assert_eq!(content.app, "so.tilo.chat");
    assert_eq!(content.filename, "a.png");
    assert_eq!(content.content_type, "image/png");
    let raw = call(serde_json::json!({
        "url": "https://api.example.test/v1/upload",
        "method": "PUT",
        "bodyContent": {"hash": "ab".repeat(32), "contentType": "image/png"},
        "key": reference,
    }))
    .unwrap();
    assert_eq!(raw.body_content.unwrap().hash, "ab".repeat(32));
    for wrong in [
        serde_json::json!({"url": "https://api.example.test", "key": "a key typed out"}),
        serde_json::json!({"url": "https://api.example.test", "key": reference, "secret": "x"}),
        serde_json::json!({"key": reference}),
        serde_json::json!({"url": "https://api.example.test", "key": reference,
            "bodyParts": [{"name": "x", "text": "t", "other": 1}]}),
    ] {
        assert!(call(wrong).is_err());
    }
    let adding = decoded(
        ADD,
        &object(serde_json::json!({
            "kind": "key",
            "belongsTo": ["https://api.example.test"],
            "sentIn": "header:X-Api-Key",
            "secret": "made-up-key-0123456789",
        })),
    )
    .unwrap();
    let VaultAction::Add { sent_in, .. } = &adding else {
        panic!("an add reads as one");
    };
    assert_eq!(sent_in.as_deref(), Some("header:X-Api-Key"));
    assert!(!format!("{adding:?}").contains("made-up-key"));
}

/// An ask names only places a secret can belong to, and the person reads the system's words
/// for it, which say where it goes and who never sees it.
#[test]
fn an_ask_for_a_secret_is_checked_and_worded_by_the_system() {
    let asked = |arguments: Value| match decoded(ASK, &object(arguments)) {
        Ok(VaultAction::Ask { asked }) => Ok(asked),
        Ok(other) => panic!("{other:?} is no ask"),
        Err(why) => Err(why),
    };
    let ask = asked(serde_json::json!({
        "kind": "login",
        "belongsTo": [" https://example.com "],
        "username": "ada",
    }))
    .unwrap();
    let ask = ask.checked().unwrap();
    assert_eq!(
        ask.words("Scout"),
        "Scout asks for the password of ada at https://example.com. Scout never sees what \
         you type: your vault keeps it for that place alone."
    );
    for wrong in [
        serde_json::json!({"kind": "login", "belongsTo": []}),
        serde_json::json!({"kind": "login", "belongsTo": ["my bank"]}),
        serde_json::json!({"kind": "login", "belongsTo": ["https://a.example"], "sentIn": "basic"}),
    ] {
        assert!(asked(wrong).unwrap().checked().is_err());
    }
    assert!(
        asked(serde_json::json!({
            "kind": "login",
            "belongsTo": ["https://example.com"],
            "secret": "hunter2",
        }))
        .is_err(),
        "an ask carries no secret"
    );
    let row = ask_row();
    let action = row.action.unwrap();
    assert_eq!(action.address.action, ASK);
    assert!(
        action
            .params
            .iter()
            .all(|param| param.ty != ParamType::Secret && !takes_item(param)),
        "an ask takes no secret and no reference"
    );
}

/// What could name a place passes the first check, and plain words, a bare host and another
/// scheme do not.
#[test]
fn a_place_is_checked_before_a_person_is_asked() {
    for place in [
        "https://example.com/login",
        "HTTP://example.com",
        "imap://mail.example.com:993",
        "smtp://mail.example.com:465",
        "ssh://git.example.com",
    ] {
        assert!(looks_like_place(place), "{place}");
    }
    for other in [
        "",
        "example.com",
        "my bank",
        "ftp://example.com",
        "https://",
        "https://exa mple.com",
    ] {
        assert!(!looks_like_place(other), "{other:?}");
    }
}

/// Each use by reference takes the item's address where the secret would stand and no secret
/// anywhere, tells an agent how the reference is come by, and is found again by its target and
/// by no other.
#[test]
fn a_use_by_reference_takes_no_secret_and_says_how_to_ask() {
    let rows = use_rows();
    assert_eq!(rows.len(), 2);
    for row in &rows {
        let action = row.action.as_ref().unwrap();
        assert_eq!(
            action
                .params
                .iter()
                .filter(|param| takes_item(param))
                .count(),
            1,
            "{}",
            action.address.action
        );
        assert!(
            !action
                .params
                .iter()
                .any(|param| param.ty == ParamType::Secret)
        );
        assert_eq!(action.contract.kind, tilo_actions::ActionKind::Outside);
        let description = action.description.as_deref().unwrap();
        for said in ["vault's `ask`", "`reference`", "asked first", "Always"] {
            assert!(description.contains(said), "{said}: {description}");
        }
        let target = RemoteActionTarget {
            provider: row.provider.clone(),
            address: action.address.clone(),
        };
        assert_eq!(
            use_of(&target).map(|found| found.address),
            Some(action.address.clone())
        );
        assert_eq!(
            use_word(&target),
            format!("{}/{}", row.provider, action.address.action)
        );
    }
    assert!(use_of(&target(LIST)).is_none());
    let accounts = |action: &str| RemoteActionTarget {
        provider: ACCOUNTS_PROVIDER.to_owned(),
        address: ActionAddress::new("accounts", action),
    };
    assert!(use_of(&accounts("add-mail")).is_none());
    assert!(
        use_of(&RemoteActionTarget {
            provider: MODELS_PROVIDER.to_owned(),
            address: ActionAddress::new("models", "set-key"),
        })
        .is_none()
    );

    // A review of a use shows the reference withheld, as it shows a secret.
    let mail = use_of(&accounts("add-mail-with-item")).unwrap();
    let arguments = serde_json::json!({
        "address": "ada@example.test",
        "incomingHost": "imap.example.test",
        "outgoingHost": "smtp.example.test",
        "password": format!("vault.item:{ID}"),
    });
    let draft = tilo_actions::Draft::render(&mail, arguments.as_object().unwrap());
    let shown = serde_json::to_string(&draft).unwrap();
    assert!(!shown.contains(ID), "{shown}");
    assert!(shown.contains("ada@example.test"), "{shown}");
}

/// What an item remembers reads with and without leaves.
#[test]
fn leaves_are_listed_beside_the_uses() {
    let none: ItemUses = serde_json::from_value(serde_json::json!({"uses": []})).unwrap();
    assert!(none.always.is_empty());
    assert_eq!(
        serde_json::to_value(&none).unwrap(),
        serde_json::json!({"uses": []})
    );
    let leave = AlwaysUse {
        agent: "scout".into(),
        app: String::new(),
        by: "Scout".into(),
        using: "tilo.accounts/add-mail-with-item".into(),
        places: vec!["imap://mail.example.test:993".into()],
        since: 7,
    };
    let some = ItemUses {
        uses: Vec::new(),
        always: vec![leave],
    };
    let said = serde_json::to_value(&some).unwrap();
    assert_eq!(said["always"][0]["agent"], "scout");
    assert_eq!(said["always"][0]["since"], 7);
    assert!(
        said["always"][0].get("app").is_none(),
        "an agent's leave names no app"
    );
    assert_eq!(serde_json::from_value::<ItemUses>(said).unwrap(), some);
    let back = decoded(
        TAKE_BACK,
        &object(serde_json::json!({"item": format!("vault.item:{ID}")})),
    )
    .unwrap();
    assert!(matches!(
        back,
        VaultAction::TakeBack {
            agent: None,
            app: None,
            ..
        }
    ));
}

/// A use says what it was for where that was written down, and reads without it where it was
/// not.
#[test]
fn a_use_says_what_it_was_for_where_that_is_known() {
    let plain: ItemUse = serde_json::from_value(serde_json::json!({
        "at": 7,
        "places": ["https://api.example.test"],
    }))
    .unwrap();
    assert!(plain.using.is_empty() && plain.what.is_empty() && plain.allowed.is_empty());
    assert_eq!(
        serde_json::to_value(&plain).unwrap(),
        serde_json::json!({"at": 7, "places": ["https://api.example.test"]})
    );
    let call = ItemUse {
        at: 8,
        places: vec!["https://api.example.test".into()],
        by: "Scout".into(),
        using: "tilo.vault/web-call".into(),
        what: "GET /v1/balance".into(),
        allowed: "once".into(),
    };
    let said = serde_json::to_value(&call).unwrap();
    assert_eq!(said["using"], "tilo.vault/web-call");
    assert_eq!(said["what"], "GET /v1/balance");
    assert_eq!(said["allowed"], "once");
    assert_eq!(serde_json::from_value::<ItemUse>(said).unwrap(), call);
}

/// A secret never reaches a log, and no reading has a field that could carry one: a view says
/// the kind, the places, and the two times.
#[test]
fn nothing_read_back_carries_a_secret() {
    let adding = decoded(
        ADD,
        &object(serde_json::json!({
            "kind": "login",
            "belongsTo": ["https://example.com"],
            "username": "ada@example.test",
            "secret": "hunter2",
            "fields": [{"label": "PIN", "value": "8642"}],
        })),
    )
    .unwrap();
    let printed = format!("{adding:?}");
    assert!(
        !printed.contains("hunter2") && !printed.contains("ada@") && !printed.contains("8642"),
        "{printed}"
    );
    let field = ItemField {
        label: "PIN".into(),
        value: "8642".into(),
    };
    assert!(!format!("{field:?}").contains("8642"));
    for printed in [
        format!(
            "{:?}",
            Revealed {
                value: "hunter2".into()
            }
        ),
        format!(
            "{:?}",
            Released {
                secret: "hunter2".into()
            }
        ),
        format!(
            "{:?}",
            PageFilled {
                username: "ada".into(),
                secret: "hunter2".into()
            }
        ),
        format!(
            "{:?}",
            DeviceKey {
                key: "hunter2".into()
            }
        ),
    ] {
        assert!(!printed.contains("hunter2"), "{printed}");
    }

    let view = ItemView {
        reference: ItemRef::from_id(ID).unwrap(),
        kind: ItemKind::CodeGenerator,
        belongs_to: vec!["https://example.com".into()],
        title: Some("Example".into()),
        added_at: 1_800_000_000,
        last_used_at: Some(1_800_000_100),
        algorithm: None,
        digits: None,
        period: None,
        tags: Vec::new(),
        fields: Vec::new(),
        has_notes: false,
        changed_at: 1_800_000_000,
        concerns: Vec::new(),
    };
    let json = serde_json::to_value(KeptItems { items: vec![view] }).unwrap();
    let mut fields: Vec<&str> = json["items"][0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        [
            "addedAt",
            "belongsTo",
            "changedAt",
            "kind",
            "lastUsedAt",
            "reference",
            "title"
        ]
    );
    assert_eq!(json["items"][0]["kind"], "code-generator");
    // A view with a field of its own making is not one.
    assert!(
        serde_json::from_value::<ItemView>(serde_json::json!({
            "reference": format!("vault.item:{ID}"),
            "kind": "login",
            "belongsTo": [],
            "addedAt": 0,
            "secret": "hunter2",
        }))
        .is_err()
    );
}

/// A page's field filled from an item is a use a leave and a rule can name and a person can be
/// shown, and no catalogue offers it.
#[test]
fn filling_a_page_is_a_use_no_catalogue_offers() {
    let target = target(FILL_PAGE);
    assert_eq!(use_word(&target), "tilo.vault/fill-page");
    let shown = use_of(&target).expect("a person is shown what it is");
    assert_eq!(shown.address, target.address);
    assert!(shown.params.iter().any(takes_item));
    assert!(
        use_rows()
            .iter()
            .all(|row| row.action.as_ref().map(|action| &action.address) != Some(&target.address)),
        "no turn is offered it by a listing"
    );
    let fill = decoded(
        FILL_PAGE,
        &object(serde_json::json!({
            "site": "https://example.test",
            "item": format!("vault.item:{}", "0".repeat(32)),
        })),
    )
    .unwrap();
    let VaultAction::FillPage { site, reason, .. } = fill else {
        panic!("a fill reads as one");
    };
    assert_eq!(site, "https://example.test");
    assert!(reason.is_none());
}

/// What a turn is offered names the field and the item by reference, and never the site: where
/// the page is, is its window's to say.
#[test]
fn a_turn_names_the_field_and_the_item_and_never_the_site() {
    let row = fill_row();
    assert_eq!(row.provider, PROVIDER);
    let offered = row.action.expect("the row offers the fill");
    assert_eq!(offered.address, target(FILL_PAGE).address);
    assert_eq!(offered.params.len(), 3);
    assert!(offered.params.iter().any(takes_item));
    let item = format!("vault.item:{}", "0".repeat(32));
    let named: FillField =
        serde_json::from_value(serde_json::json!({"element": "d1e5", "item": item})).unwrap();
    assert_eq!(named.element, "d1e5");
    assert!(named.reason.is_empty());
    assert!(
        serde_json::from_value::<FillField>(serde_json::json!({
            "element": "d1e5",
            "item": item,
            "site": "https://example.test",
        }))
        .is_err(),
        "a turn that names a site is not read"
    );
}
