use super::*;

tilo_test_env::scenario! {
    fn a_control_is_named_in_words_a_person_reads;

    phase names_read_in_sentence_case;
    {
        assert_eq!(sentence("new_note"), "New note");
        assert_eq!(sentence("NewNote"), "New note");
        assert_eq!(sentence("toggle_pin"), "Toggle pin");
        assert_eq!(sentence("Note"), "Note");
        assert_eq!(sentence(""), "");
    }

    phase a_bound_control_is_not_named_by_a_name_in_the_code;
    // A control bound to view state is not announced by that state's name in the code, which
    // a person never says; it is announced by what the view says it is for.
    {
        let state = Binding {
            target: BindTarget::State {
                key: crate::runtime::StateKey::view("w", MemberId::person("me").into(), "draft"),
                name: "draft".to_owned(),
                ty: "Text".to_owned(),
            },
            author: crate::runtime::ModuleHash(String::new()),
        };
        assert_eq!(
            announced(&state, |_, _| "should not be asked".to_owned()),
            ""
        );
        let field = Binding {
            target: BindTarget::Field {
                target: ResourceRef::parse_id("r1", String::new()).expect("a resource id"),
                field: "due".to_owned(),
            },
            author: crate::runtime::ModuleHash(String::new()),
        };
        assert_eq!(announced(&field, |_, field| sentence(field)), "Due");
    }
}

#[test]
fn a_splice_lands_on_character_boundaries() {
    assert_eq!(spliced("Q3 plan", 7, 0, "s"), "Q3 plans");
    assert_eq!(spliced("Q3 plans", 2, 6, ""), "Q3");
    assert_eq!(
        spliced("naïve", 3, 1, "i"),
        "nai\u{308}ve".replace("i\u{308}", "i")
    );
    assert_eq!(spliced("", 9, 9, "x"), "x");
}

struct EnglishWords;

impl tilo_formats::Words for EnglishWords {
    fn word(&self, key: &str, args: &[(&str, tilo_formats::Word)]) -> String {
        let get = |name: &str| {
            args.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| match v {
                    tilo_formats::Word::Text(t) => t.clone(),
                    tilo_formats::Word::Number(n) => format!("{n}"),
                })
                .unwrap_or_default()
        };
        match key {
            "day-today" => "Today".to_owned(),
            "format-clock-hours" => "12".to_owned(),
            "format-time-am" => "AM".to_owned(),
            "format-time-pm" => "PM".to_owned(),
            "format-month-short" => {
                let months = [
                    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov",
                    "Dec",
                ];
                let Some((_, tilo_formats::Word::Number(n))) = args.first() else {
                    return String::new();
                };
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "month numbers in tests are 1..=12"
                )]
                months
                    .get((*n as usize).saturating_sub(1))
                    .copied()
                    .unwrap_or_default()
                    .to_owned()
            }
            "format-month-day" => format!("{} {}", get("month"), get("day")),
            "format-month-day-year" => {
                format!("{} {}, {}", get("month"), get("day"), get("year"))
            }
            "format-month-day-time" => {
                format!("{} {}, {}", get("month"), get("day"), get("time"))
            }
            other => other.to_owned(),
        }
    }
}

static ENGLISH_WORDS: EnglishWords = EnglishWords;

struct PinnedPlatform {
    now: &'static str,
    zone: i64,
    now_calls: std::cell::Cell<usize>,
}

impl PinnedPlatform {
    fn with_zone(now: &'static str, zone: i64) -> Self {
        Self {
            now,
            zone,
            now_calls: std::cell::Cell::new(0),
        }
    }
}

impl Locale for PinnedPlatform {
    fn add_words(&self, _window: bool, _catalogues: &[(String, String)]) {}
    fn words_revision(&self) -> u64 {
        0
    }
    fn language(&self) -> String {
        "en".to_owned()
    }
    fn formats(&self) -> tilo_formats::RegionFormats {
        tilo_formats::RegionFormats {
            time: tilo_formats::TimeStyle::H12,
            ..tilo_formats::RegionFormats::default()
        }
    }
    fn format_words(&self) -> &dyn tilo_formats::Words {
        &ENGLISH_WORDS
    }
}

impl Platform for PinnedPlatform {
    fn load(&self, _host: &Host) -> u64 {
        0
    }
    fn locale(&self) -> Option<&dyn Locale> {
        Some(self)
    }
    fn phrase(&self, id: &str, _args: &[(String, String)]) -> String {
        id.to_owned()
    }
    fn now(&self) -> String {
        self.now_calls.set(self.now_calls.get().saturating_add(1));
        self.now.to_owned()
    }
    fn moment(&self) -> Zoned {
        let offset = i32::try_from(self.zone)
            .ok()
            .and_then(|seconds| jiff::tz::Offset::from_seconds(seconds).ok())
            .unwrap_or(jiff::tz::Offset::UTC);
        self.now
            .parse::<jiff::Timestamp>()
            .unwrap()
            .to_zoned(jiff::tz::TimeZone::fixed(offset))
    }
    fn waker(&self) -> crate::background::Waker {
        Arc::new(|| {})
    }
}

/// A platform that overrides only `now()` (leaving `moment()` at the trait default), so tests
/// verify the default reads and parses `self.now()` at most once per `shown`/`sentence`/`zone`.
struct TextOnlyPlatform {
    now: &'static str,
    now_calls: std::cell::Cell<usize>,
}

impl TextOnlyPlatform {
    fn new(now: &'static str) -> Self {
        Self {
            now,
            now_calls: std::cell::Cell::new(0),
        }
    }
}

impl Locale for TextOnlyPlatform {
    fn add_words(&self, _window: bool, _catalogues: &[(String, String)]) {}
    fn words_revision(&self) -> u64 {
        0
    }
    fn language(&self) -> String {
        "en".to_owned()
    }
    fn formats(&self) -> tilo_formats::RegionFormats {
        tilo_formats::RegionFormats {
            time: tilo_formats::TimeStyle::H12,
            ..tilo_formats::RegionFormats::default()
        }
    }
    fn format_words(&self) -> &dyn tilo_formats::Words {
        &ENGLISH_WORDS
    }
}

impl Platform for TextOnlyPlatform {
    fn load(&self, _host: &Host) -> u64 {
        0
    }
    fn locale(&self) -> Option<&dyn Locale> {
        Some(self)
    }
    fn phrase(&self, id: &str, _args: &[(String, String)]) -> String {
        id.to_owned()
    }
    fn now(&self) -> String {
        self.now_calls.set(self.now_calls.get().saturating_add(1));
        self.now.to_owned()
    }
    fn waker(&self) -> crate::background::Waker {
        Arc::new(|| {})
    }
}

/// An implementer that pins `moment()` (or only `now()`) gets `shown()`, `sentence()` and
/// `zone()` from that single moment, never building/parsing text when `moment()` is overridden
/// and parsing `now()` at most once per call (including when unparseable) on the default path.
#[test]
fn platform_defaults_derive_shown_and_sentence_from_now_and_zone() {
    struct ZoneGuard;
    impl Drop for ZoneGuard {
        fn drop(&mut self) {
            tilo_clock::local::install(None);
        }
    }

    // 1. Overriding `moment()` (like live hosts do from `tilo_clock::local::now`) derives `zone`,
    // `shown` and `sentence` without ever calling `now()` to build or parse text.
    // 2026-10-27T23:30:00Z is Oct 27 in UTC, and already Oct 28 at UTC+2.
    let utc = PinnedPlatform::with_zone("2026-10-27T23:30:00Z", 0);
    assert_eq!(utc.zone(), 0);
    assert_eq!(utc.shown("date", "2026-10-27"), "Today");
    assert_eq!(utc.sentence("date", "2026-10-27"), "Oct 27");
    assert_eq!(utc.sentence("date", "2025-10-27"), "Oct 27, 2025");
    assert_eq!(
        utc.sentence("time", "2026-10-27T14:05:00Z"),
        "Oct 27, 2:05 PM"
    );
    assert_eq!(
        utc.now_calls.get(),
        0,
        "overriding moment() never calls now() to format or parse text"
    );

    let plus_two = PinnedPlatform::with_zone("2026-10-27T23:30:00Z", 2 * 3600);
    assert_eq!(plus_two.zone(), 2 * 3600);
    assert_eq!(plus_two.shown("date", "2026-10-28"), "Today");
    assert_eq!(
        plus_two.sentence("time", "2026-10-27T14:05:00Z"),
        "Oct 27, 4:05 PM"
    );
    assert_eq!(plus_two.now_calls.get(), 0);

    // 2. A platform pinning only `now()` as text parses it exactly once per call to `shown`,
    // `sentence` and `zone`, and respects the thread-local zone.
    tilo_clock::local::install(Some(tilo_clock::local::Zone::of(jiff::tz::TimeZone::UTC)));
    let _zone = ZoneGuard;
    let text_only = TextOnlyPlatform::new("2026-10-27T23:30:00Z");
    assert_eq!(text_only.shown("date", "2026-10-27"), "Today");
    assert_eq!(text_only.now_calls.get(), 1, "shown reads now() once");
    assert_eq!(
        text_only.sentence("time", "2026-10-27T14:05:00Z"),
        "Oct 27, 2:05 PM"
    );
    assert_eq!(text_only.now_calls.get(), 2, "sentence reads now() once");
    assert_eq!(text_only.zone(), 0);
    assert_eq!(text_only.now_calls.get(), 3, "zone reads now() once");

    // 3. Even an unparseable `now()` is read and warned once per call, not twice.
    let invalid = TextOnlyPlatform::new("not-a-timestamp");
    let _ = invalid.shown("date", "2026-10-27");
    assert_eq!(
        invalid.now_calls.get(),
        1,
        "unparseable now() is parsed once per shown() call"
    );
    let _ = invalid.sentence("date", "2026-10-27");
    assert_eq!(
        invalid.now_calls.get(),
        2,
        "unparseable now() is parsed once per sentence() call"
    );
    let _ = invalid.zone();
    assert_eq!(
        invalid.now_calls.get(),
        3,
        "unparseable now() is parsed once per zone() call"
    );
}
