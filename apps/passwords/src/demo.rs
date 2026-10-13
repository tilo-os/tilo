//! The fixture vault captures and tests show. Every value is synthetic and
//! never backed by a real vault.

use accounts::{CredentialGrant, SecretId, SecretItem, SecretItemKind};

const DAY: u64 = 24 * 60 * 60;

struct Fixture {
    id: &'static str,
    kind: SecretItemKind,
    title: &'static str,
    username: &'static str,
    password: &'static str,
    origins: &'static [&'static str],
    favorite: bool,
    days_old: u64,
}

const FIXTURES: [Fixture; 8] = [
    Fixture {
        id: "demo-bank",
        kind: SecretItemKind::Login,
        title: "Northstar Bank",
        username: "ada@example.com",
        password: "Harbor-Lights-2019",
        origins: &["https://bank.example", "https://login.bank.example"],
        favorite: true,
        days_old: 3,
    },
    Fixture {
        id: "demo-mail",
        kind: SecretItemKind::Login,
        title: "Personal mail",
        username: "ada@mail.example",
        password: "Harbor-Lights-2019",
        origins: &["https://mail.example"],
        favorite: false,
        days_old: 410,
    },
    Fixture {
        id: "demo-code",
        kind: SecretItemKind::Login,
        title: "Code hosting",
        username: "adalovelace",
        password: "q7Lm-Vt2x-Rk9p-Hw4c",
        origins: &["https://code.example"],
        favorite: true,
        days_old: 1,
    },
    Fixture {
        id: "demo-console",
        kind: SecretItemKind::ApiKey,
        title: "Cloud console",
        // Cloud account IDs mix 0 and O, 1, I and l: the case the
        // Identifier text kind sets apart.
        username: "lovelace-prod-0Il1O",
        password: "ck_live_5bT9wQ2mXc8LrV4n",
        origins: &["https://console.cloud.example"],
        favorite: false,
        days_old: 12,
    },
    Fixture {
        id: "demo-streaming",
        kind: SecretItemKind::Login,
        title: "Streaming",
        username: "ada.l@example.com",
        password: "tv1234",
        origins: &["https://watch.example"],
        favorite: false,
        days_old: 60,
    },
    Fixture {
        id: "demo-wifi",
        kind: SecretItemKind::Wifi,
        title: "Home Wi-Fi",
        username: "",
        password: "harbourlights",
        origins: &[],
        favorite: false,
        days_old: 430,
    },
    Fixture {
        id: "demo-design",
        kind: SecretItemKind::Other,
        title: "Design library licence",
        username: "ada@studio.example",
        password: "DL-4K7Q-92XM-PL3T",
        origins: &[],
        favorite: false,
        days_old: 40,
    },
    Fixture {
        id: "demo-recovery",
        kind: SecretItemKind::SecureNote,
        title: "Recovery codes",
        username: "",
        password: "",
        origins: &[],
        favorite: false,
        days_old: 20,
    },
];

/// The fixture items, with their secrets, sorted by title.
pub(crate) fn full_items() -> Vec<SecretItem> {
    let now = crate::vault::now_unix();
    let mut items: Vec<SecretItem> = FIXTURES
        .iter()
        .map(|fixture| {
            let mut item = SecretItem::login(fixture.title, fixture.username, fixture.password);
            item.id = SecretId(fixture.id.into());
            item.kind = fixture.kind;
            for origin in fixture.origins {
                let _ = item.add_origin(origin);
            }
            if fixture.favorite {
                item.tags.insert("favorite".into());
            }
            match fixture.kind {
                SecretItemKind::Wifi => {
                    item.fields.insert("network".into(), "Lovelace home".into());
                }
                SecretItemKind::SecureNote => {
                    item.notes = "Code hosting: 4821-7730, 1956-0442, 7303-9185".into();
                }
                SecretItemKind::Other => {
                    item.tags.insert("work".into());
                }
                SecretItemKind::Login | SecretItemKind::ApiKey => {}
            }
            let changed = now.saturating_sub(fixture.days_old.saturating_mul(DAY));
            item.created_unix = changed;
            item.updated_unix = changed;
            item
        })
        .collect();
    items.sort_by_key(|item| item.title.to_lowercase());
    items
}

/// The fixture items as the window caches them: metadata only.
pub(crate) fn items() -> Vec<SecretItem> {
    full_items()
        .into_iter()
        .map(|mut item| {
            item.password.clear();
            item.notes.clear();
            item
        })
        .collect()
}

/// Logins a browser export would hold, for the import preview.
pub(crate) fn import_rows() -> Vec<crate::transfer::ImportLogin> {
    use zeroize::Zeroizing;
    [
        ("Northstar Bank", "ada@example.com", "https://bank.example"),
        ("Library card", "ada.lovelace", "https://library.example"),
        ("Train tickets", "ada@example.com", "https://rail.example"),
        ("Photo backup", "ada@example.com", "https://photos.example"),
    ]
    .into_iter()
    .map(|(title, username, origin)| crate::transfer::ImportLogin {
        title: Zeroizing::new(title.into()),
        username: Zeroizing::new(username.into()),
        origin: Zeroizing::new(origin.into()),
        password: Zeroizing::new("not-rendered".into()),
        notes: Zeroizing::new(String::new()),
    })
    .collect()
}

/// Enough further logins that the list scrolls.
pub(crate) fn more_items() -> Vec<SecretItem> {
    let now = crate::vault::now_unix();
    [
        ("Airline", "ada@example.com", "https://fly.example"),
        ("Bookshop", "ada@example.com", "https://books.example"),
        ("City council", "ada.lovelace", "https://council.example"),
        ("Dentist", "ada@example.com", "https://smile.example"),
        ("Electricity", "ada@example.com", "https://power.example"),
        ("Fitness club", "ada.l", "https://gym.example"),
        (
            "Grocery delivery",
            "ada@example.com",
            "https://groceries.example",
        ),
        (
            "Health insurance",
            "ada@example.com",
            "https://cover.example",
        ),
        ("Internet provider", "lovelace-home", "https://isp.example"),
        ("Job board", "ada@example.com", "https://jobs.example"),
        ("Kids' school", "ada.lovelace", "https://school.example"),
        (
            "Language course",
            "ada@example.com",
            "https://lingo.example",
        ),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (title, username, origin))| {
        let mut item = SecretItem::login(title, username, "");
        item.id = SecretId(format!("demo-more-{index}"));
        let _ = item.add_origin(origin);
        let days = u64::try_from(index)
            .unwrap_or(0)
            .saturating_mul(9)
            .saturating_add(5);
        item.created_unix = now.saturating_sub(days.saturating_mul(DAY));
        item.updated_unix = item.created_unix;
        item
    })
    .collect()
}

/// Apps the fixture vault lets use an item without asking.
pub(crate) fn grants() -> Vec<CredentialGrant> {
    let now = crate::vault::now_unix();
    [
        (
            "so.tilo.browser",
            "demo-bank",
            Some("https://bank.example"),
            30_u64,
        ),
        (
            "so.tilo.mail",
            "demo-mail",
            Some("https://mail.example"),
            90,
        ),
        (
            "so.tilo.code-editor",
            "demo-code",
            Some("https://code.example"),
            7,
        ),
    ]
    .into_iter()
    .enumerate()
    .filter_map(|(index, (app, item, origin, days))| {
        serde_json::from_value(serde_json::json!({
            "id": format!("demo-grant-{index}"),
            "app_id": app,
            "item_id": item,
            "origin": origin,
            "created_unix": now.saturating_sub(days.saturating_mul(DAY)),
        }))
        .ok()
    })
    .collect()
}
