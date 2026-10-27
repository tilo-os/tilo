#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

use std::collections::BTreeMap;

use proptest::prelude::*;
use serde::Deserialize;
use tilo_reference::Span;
use tilo_reference::link::{
    AppId, HostOrigin, Link, LinkSecrets, LinkTarget, Mode, RecordSegment, ResourceAddress, Scope,
    Slug, TargetJson, WebEntries, WebEntry,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Corpus {
    entries: Vec<EntryRow>,
    links: Vec<LinkRow>,
    native: Vec<NativeRow>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryRow {
    base: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkRow {
    url: String,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    target: Option<TargetJson>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    secrets: BTreeMap<String, String>,
    #[serde(default)]
    public: Option<String>,
    #[serde(default)]
    formats: Option<String>,
    #[serde(default)]
    #[allow(dead_code, reason = "documentation for readers of the corpus")]
    note: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeRow {
    uri: String,
    #[serde(default)]
    target: Option<TargetJson>,
    #[serde(default)]
    error: Option<String>,
}

fn corpus() -> Corpus {
    serde_json::from_str(include_str!("links.json")).unwrap()
}

fn entries(corpus: &Corpus) -> WebEntries {
    WebEntries::new(
        corpus
            .entries
            .iter()
            .map(|row| WebEntry::new(&row.base).unwrap())
            .collect(),
    )
}

fn secrets_of(link: &Link) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (name, secret) in [
        ("invite", &link.secrets.invite),
        ("ticket", &link.secrets.ticket),
        ("token", &link.secrets.token),
        ("pair", &link.secrets.pair),
        ("install", &link.secrets.install),
    ] {
        if let Some(secret) = secret {
            out.insert(name.to_owned(), secret.expose().to_owned());
        }
    }
    out
}

/// Review focus R2.
#[test]
fn corpus_round_trips() {
    let corpus = corpus();
    let entries = entries(&corpus);
    for row in corpus.links.iter().filter(|row| row.target.is_some()) {
        let link =
            Link::parse(&row.url, &entries).unwrap_or_else(|error| panic!("{}: {error}", row.url));
        assert_eq!(
            Some(TargetJson::from(&link.target)),
            row.target,
            "{}",
            row.url
        );
        let mode = match link.mode {
            Mode::Desktop => "desktop",
            Mode::App => "app",
        };
        assert_eq!(row.mode.as_deref(), Some(mode), "{}", row.url);
        assert_eq!(secrets_of(&link), row.secrets, "{}", row.url);
        let (entry, _) = entries.find(&row.url).unwrap();
        let expected = row.formats.as_deref().unwrap_or(&row.url);
        assert_eq!(
            link.to_url(entry).unwrap(),
            expected,
            "format is the inverse of parse"
        );
        assert_eq!(
            row.public.is_some(),
            !row.secrets.is_empty(),
            "{}: public iff secrets",
            row.url
        );
        if let Some(public) = &row.public {
            assert_eq!(&link.public().to_url(entry).unwrap(), public);
        }
        let back = TargetJson::from(&link.target);
        assert_eq!(
            LinkTarget::try_from(back).unwrap(),
            link.target,
            "JSON form round-trips"
        );
    }
}

/// Review focus R3.
#[test]
fn corpus_refusals_are_exact() {
    let corpus = corpus();
    let entries = entries(&corpus);
    for row in corpus.links.iter().filter(|row| row.error.is_some()) {
        let error = Link::parse(&row.url, &entries).expect_err(&row.url);
        assert_eq!(
            Some(error.code()),
            row.error.as_deref(),
            "{}: {error}",
            row.url
        );
        assert!(!error.to_string().is_empty());
    }
    let long = format!(
        "https://tilo.so/#/me/so.tilo.files/file:{}",
        "a".repeat(9000)
    );
    assert_eq!(Link::parse(&long, &entries).unwrap_err().code(), "too-long");
}

#[test]
fn native_corpus() {
    for row in corpus().native {
        match (&row.target, &row.error) {
            (Some(target), None) => {
                let link = Link::parse_native(&row.uri).unwrap();
                assert_eq!(&TargetJson::from(&link.target), target);
                assert_eq!(link.mode, Mode::Desktop);
                assert_eq!(link.to_native().unwrap(), row.uri);
            }
            (None, Some(code)) => {
                assert_eq!(Link::parse_native(&row.uri).unwrap_err().code(), code);
            }
            _ => panic!("{}: a row has a target or an error", row.uri),
        }
    }
}

fn scope() -> impl Strategy<Value = Scope> {
    prop_oneof![
        Just(Scope::Own),
        (
            prop_oneof![
                Just("https://tilo.so"),
                Just("https://quinten.tilo.cloud"),
                Just("http://localhost:9700")
            ],
            any::<u128>(),
            proptest::option::of("[a-z0-9][a-z0-9-]{0,20}[a-z0-9]"),
        )
            .prop_map(|(host, bits, slug)| Scope::Room {
                host: HostOrigin::new(host).unwrap(),
                room: uuid::Builder::from_random_bytes(bits.to_le_bytes()).into_uuid(),
                slug: slug.map(|slug| Slug::parse(&slug).unwrap()),
            }),
    ]
}

fn target() -> impl Strategy<Value = LinkTarget> {
    let app = "[a-z]{1,8}(\\.[a-z0-9-]{1,8}){1,3}";
    let record = prop_oneof![
        Just(RecordSegment::Primary),
        "[a-z]{1,6}:\\PC{0,40}".prop_map(RecordSegment::Id),
    ];
    let resource = (
        "[a-z_.]{1,12}",
        prop_oneof!["\\PC{1,40}".boxed(), "[a-z0-9]{1,10}".boxed()],
        any::<bool>(),
        proptest::option::of((0_u32..1000, 0_u32..1000, any::<bool>())),
    )
        .prop_map(|(kind, rest, prefixed, span)| ResourceAddress {
            id: if prefixed {
                format!("{kind}:{rest}")
            } else {
                rest
            },
            kind,
            span: span.map(|(a, b, chars)| {
                let (start, end) = (a.min(b) as usize, a.max(b) as usize);
                if chars {
                    Span::chars(start, end)
                } else {
                    Span::bytes(start, end)
                }
            }),
        });
    (
        scope(),
        proptest::option::of((
            app,
            proptest::option::of((
                record,
                proptest::option::of(resource),
                proptest::option::of("[a-z][a-z0-9-]{0,15}"),
            )),
        )),
    )
        .prop_map(|(scope, app)| {
            let mut target = LinkTarget::new(scope);
            if let Some((app, rest)) = app {
                target.app = Some(AppId::new(app).unwrap());
                if let Some((record, resource, view)) = rest {
                    target.record = Some(record);
                    target.resource = resource;
                    target.view = view;
                }
            }
            target
        })
        .prop_filter("structurally valid", |target| target.check().is_ok())
}

proptest! {
    /// Review focus R2: whatever the formatter writes parses back to the same target.
    #[test]
    fn round_trip_is_identity(target in target(), app_mode in any::<bool>()) {
        let entry = WebEntry::new("https://tilo.so/").unwrap();
        let entries = WebEntries::new(vec![entry.clone()]);
        let link = Link { mode: if app_mode { Mode::App } else { Mode::Desktop }, target, secrets: LinkSecrets::default() };
        let url = link.to_url(&entry).unwrap();
        let parsed = Link::parse(&url, &entries).unwrap();
        prop_assert_eq!(&parsed, &link);
        prop_assert_eq!(parsed.to_url(&entry).unwrap(), url);
        let native = link.to_native().unwrap();
        prop_assert_eq!(Link::parse_native(&native).unwrap().target, link.target);
    }

    /// Review focus R2: any string that parses is already in its one spelling.
    #[test]
    fn whatever_parses_is_canonical(parts in proptest::collection::vec(prop_oneof![
        Just("/"), Just("me"), Just("w"), Just("-"), Just("@quinten.tilo.cloud"), Just("@tilo.so"),
        Just("6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90"), Just("rg~"), Just("so.tilo.chat"), Just("uuid:1"),
        Just("chat.channel:general"), Just("k"), Just("%41"), Just("%2F"), Just("%20"), Just("?"), Just("&"),
        Just("view=v"), Just("chars=1-2"), Just("bytes=0-0"), Just("invite=x"), Just("token=y"), Just(":"),
    ], 0..12)) {
        let entry = WebEntry::new("https://tilo.so/").unwrap();
        let entries = WebEntries::new(vec![entry.clone()]);
        let url = format!("https://tilo.so/#{}", parts.concat());
        if let Ok(link) = Link::parse(&url, &entries) {
            prop_assert_eq!(link.to_url(&entry).unwrap(), url);
        }
    }
}
