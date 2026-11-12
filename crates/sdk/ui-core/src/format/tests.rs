use super::*;
use crate::clock::TimeStyle;
use crate::i18n::I18nManager;
use crate::widget::Tone;
use chrono::NaiveDate;

/// Speak `locale` on this thread, the toolkit's catalogue loaded.
/// The window speaks `locale`: its own catalogue is in it and the person
/// selected it. Formats follow the window's language, so a process whose own
/// copy is English keeps English dates whatever the device's locale says.
fn speak(locale: &str) {
    let mut catalogues = vec![("en-US", "")];
    if locale != "en-US" {
        catalogues.push((locale, ""));
    }
    I18nManager::install("en-US", locale, &catalogues).expect("the window's catalogues install");
    I18nManager::register_widget_messages();
}

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> NaiveDateTime {
    day(y, m, d).and_hms_opt(h, min, 0).unwrap()
}

fn formats(date: DateStyle, time: TimeStyle) -> Formats {
    Formats::with(RegionFormats {
        date,
        time,
        ..RegionFormats::default()
    })
}

const H24: Formats = Formats::with(RegionFormats {
    date: DateStyle::Iso,
    time: TimeStyle::H24,
    number: None,
    first_day: FirstDayOfWeek::Monday,
});

/// "Now" in every list case: Tuesday 30 June 2026, 14:00.
fn now() -> NaiveDateTime {
    at(2026, 6, 30, 14, 0)
}

#[test]
fn the_list_style_reads_as_the_person_asked_in_english_and_german() {
    speak("en-US");
    let cases = [
        (at(2026, 6, 30, 16, 2), "16:02", "16:02"),
        (at(2026, 6, 29, 9, 30), "Yesterday", "Gestern"),
        (at(2026, 7, 1, 9, 30), "Tomorrow", "Morgen"),
        (at(2026, 6, 28, 9, 30), "Sun", "So."),
        (at(2026, 6, 24, 9, 30), "Wed", "Mi."),
        (at(2026, 6, 23, 9, 30), "Jun 23", "23. Juni"),
        (at(2026, 1, 3, 9, 30), "Jan 3", "3. Jan."),
        (at(2025, 6, 30, 9, 30), "Jun 30, 2025", "30. Juni 2025"),
    ];
    for (when, english, _) in cases {
        assert_eq!(H24.list_date(when, now()), english, "{when}");
    }
    speak("de-DE");
    for (when, _, german) in cases {
        assert_eq!(H24.list_date(when, now()), german, "{when}");
    }
    speak("en-US");
    let twelve = formats(DateStyle::Iso, TimeStyle::H12);
    assert_eq!(twelve.list_date(at(2026, 6, 30, 16, 2), now()), "4:02 PM");
    assert_eq!(twelve.list_date(at(2026, 6, 30, 0, 5), now()), "12:05 AM");
}

#[test]
fn the_timestamp_and_date_styles_follow_the_date_and_clock_settings() {
    speak("en-US");
    let moment = at(2026, 9, 18, 12, 30);
    for (date, time, want) in [
        (DateStyle::Iso, TimeStyle::H24, "2026-09-18 12:30"),
        (DateStyle::Iso, TimeStyle::H12, "2026-09-18 12:30 PM"),
        (DateStyle::Us, TimeStyle::H12, "09/18/2026 12:30 PM"),
        (DateStyle::Eu, TimeStyle::H24, "18/09/2026 12:30"),
        (DateStyle::Long, TimeStyle::H24, "18 Sep 2026, 12:30"),
        (DateStyle::Long, TimeStyle::H12, "18 Sep 2026, 12:30 PM"),
    ] {
        assert_eq!(formats(date, time).timestamp(moment), want);
    }
    for style in DateStyle::ALL {
        assert_eq!(
            formats(style, TimeStyle::H24).date(day(2023, 3, 15)),
            style.example(super::words()),
            "the date style reads as Settings' example"
        );
    }
    for (style, want) in [
        (DateStyle::Iso, "2023-03-15"),
        (DateStyle::Us, "03/15/2023"),
        (DateStyle::Eu, "15/03/2023"),
        (DateStyle::Long, "15 Mar 2023"),
        (DateStyle::Language, "Mar 15, 2023"),
    ] {
        assert_eq!(style.example(super::words()), want);
    }

    speak("de-DE");
    assert_eq!(
        formats(DateStyle::Long, TimeStyle::H24).timestamp(moment),
        "18. Sept. 2026, 12:30"
    );
    assert_eq!(
        formats(DateStyle::Long, TimeStyle::H24).date(day(2023, 3, 15)),
        "15. März 2023"
    );
    assert_eq!(
        formats(DateStyle::Eu, TimeStyle::H24).timestamp(moment),
        "18/09/2026 12:30"
    );
}

/// With no date format or clock chosen, full timestamps, dates and times
/// follow the desktop's language and region; ISO, 12-hour and 24-hour are
/// only what someone picks.
#[test]
fn without_a_chosen_format_dates_and_clocks_follow_the_language() {
    assert_eq!(RegionFormats::default().date, DateStyle::Language);
    assert_eq!(RegionFormats::default().time, TimeStyle::Language);
    let unset = RegionFormats::from_section(serde_json::Map::new());
    assert_eq!(unset.date, DateStyle::Language);
    assert_eq!(unset.time, TimeStyle::Language);
    let moment = at(2026, 6, 30, 13, 42);
    let unchosen = Formats::default();
    let chosen24 = Formats::with(RegionFormats {
        time: TimeStyle::H24,
        ..RegionFormats::default()
    });

    speak("en-US");
    assert_eq!(unchosen.timestamp(moment), "Jun 30, 2026, 1:42 PM");
    assert_eq!(unchosen.time(&moment), "1:42 PM");
    assert!(!TimeStyle::Language.is_24_hour(super::words()));
    assert_eq!(chosen24.timestamp(moment), "Jun 30, 2026, 13:42");
    assert_eq!(unchosen.date(day(2026, 6, 30)), "Jun 30, 2026");
    assert_eq!(unchosen.date(day(2026, 1, 3)), "Jan 3, 2026");

    speak("de-DE");
    assert_eq!(unchosen.timestamp(moment), "30.06.2026, 13:42");
    assert_eq!(unchosen.date(day(2026, 1, 3)), "03.01.2026");
    assert_eq!(unchosen.time(&moment), "13:42");
    assert!(TimeStyle::Language.is_24_hour(super::words()));
    assert_eq!(TimeStyle::Language.example(super::words()), "14:05");
    assert_eq!(
        Formats::with(RegionFormats {
            time: TimeStyle::H12,
            ..RegionFormats::default()
        })
        .time(&moment),
        "1:42 PM",
        "a chosen 12-hour clock holds in German"
    );
    assert_eq!(DateStyle::Language.example(super::words()), "15.03.2023");
    // ISO stays an explicit choice, the same in every language.
    assert_eq!(
        formats(DateStyle::Iso, TimeStyle::H24).timestamp(moment),
        "2026-06-30 13:42"
    );
    // A spreadsheet cell that follows the language does so under any choice.
    assert_eq!(
        formats(DateStyle::Iso, TimeStyle::H24).language_date(day(2026, 6, 30)),
        "30.06.2026"
    );
    speak("en-US");
}

#[test]
fn headings_name_the_day_and_month_in_the_language() {
    speak("en-US");
    assert_eq!(H24.day_heading(day(2026, 6, 30), false), "Tuesday, June 30");
    assert_eq!(
        H24.day_heading(day(2026, 6, 30), true),
        "Tuesday, June 30, 2026"
    );
    assert_eq!(H24.month_heading(day(2026, 6, 30)), "June 2026");
    assert_eq!(H24.month_heading_short(day(2026, 6, 30)), "Jun 2026");
    assert_eq!(H24.day_heading_short(day(2026, 6, 30)), "Tue, Jun 30");
    assert_eq!(
        H24.day_range(day(2026, 6, 23), day(2026, 6, 29)),
        "Jun 23 – 29"
    );
    assert_eq!(
        H24.day_range(day(2026, 6, 29), day(2026, 7, 5)),
        "Jun 29 – Jul 5"
    );
    assert_eq!(
        H24.day_range(day(2025, 12, 29), day(2026, 1, 4)),
        "Dec 29, 2025 – Jan 4, 2026"
    );
    assert_eq!(H24.month_day_time(at(2026, 6, 30, 14, 5)), "Jun 30, 14:05");
    speak("de-DE");
    assert_eq!(H24.month_day_time(at(2026, 6, 30, 14, 5)), "30. Juni 14:05");
    assert_eq!(H24.day_heading_short(day(2026, 6, 30)), "Di., 30. Juni");
    assert_eq!(
        H24.day_range(day(2026, 6, 23), day(2026, 6, 29)),
        "23.–29. Juni"
    );
    assert_eq!(
        H24.day_heading(day(2026, 6, 30), false),
        "Dienstag, 30. Juni"
    );
    assert_eq!(
        H24.day_heading(day(2026, 6, 30), true),
        "Dienstag, 30. Juni 2026"
    );
    assert_eq!(H24.month_heading(day(2026, 3, 1)), "März 2026");
}

#[test]
fn the_week_starts_on_the_day_the_person_chose() {
    use chrono::Weekday::{Fri, Mon, Sat, Sun, Thu, Tue, Wed};
    for (first, want) in [
        (FirstDayOfWeek::Monday, [Mon, Tue, Wed, Thu, Fri, Sat, Sun]),
        (FirstDayOfWeek::Sunday, [Sun, Mon, Tue, Wed, Thu, Fri, Sat]),
        (
            FirstDayOfWeek::Saturday,
            [Sat, Sun, Mon, Tue, Wed, Thu, Fri],
        ),
    ] {
        let formats = Formats::with(RegionFormats {
            first_day: first,
            ..RegionFormats::default()
        });
        assert_eq!(formats.week(), want);
    }
}

#[test]
fn durations_read_in_words_with_their_two_largest_units() {
    use std::time::Duration;
    speak("en-US");
    for (secs, english, german) in [
        (0, "0 sec", "0 Sek."),
        (45, "45 sec", "45 Sek."),
        (245, "4 min 5 sec", "4 Min. 5 Sek."),
        (300, "5 min", "5 Min."),
        (3900, "1 hr 5 min", "1 Std. 5 Min."),
        (3605, "1 hr", "1 Std."),
        (86_400, "1 day", "1 Tag"),
        (2 * 86_400 + 3 * 3600 + 59, "2 days 3 hr", "2 Tage 3 Std."),
    ] {
        speak("en-US");
        assert_eq!(H24.duration(Duration::from_secs(secs)), english, "{secs}");
        speak("de-DE");
        assert_eq!(H24.duration(Duration::from_secs(secs)), german, "{secs}");
    }
    for (secs, want) in [
        (0, "0:00"),
        (59, "0:59"),
        (75, "1:15"),
        (3599, "59:59"),
        (3600, "1:00:00"),
        (3661, "1:01:01"),
    ] {
        assert_eq!(timecode(secs), want, "timecode({secs})");
    }
}

/// A byte formatter's unit matches its base: 1024 bytes is one *kibibyte*
/// (`KiB`, never `KB`), 1000 bytes one SI kilobyte (`kB`, never `KiB`).
#[test]
fn sizes_use_units_matching_their_base_and_the_languages_decimal_mark() {
    speak("en-US");
    for (n, want) in [
        (0, "0 B"),
        (999, "999 B"),
        (1000, "1 kB"),
        (1_500_000, "1.5 MB"),
        (3_000_000_000, "3 GB"),
    ] {
        assert_eq!(H24.size(n), want, "size({n})");
    }
    for (n, want) in [
        (1023, "1,023 B"),
        (1024, "1 KiB"),
        (5 * 1024 * 1024, "5 MiB"),
        (3 * 1024 * 1024 * 1024, "3 GiB"),
    ] {
        assert_eq!(H24.memory(n), want, "memory({n})");
    }
    for (rate, want) in [
        (0.0, "0 B/s"),
        (-5.0, "0 B/s"),
        (512.0, "512 B/s"),
        (1000.0, "1 kB/s"),
        (6_710_886.0, "6.7 MB/s"),
    ] {
        assert_eq!(H24.rate(rate), want, "rate({rate})");
    }

    speak("de-DE");
    assert_eq!(H24.size(1_500_000), "1,5 MB");
    assert_eq!(H24.memory(5 * 1024 * 1024), "5 MiB");
    assert_eq!(H24.memory(1536), "1,5 KiB");
    assert_eq!(H24.rate(6_710_886.0), "6,7 MB/s");
    assert_eq!(H24.size(999), "999 B");
}

#[test]
fn counts_group_digits_by_language_until_a_number_style_is_chosen() {
    speak("en-US");
    assert_eq!(H24.count(0), "0");
    assert_eq!(H24.count(999), "999");
    assert_eq!(H24.count(12_345), "12,345");
    assert_eq!(H24.items(1), "1 item");
    assert_eq!(H24.items(12), "12 items");
    assert_eq!(H24.items(1_234), "1,234 items");
    assert_eq!(H24.decimal(1234.5, 1), "1,234.5");
    assert_eq!(H24.decimal(-0.04, 1), "0.0");
    assert_eq!(H24.decimal(-2.5, 2), "-2.50");

    speak("de-DE");
    assert_eq!(H24.count(1_234_567), "1.234.567");
    assert_eq!(H24.items(1), "1 Objekt");
    assert_eq!(H24.items(12), "12 Objekte");
    assert_eq!(H24.decimal(1234.5, 1), "1.234,5");

    // A chosen number style wins over the language's convention.
    let point = Formats::with(RegionFormats {
        number: Some(NumberStyle::DecimalPoint),
        ..RegionFormats::default()
    });
    assert_eq!(point.decimal(1234.5, 1), "1,234.5");
    speak("en-US");
    let comma = Formats::with(RegionFormats {
        number: Some(NumberStyle::DecimalComma),
        ..RegionFormats::default()
    });
    assert_eq!(comma.count(1_234_567), "1.234.567");
    assert_eq!(comma.size(1_500_000), "1,5 MB");
}

#[test]
fn money_reads_whole_amounts_without_cents() {
    speak("en-US");
    assert_eq!(H24.money(0), "$0");
    assert_eq!(H24.money(4_999), "$0", "less than a cent reads as none");
    assert_eq!(H24.money(420_000), "$0.42");
    assert_eq!(H24.money(1_500_000), "$1.50");
    assert_eq!(H24.money(5_000_000), "$5");
    assert_eq!(H24.money(1_250_000_000), "$1,250");
    speak("de-DE");
    assert_eq!(H24.money(420_000), "$0,42");
    assert_eq!(H24.money(1_250_000_000), "$1.250");
}

#[test]
fn an_amount_typed_back_reads_as_the_money_written() {
    assert_eq!(super::parse_money("5"), Some(5_000_000));
    assert_eq!(super::parse_money(" 12.5 "), Some(12_500_000));
    assert_eq!(super::parse_money("0.50"), Some(500_000));
    assert_eq!(super::parse_money("$5"), Some(5_000_000));
    assert_eq!(super::parse_money("0"), Some(0));
    for none in ["", "twelve", "5.", ".50", "5.123", "1,250", "-5", "5x"] {
        assert_eq!(super::parse_money(none), None, "{none:?} names no amount");
    }
}

#[test]
fn relative_times_bucket_at_their_boundaries() {
    speak("en-US");
    for (secs, want) in [
        (0, "just now"),
        (-10, "just now"),
        (44, "just now"),
        (45, "1 min ago"),
        (180, "3 min ago"),
        (60 * 60, "1 hr ago"),
        (90_000, "yesterday"),
        (3 * 86_400, "3 days ago"),
        (8 * 86_400, "1 week ago"),
        (31 * 86_400, "1 month ago"),
        (800 * 86_400, "2 years ago"),
    ] {
        assert_eq!(relative(secs), want, "relative({secs})");
    }
    for (secs, want) in [
        (-10, "now"),
        (59, "now"),
        (60, "1m"),
        (3600, "1h"),
        (86_400, "1d"),
        (7 * 86_400, "1w"),
        (30 * 86_400, "1mo"),
        (365 * 86_400, "1y"),
    ] {
        assert_eq!(relative_compact(secs), want, "relative_compact({secs})");
    }
    for (n, want) in [
        (999, "999"),
        (1_000, "1K"),
        (12_300, "12.3K"),
        (999_999, "1M"),
        (4_100_000, "4.1M"),
        (1_100_000_000, "1.1B"),
    ] {
        assert_eq!(compact_count(n), want, "compact_count({n})");
    }
    speak("de-DE");
    assert_eq!(relative(180), "vor 3 Min.");
    assert_eq!(compact_count(12_300), "12,3 Tsd.");
}

/// A Region & Formats change reaches every formatter and moves the
/// revision views re-render on, without a restart.
#[test]
fn a_formats_change_applies_live_and_moves_the_i18n_revision() {
    speak("en-US");
    region::install(RegionFormats {
        time: TimeStyle::H24,
        ..RegionFormats::default()
    });
    let before = I18nManager::revision();
    let time = chrono::NaiveTime::from_hms_opt(16, 2, 0).unwrap();
    assert_eq!(super::time(&time), "16:02");
    assert!(region::install(RegionFormats {
        time: TimeStyle::H12,
        ..RegionFormats::default()
    }));
    assert_ne!(I18nManager::revision(), before);
    assert_eq!(super::time(&time), "4:02 PM");
    assert_eq!(crate::clock::time_of_day(&time), "4:02 PM");
    assert!(!region::install(RegionFormats {
        time: TimeStyle::H12,
        ..RegionFormats::default()
    }));
    region::install(RegionFormats::default());
}

#[test]
fn the_locale_section_reads_key_by_key() {
    let section = serde_json::json!({
        "date_format": "Long",
        "time_format": "H12",
        "first_day_of_week": "Sunday",
        "number_style": "nonsense",
    });
    let section = section.as_object().cloned().unwrap();
    let read = RegionFormats::from_section(section);
    assert_eq!(read.date, DateStyle::Long);
    assert_eq!(read.time, TimeStyle::H12);
    assert_eq!(read.first_day, FirstDayOfWeek::Sunday);
    assert_eq!(read.number, None);
    assert_eq!(
        RegionFormats::from_section(serde_json::Map::new()),
        RegionFormats::default()
    );
    for (stored, want) in [("ISO", DateStyle::Iso), ("Language", DateStyle::Language)] {
        let section = serde_json::json!({ "date_format": stored });
        let read = RegionFormats::from_section(section.as_object().cloned().unwrap());
        assert_eq!(read.date, want, "{stored}");
    }
}

/// The day fact across its boundaries: a day gone, today, tomorrow, the week ahead, the rest of
/// the year and another year, in both languages.
#[test]
fn the_day_fact_names_a_day_by_how_far_it_is_in_english_and_german() {
    let today = day(2026, 6, 30); // a Tuesday
    speak("en-US");
    for (when, english, _) in [
        (
            day(2026, 6, 20),
            "Overdue since Jun 20",
            "Überfällig seit 20. Juni",
        ),
        (day(2026, 6, 30), "Today", "Heute"),
        (day(2026, 7, 1), "Tomorrow", "Morgen"),
        (day(2026, 7, 2), "Thursday", "Donnerstag"),
        (day(2026, 7, 6), "Monday", "Montag"),
        (day(2026, 7, 7), "Jul 7", "7. Juli"),
        (day(2026, 12, 31), "Dec 31", "31. Dez."),
        (day(2027, 1, 2), "Jan 2, 2027", "2. Jan. 2027"),
    ] {
        assert_eq!(H24.day_fact(when, None, today).text, english, "{when:?}");
    }
    speak("de-DE");
    for (when, _, german) in [
        (
            day(2026, 6, 20),
            "Overdue since Jun 20",
            "Überfällig seit 20. Juni",
        ),
        (day(2026, 6, 30), "Today", "Heute"),
        (day(2026, 7, 1), "Tomorrow", "Morgen"),
        (day(2026, 7, 2), "Thursday", "Donnerstag"),
        (day(2026, 7, 6), "Monday", "Montag"),
        (day(2026, 7, 7), "Jul 7", "7. Juli"),
        (day(2026, 12, 31), "Dec 31", "31. Dez."),
        (day(2027, 1, 2), "Jan 2, 2027", "2. Jan. 2027"),
    ] {
        assert_eq!(H24.day_fact(when, None, today).text, german, "{when:?}");
    }
}

/// A day gone by is the only day read in the overdue tone, and it is read so across midnight,
/// the end of the week and the end of the year, saying which day it was.
#[test]
fn the_day_fact_is_overdue_through_midnight_and_the_years_end() {
    let midnight = || chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap();
    speak("en-US");
    let fact = H24.day_fact(day(2026, 6, 29), Some(midnight()), day(2026, 6, 30));
    assert_eq!(fact.text, "Overdue since Jun 29 00:00");
    assert_eq!(fact.tone, crate::widget::Tone::Danger);
    let fact = H24.day_fact(day(2026, 6, 30), Some(midnight()), day(2026, 6, 30));
    assert_eq!(fact.text, "Today 00:00");
    assert_eq!(fact.tone, crate::widget::Tone::Neutral);
    let fact = H24.day_fact(day(2027, 1, 1), None, day(2026, 12, 31));
    assert_eq!(fact.text, "Tomorrow");
    assert_eq!(fact.tone, crate::widget::Tone::Neutral);
    assert_eq!(
        H24.day_fact(day(2026, 12, 31), None, day(2027, 1, 1)).text,
        "Overdue since Dec 31, 2026"
    );
}

/// A moment told beside words that name its day reads as the time of day, the short weekday
/// before it when the day is not today, and the date further out: the fact a choice shows at its
/// trailing edge.
#[test]
fn the_moment_fact_tells_the_time_beside_the_words_that_name_its_day() {
    let today = day(2026, 6, 30); // a Tuesday
    speak("en-US");
    for (when, english, german) in [
        (at(2026, 6, 30, 15, 0), "15:00", "15:00"),
        (at(2026, 7, 1, 9, 0), "Wed 09:00", "Mi. 09:00"),
        (at(2026, 7, 6, 9, 0), "Mon 09:00", "Mo. 09:00"),
        (at(2026, 7, 7, 9, 0), "Jul 7 09:00", "7. Juli 09:00"),
        (at(2026, 12, 31, 9, 0), "Dec 31 09:00", "31. Dez. 09:00"),
        (
            at(2027, 1, 2, 9, 0),
            "Jan 2, 2027 09:00",
            "2. Jan. 2027 09:00",
        ),
    ] {
        assert_eq!(H24.moment_fact(when, today), english, "{when:?}");
        speak("de-DE");
        assert_eq!(H24.moment_fact(when, today), german, "{when:?}");
        speak("en-US");
    }
}

/// A day something was done on reads as the day it happened: today, yesterday, the weekday
/// within the week gone by, the month and day of this year and the date of another. It is
/// history, so no day of it reads as overdue.
#[test]
fn a_done_day_reads_as_the_day_it_happened_in_english_and_german() {
    let today = day(2026, 6, 30); // a Tuesday
    speak("en-US");
    for (when, english, _) in [
        (day(2026, 6, 30), "Today", "Heute"),
        (day(2026, 6, 29), "Yesterday", "Gestern"),
        (day(2026, 6, 24), "Wednesday", "Mittwoch"),
        (day(2026, 6, 20), "Jun 20", "20. Juni"),
        (day(2026, 1, 5), "Jan 5", "5. Jan."),
        (day(2025, 12, 31), "Dec 31, 2025", "31. Dez. 2025"),
    ] {
        assert_eq!(H24.day_done(when, today), english, "{when:?}");
    }
    speak("de-DE");
    for (when, _, german) in [
        (day(2026, 6, 30), "Today", "Heute"),
        (day(2026, 6, 29), "Yesterday", "Gestern"),
        (day(2026, 6, 24), "Wednesday", "Mittwoch"),
        (day(2026, 6, 20), "Jun 20", "20. Juni"),
        (day(2026, 1, 5), "Jan 5", "5. Jan."),
        (day(2025, 12, 31), "Dec 31, 2025", "31. Dez. 2025"),
    ] {
        assert_eq!(H24.day_done(when, today), german, "{when:?}");
    }
}

/// A day and the time of it are one fact: today's time stands with "Today", the week ahead's
/// with its weekday, and a day gone by says its date under the overdue tone.
#[test]
fn a_day_and_its_time_read_as_one_fact_in_english_and_german() {
    let today = day(2026, 6, 30);
    let cases = [
        (
            day(2026, 6, 30),
            (9, 0),
            "Today 09:00",
            "Heute 09:00",
            Tone::Neutral,
        ),
        (
            day(2026, 7, 1),
            (9, 0),
            "Tomorrow 09:00",
            "Morgen 09:00",
            Tone::Neutral,
        ),
        (
            day(2026, 7, 2),
            (14, 30),
            "Thu 14:30",
            "Do. 14:30",
            Tone::Neutral,
        ),
        (
            day(2026, 7, 8),
            (9, 0),
            "Jul 8 09:00",
            "8. Juli 09:00",
            Tone::Neutral,
        ),
        (
            day(2025, 6, 29),
            (9, 0),
            "Overdue since Jun 29, 2025 09:00",
            "Überfällig seit 29. Juni 2025 09:00",
            Tone::Danger,
        ),
        (
            day(2026, 6, 29),
            (14, 30),
            "Overdue since Jun 29 14:30",
            "Überfällig seit 29. Juni 14:30",
            Tone::Danger,
        ),
        (
            day(2026, 6, 25),
            (7, 5),
            "Overdue since Jun 25 07:05",
            "Überfällig seit 25. Juni 07:05",
            Tone::Danger,
        ),
        (
            day(2026, 6, 10),
            (7, 5),
            "Overdue since Jun 10 07:05",
            "Überfällig seit 10. Juni 07:05",
            Tone::Danger,
        ),
    ];
    for (when, (h, m), english, _, tone) in cases {
        speak("en-US");
        let time = chrono::NaiveTime::from_hms_opt(h, m, 0).unwrap();
        let fact = H24.day_fact(when, Some(time), today);
        assert_eq!(fact.text, english, "{when:?}");
        assert_eq!(fact.tone, tone, "{when:?}");
    }
    for (when, (h, m), _, german, _) in cases {
        speak("de-DE");
        let time = chrono::NaiveTime::from_hms_opt(h, m, 0).unwrap();
        assert_eq!(
            H24.day_fact(when, Some(time), today).text,
            german,
            "{when:?}"
        );
    }
}

#[test]
fn a_list_joins_with_its_languages_own_separator() {
    let facts = |items: &[&str]| {
        items
            .iter()
            .map(|item| String::from(*item))
            .collect::<Vec<_>>()
    };
    for locale in ["en-US", "de-DE"] {
        speak(locale);
        assert_eq!(list(&facts(&["Home folders"])), "Home folders", "{locale}");
        assert_eq!(
            list(&facts(&["Home folders", "encrypted"])),
            "Home folders, encrypted",
            "{locale}"
        );
        assert_eq!(
            list(&facts(&["Home folders", "encrypted", "134 GB free"])),
            "Home folders, encrypted, 134 GB free",
            "{locale}"
        );
        assert_eq!(list(&[]), "", "{locale}");
    }
}
