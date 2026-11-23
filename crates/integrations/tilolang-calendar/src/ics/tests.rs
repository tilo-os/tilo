// Calendar arithmetic is bounded by the values a calendar holds: a time of
// day, a duration within one, a count of occurrences. Panic-producing lints
// stay denied.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    reason = "bounded date, time and duration arithmetic on calendar values"
)]
use super::*;

tilo_test_env::scenario! {
    /// Import: what an iCalendar file says becomes the events it names, at their instants.
    fn ics_files_import_at_the_instants_they_name;

    phase duration_events_keep_their_full_span;
    {
        for (start, duration, expected) in [
            ("DTSTART:20260615T090000", "PT1H30M", "2026-06-15T10:30:00"),
            (
                "DTSTART:20260615T090000Z",
                "+PT45M15S",
                "2026-06-15T09:45:15",
            ),
            ("DTSTART;VALUE=DATE:20260615", "P3D", "2026-06-18T00:00:00"),
            ("DTSTART;VALUE=DATE:20260615", "P2W", "2026-06-29T00:00:00"),
            (
                "DTSTART;TZID=Europe/Berlin:20260328T120000",
                "P1D",
                "2026-03-29T12:00:00",
            ),
            (
                "DTSTART;TZID=Europe/Berlin:20260328T120000",
                "PT24H",
                "2026-03-29T13:00:00",
            ),
        ] {
            let payload = format!(
                "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nSUMMARY:Visit\r\n{start}\r\nDURATION:{duration}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            );
            let (events, _) = parse(&payload);
            assert_eq!(events.len(), 1, "{duration}");
            assert_eq!(
                events[0].end,
                expected.parse::<DateTime>().unwrap(),
                "{duration}"
            );
            let model = Calendar {
                events: events.into(),
                ..Calendar::default()
            };
            let (round_trip, _) = parse(&export(&model));
            assert_eq!(round_trip[0].end, model.events.first().unwrap().end);
        }
        for duration in [
            "P",
            "PT",
            "-PT1H",
            "P1M",
            "PT1H1H",
            "PT1M1H",
            "P1DT",
            "PT999999999999999999H",
            "P9999999999999999W",
        ] {
            let payload = format!(
                "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nDTSTART:20260615T090000\r\nDURATION:{duration}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            );
            assert!(parse(&payload).0.is_empty(), "{duration}");
        }
    }

    phase external_uids_preserve_detached_occurrences_and_import_identity;
    {
        let payload = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:weekly-meeting@example.com\r\nSUMMARY:Meeting\r\nDTSTART:20260615T090000\r\nDTEND:20260615T100000\r\nRRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:weekly-meeting@example.com\r\nRECURRENCE-ID:20260622T090000\r\nSTATUS:CANCELLED\r\nEND:VEVENT\r\nBEGIN:VTODO\r\nUID:follow-up@example.com\r\nSUMMARY:Follow up\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
        let (events, tasks) = parse(payload);
        let (again, tasks_again) = parse(payload);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, again[0].id);
        assert_eq!(tasks[0].id, tasks_again[0].id);
        assert_ne!(events[0].id, tasks[0].id);
        assert_eq!(events[0].occurrence_overrides.len(), 1);
        let from = Date::new(2026, 6, 15).unwrap();
        let cancelled = Date::new(2026, 6, 22).unwrap();
        assert_eq!(events[0].occurrences_in(from, cancelled), vec![from]);
    }

    phase imported_alarms_keep_their_time_before_the_start;
    {
        let reminder = |trigger: &str| {
            let payload = format!(
                "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:alarm@example.com\r\n\
             DTSTART:20261020T090000\r\nDTEND:20261020T100000\r\nSUMMARY:Dentist\r\n\
             BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:{trigger}\r\nEND:VALARM\r\n\
             END:VEVENT\r\nEND:VCALENDAR\r\n"
            );
            parse(&payload).0[0].reminder_min
        };
        assert_eq!(reminder("-PT15M"), Some(15));
        assert_eq!(reminder("-PT1H"), Some(60));
        assert_eq!(reminder("-P1D"), Some(1440));
        assert_eq!(reminder("PT0S"), Some(0));
        assert_eq!(reminder("-PT1H30M"), Some(90));
        assert_eq!(reminder("PT15M"), None);
    }

    phase date_time_series_ends_exclusions_and_due_times_import;
    {
        let date = |day| Date::new(2026, 10, day).expect("valid date");
        let weekly = |until: &str, exdate: &str| {
            let payload = format!(
                "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:standup@example.com\r\n\
             RRULE:FREQ=WEEKLY;UNTIL={until}\r\n\
             DTSTART;TZID=Europe/Berlin:20261005T090000\r\n\
             DTEND;TZID=Europe/Berlin:20261005T093000\r\n\
             {exdate}\r\nSUMMARY:Standup\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            );
            let (events, _) = parse(&payload);
            events[0].occurrences_in(date(1), date(31) + jiff::Span::new().days(90))
        };
        assert_eq!(
            weekly(
                "20261019T070000Z",
                "EXDATE;TZID=Europe/Berlin:20261012T090000"
            ),
            vec![date(5), date(19)]
        );
        assert_eq!(
            weekly("20261026T075959Z", "EXDATE:20261005T070000Z"),
            vec![date(12), date(19)]
        );
        assert_eq!(
            weekly("20261026", "EXDATE;VALUE=DATE:20261019"),
            vec![date(5), date(12), date(26)]
        );

        let (_, tasks) = parse(
            "BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\nUID:ship@example.com\r\nSUMMARY:Ship\r\n\
         DUE;TZID=Europe/Berlin:20261015T170000\r\nEND:VTODO\r\n\
         BEGIN:VTODO\r\nUID:file@example.com\r\nSUMMARY:File\r\n\
         DUE:20261016T120000Z\r\nEND:VTODO\r\nEND:VCALENDAR\r\n",
        );
        assert_eq!(
            tasks.iter().map(|task| task.due).collect::<Vec<_>>(),
            vec![Some(date(15)), Some(date(16))]
        );
    }

    phase a_zoned_series_imports_to_the_days_rfc_5545_gives_it;
    {
        let at = |date: &str, time: &str| -> Vec<DateTime> {
            date.split(',')
                .map(|day| format!("{day}T{time}").parse().expect("valid date"))
                .collect()
        };
        let series = [
            (
                "DTSTART;TZID=Europe/Berlin:20261014T090000\n\
         RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR;INTERVAL=2;COUNT=12\n\
         EXDATE;TZID=Europe/Berlin:20261016T090000",
                at(
                    "2026-10-14,2026-10-15,2026-10-26,2026-10-27,2026-10-28,2026-10-29,\
                     2026-10-30,2026-11-09,2026-11-10,2026-11-11,2026-11-12",
                    "09:00",
                ),
            ),
            (
                "DTSTART;TZID=Europe/Berlin:20261014T090000\n\
         RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR;INTERVAL=2;UNTIL=20261110T080000Z\n\
         EXDATE:20261027T080000Z",
                at(
                    "2026-10-14,2026-10-15,2026-10-16,2026-10-26,2026-10-28,2026-10-29,\
                     2026-10-30,2026-11-09,2026-11-10",
                    "09:00",
                ),
            ),
            (
                "DTSTART;TZID=America/New_York:20260131T183000\n\
         RRULE:FREQ=MONTHLY;COUNT=6",
                at(
                    "2026-01-31,2026-03-31,2026-05-31,2026-07-31,2026-08-31,2026-10-31",
                    "18:30",
                ),
            ),
            (
                "DTSTART;TZID=America/New_York:20240229T070000\n\
         RRULE:FREQ=YEARLY;INTERVAL=1;UNTIL=20320301T000000Z",
                at("2024-02-29,2028-02-29,2032-02-29", "07:00"),
            ),
        ];
        let (from, to) = (
            Date::new(2024, 1, 1).expect("valid date"),
            Date::new(2033, 1, 1).expect("valid date"),
        );
        for (lines, expected) in series {
            let payload = format!(
                "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:series@example.com\r\nSUMMARY:Series\r\n{}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
                lines.replace('\n', "\r\n")
            );
            let (events, _) = parse(&payload);
            let event = &events[0];
            let imported: Vec<DateTime> = event
                .occurrences_in(from, to)
                .into_iter()
                .map(|date| date.to_datetime(event.start.time()))
                .collect();
            assert_eq!(imported, expected, "{lines}");
        }
    }

    phase outlook_invites_import_at_their_instant;
    {
        let at_ten = "2026-11-03T09:00:00Z".to_string();
        for zone in [String::new(), outlook_zone("W. Europe Standard Time")] {
            let payload = outlook_invite(
                &zone,
                "W. Europe Standard Time",
                "20261103T100000",
                "20261103T110000",
            );
            assert_eq!(
                imported_at(&payload),
                ("Europe/Berlin".to_string(), at_ten.clone())
            );
            let parsed = parse_calendar(&payload);
            assert_eq!(parsed.events[0].timezone, "Europe/Berlin");
            assert_eq!(parsed.events[0].reminder_min, Some(15));
            assert!(parsed.unknown_zones.is_empty());
        }
        assert_eq!(
            imported_at(&outlook_invite(
                "",
                "Pacific Standard Time",
                "20260710T090000",
                "20260710T100000"
            )),
            (
                "America/Los_Angeles".to_string(),
                "2026-07-10T16:00:00Z".to_string()
            )
        );
        // A zone Outlook names after its cities, or a customised one, is
        // known only by its VTIMEZONE's rules: the zone that keeps them, and
        // one of the cities when it names one.
        let cities = "(UTC+01:00) Amsterdam\\, Berlin\\, Bern\\, Rome\\, Stockholm\\, Vienna";
        let quoted = "\"(UTC+01:00) Amsterdam, Berlin, Bern, Rome, Stockholm, Vienna\"";
        assert_eq!(
            imported_at(&outlook_invite(
                &outlook_zone(cities),
                quoted,
                "20261103T100000",
                "20261103T110000"
            )),
            ("Europe/Berlin".to_string(), at_ten)
        );
        let (zone, instant) = imported_at(&outlook_invite(
            &outlook_zone("Customized Time Zone"),
            "Customized Time Zone",
            "20260710T100000",
            "20260710T110000",
        ));
        assert_eq!(instant, "2026-07-10T08:00:00Z");
        let winter = Date::new(2026, 1, 15).unwrap()
            .at(12, 0, 0, 0);
        assert_eq!(
            crate::timezone::wall_to_utc(&zone, winter)
                .unwrap()
                .0
                .to_string(),
            "2026-01-15T11:00:00Z",
            "{zone} keeps Central European winter time"
        );
        let sydney = "BEGIN:VTIMEZONE\r\nTZID:Customized Time Zone 1\r\n\
         BEGIN:STANDARD\r\nDTSTART:16010101T030000\r\nTZOFFSETFROM:+1100\r\n\
         TZOFFSETTO:+1000\r\nRRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=1SU;BYMONTH=4\r\n\
         END:STANDARD\r\nBEGIN:DAYLIGHT\r\nDTSTART:16010101T020000\r\n\
         TZOFFSETFROM:+1000\r\nTZOFFSETTO:+1100\r\n\
         RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=1SU;BYMONTH=10\r\nEND:DAYLIGHT\r\n\
         END:VTIMEZONE\r\n";
        assert_eq!(
            imported_at(&outlook_invite(
                sydney,
                "Customized Time Zone 1",
                "20261201T090000",
                "20261201T100000"
            )),
            (
                "Australia/Sydney".to_string(),
                "2026-11-30T22:00:00Z".to_string()
            )
        );
        let located = "BEGIN:VTIMEZONE\r\nTZID:/citadel.org/20250101_1/Custom\r\n\
         X-LIC-LOCATION:America/New_York\r\nEND:VTIMEZONE\r\n";
        assert_eq!(
            imported_at(&outlook_invite(
                located,
                "/citadel.org/20250101_1/Custom",
                "20260710T090000",
                "20260710T100000"
            ))
            .0,
            "America/New_York"
        );
    }

    phase a_customised_zone_is_the_one_changing_clocks_at_its_instants;
    {
        let eastern = "BEGIN:VTIMEZONE\r\nTZID:Customized Time Zone\r\n\
         BEGIN:STANDARD\r\nDTSTART:16010101T020000\r\nTZOFFSETFROM:-0400\r\n\
         TZOFFSETTO:-0500\r\nRRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=1SU;BYMONTH=11\r\n\
         END:STANDARD\r\nBEGIN:DAYLIGHT\r\nDTSTART:16010101T020000\r\n\
         TZOFFSETFROM:-0500\r\nTZOFFSETTO:-0400\r\n\
         RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=2SU;BYMONTH=3\r\nEND:DAYLIGHT\r\n\
         END:VTIMEZONE\r\n";
        assert_eq!(
            imported_at(&outlook_invite(
                eastern,
                "Customized Time Zone",
                "20260710T090000",
                "20260710T100000"
            )),
            (
                "America/New_York".to_string(),
                "2026-07-10T13:00:00Z".to_string()
            )
        );
    }

    phase an_unknown_zone_keeps_the_time_as_written;
    {
        let no_fit = "BEGIN:VTIMEZONE\r\nTZID:Lunar Standard Time\r\nBEGIN:STANDARD\r\n\
         DTSTART:16010101T000000\r\nTZOFFSETFROM:+0123\r\nTZOFFSETTO:+0123\r\n\
         END:STANDARD\r\nEND:VTIMEZONE\r\n";
        for (zone, tzid) in [
            ("", "Olympus Mons Standard Time"),
            (no_fit, "Lunar Standard Time"),
        ] {
            let payload = outlook_invite(zone, tzid, "20261103T100000", "20261103T110000");
            let parsed = parse_calendar(&payload);
            assert_eq!(parsed.unknown_zones, vec![tzid.to_string()]);
            let event = &parsed.events[0];
            assert_eq!(event.timezone, "floating");
            assert_eq!(event.start.to_string(), "2026-11-03T10:00:00");
            assert_eq!(event.end.to_string(), "2026-11-03T11:00:00");
        }
    }

    phase an_end_in_another_zone_is_the_same_instant;
    {
        let payload = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:flight@example.com\r\n\
         SUMMARY:Flight\r\nDTSTART;TZID=W. Europe Standard Time:20260710T100000\r\n\
         DTEND;TZID=Eastern Standard Time:20260710T123000\r\nEND:VEVENT\r\n\
         END:VCALENDAR\r\n";
        let (events, _) = parse(payload);
        assert_eq!(events[0].timezone, "Europe/Berlin");
        assert_eq!(events[0].end.to_string(), "2026-07-10T18:30:00");
        assert_eq!(
            events[0].end_instant().unwrap().to_string(),
            "2026-07-10T16:30:00Z"
        );
    }
}

tilo_test_env::scenario! {
    /// Export: the file a calendar is written as, and what reading it back gives.
    fn calendars_export_as_rfc_5545_and_read_back;

    phase an_events_notes_are_exported_as_its_description;
    {
        let mut model = crate::model::seed_demo(Date::new(2026, 6, 15).unwrap());
        let id = model.events.iter().next().unwrap().id;
        model.notes.insert(
            id.to_string(),
            tilo_document::RichText::from_markdown("Bring the Q3 numbers"),
        );
        let ics = export(&model);
        assert!(
            ics.contains("DESCRIPTION:Bring the Q3 numbers\r\n"),
            "{ics}"
        );
    }

    phase an_entrys_notes_are_read_from_its_text;
    {
        let entry = |more: &str| {
            format!(
                "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:review-1@example.test\r\n\
                 SUMMARY:Review\r\nDTSTART:20261118T140000Z\r\nDTEND:20261118T143000Z\r\n\
                 {more}END:VEVENT\r\nEND:VCALENDAR\r\n"
            )
        };
        // Its own words, with what the standard writes for a comma and for a line's end.
        assert_eq!(
            notes(&entry(
                "DESCRIPTION:Bring the numbers\\, all of them.\\nFourth floor\r\n"
            )),
            "Bring the numbers, all of them.\nFourth floor"
        );
        // A long line, folded as a server folds one.
        assert_eq!(
            notes(&entry("DESCRIPTION:Bring the num\r\n bers\r\n")),
            "Bring the numbers"
        );
        // A reminder's words are not the entry's, with notes beside them and without.
        let reminder = "BEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:The review is soon\r\n\
                        TRIGGER:-PT15M\r\nEND:VALARM\r\n";
        assert_eq!(
            notes(&entry(&format!(
                "DESCRIPTION:Bring the numbers\r\n{reminder}"
            ))),
            "Bring the numbers"
        );
        assert_eq!(notes(&entry(reminder)), "");
        assert_eq!(notes(&entry("")), "", "an entry without notes");
        // Of an entry that repeats: the notes of the series, wherever a changed
        // occurrence stands in the text.
        let series = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:weekly-1@example.test\r\n\
                      RECURRENCE-ID:20261125T140000Z\r\nSUMMARY:Review\r\nDTSTART:20261125T150000Z\r\n\
                      DESCRIPTION:Moved this once\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\n\
                      UID:weekly-1@example.test\r\nSUMMARY:Review\r\nDTSTART:20261118T140000Z\r\n\
                      RRULE:FREQ=WEEKLY\r\nDESCRIPTION:Every week\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        assert_eq!(notes(series), "Every week");
    }

    phase export_envelope_and_event_task_all_day_round_trips;
    {
        {
            let model = crate::model::seed_demo(Date::new(2026, 6, 15).unwrap());
            let ics = export(&model);
            assert!(ics.starts_with("BEGIN:VCALENDAR\r\n"));
            assert!(ics.contains("BEGIN:VEVENT"));
            assert!(ics.contains("BEGIN:VTODO"));
            assert!(ics.trim_end().ends_with("END:VCALENDAR"));
        }
        {
            let model = crate::model::seed_demo(Date::new(2026, 6, 15).unwrap());
            let ics = export(&model);
            let (events, tasks) = parse(&ics);
            assert_eq!(events.len(), model.events.len());
            assert_eq!(tasks.len(), model.tasks.len());

            let standup = events.iter().find(|e| e.title == "Team standup").unwrap();
            assert_eq!(standup.recurrence, Recurrence::Weekdays);
            assert_eq!(standup.reminder_min, Some(10));
            assert_eq!(standup.start.hour(), 9);
            assert_eq!(standup.start.minute(), 30);

            let offsite = events
                .iter()
                .find(|e| e.title == "Company offsite")
                .unwrap();
            assert!(offsite.all_day);
        }
        {
            let start = Date::new(2026, 6, 15).unwrap();
            let model = Calendar {
                projects: Keyed::default(),
                tasks: Keyed::default(),
                events: vec![Event {
                    id: Uuid::new_v4(),
                    title: "Conference".into(),
                    start: start.to_datetime(Time::MIN),
                    end: (start + jiff::Span::new().days(3)).to_datetime(Time::MIN),
                    all_day: true,
                    ..Event::default()
                }]
                .into(),
                ..Calendar::default()
            };
            let calendar = export(&model);
            assert!(calendar.contains("DTSTART;VALUE=DATE:20260615"));
            assert!(calendar.contains("DTEND;VALUE=DATE:20260618"));

            let (events, _) = parse(&calendar);
            let event = events.first().expect("round-tripped event");
            assert!(event.all_day);
            assert!(event.occurs_on(start + jiff::Span::new().days(2)));
            assert!(!event.occurs_on(start + jiff::Span::new().days(3)));
        }
        {
            let model = crate::model::seed_demo(Date::new(2026, 6, 15).unwrap());
            let (_, tasks) = parse(&export(&model));
            assert!(tasks.iter().any(|t| t.done));
            assert!(tasks.iter().any(|t| t.priority == Priority::High));
            let mut expected: Vec<_> = model.tasks.iter().map(|task| task.id).collect();
            let mut actual: Vec<_> = tasks.iter().map(|task| task.id).collect();
            expected.sort_unstable();
            actual.sort_unstable();
            assert_eq!(actual, expected, "VTODO UIDs retain task identities");
        }
    }

    phase timezones_recurrence_and_invite_identity_round_trip_deterministically;
    {
        {
            let mut model =
                crate::model::seed_demo(Date::new(2026, 6, 15).expect("valid date"));
            let event = model
                .events
                .iter_mut()
                .find(|event| event.title == "Design review")
                .expect("recurring demo event");
            let guest = Attendee {
                id: Uuid::new_v4(),
                name: "Sam Example".into(),
                email: "sam@example.com".into(),
                status: InviteStatus::Tentative,
            };
            event.timezone = "Europe/Berlin".into();
            event.organizer = Some(guest.id);
            event.attendees.push(guest.clone());
            event.recurrence_interval = 2;
            event.recurrence_until = Date::new(2026, 12, 31).ok();
            event.recurrence_count = Some(8);
            event.recurrence_exdates = [Date::new(2026, 6, 17).expect("valid date")]
                .into_iter()
                .collect();
            event.occurrence_overrides.clear();
            event
                .occurrence_overrides
                .push(crate::model::OccurrenceOverride {
                    original_date: Date::new(2026, 6, 19).expect("valid date"),
                    original_time: None,
                    cancelled: true,
                    start: None,
                    end: None,
                    end_later_fold: false,
                    title: None,
                    location: None,
                });
            let event_id = event.id;
            let expected_attendees = event.attendees.clone();

            let exported = export(&model);
            assert!(exported.contains("INTERVAL=2"), "{exported}");
            let uid = format!("UID:{event_id}@tilo");
            let block = exported
                .split("BEGIN:VEVENT")
                .find(|block| block.contains(&uid) && block.contains("RRULE:"))
                .expect("series component");
            assert!(block.contains("INTERVAL=2"), "{block}");
            let (round_tripped, _) = parse(&exported);
            let event = round_tripped
                .iter()
                .find(|candidate| candidate.id == event_id)
                .expect("stable UID round trips");
            assert_eq!(event.timezone, "Europe/Berlin");
            assert_eq!(event.organizer, Some(guest.id));
            let mut attendees = event.attendees.clone();
            attendees.sort_by(|left, right| left.email.cmp(&right.email));
            let mut expected_attendees = expected_attendees;
            expected_attendees.sort_by(|left, right| left.email.cmp(&right.email));
            assert_eq!(attendees, expected_attendees);
            assert_eq!(event.recurrence_interval, 2);
            assert_eq!(event.recurrence_count, Some(8));
            assert_eq!(
                event.recurrence_until,
                Date::new(2026, 12, 31).ok()
            );
            assert_eq!(event.recurrence_exdates.len(), 1);
            assert_eq!(event.occurrence_overrides.len(), 1);
            assert!(event.occurrence_overrides[0].cancelled);
        }
        {
            let payload = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:11111111-1111-1111-1111-111111111111@example\r\nDTSTART;TZID=Europe/Berlin:20261020T090000\r\nDTEND;TZID=Europe/Berlin:20261020T100000\r\nRRULE:FREQ=WEEKLY\r\nATTENDEE;CN=Sam;PARTSTAT=NEEDS-ACTION:mailto:Sam@Example.COM\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:11111111-1111-1111-1111-111111111111@example\r\nRECURRENCE-ID;TZID=Europe/Berlin:20261027T090000\r\nDTSTART;TZID=Europe/Berlin:20261027T110000\r\nDTEND;TZID=Europe/Berlin:20261027T120000\r\nSUMMARY:Moved\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
            let (first, _) = parse(payload);
            let (second, _) = parse(payload);
            assert_eq!(first[0].attendees[0].id, second[0].attendees[0].id);
            let edit = &first[0].occurrence_overrides[0];
            assert_eq!(
                edit.original_date,
                Date::new(2026, 10, 27).unwrap()
            );
            assert_eq!(edit.original_time, Time::new(9, 0, 0, 0).ok());

            let exported = export(&Calendar {
                events: first.into(),
                tasks: Keyed::default(),
                projects: Keyed::default(),
                ..Calendar::default()
            });
            assert!(exported.contains("RECURRENCE-ID;TZID=Europe/Berlin:20261027T090000"));
            let (round_tripped, _) = parse(&exported);
            assert_eq!(
                round_tripped[0].occurrence_overrides[0].original_time,
                Time::new(9, 0, 0, 0).ok()
            );
        }
        {
            let payload = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:22222222-2222-2222-2222-222222222222@example\r\nDTSTART;TZID=Europe/Berlin:20260329T023000\r\nDTEND;TZID=Europe/Berlin:20260329T033000\r\nSUMMARY:DST gap\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
            let (events, _) = parse(payload);
            assert_eq!(
                events[0].start.time(),
                Time::new(3, 0, 0, 0).unwrap()
            );
            assert_eq!(
                events[0].end.time(),
                Time::new(3, 30, 0, 0).unwrap()
            );
        }
        {
            let payload = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:bad@example\r\nSUMMARY:Broken\r\nDTSTART:not-a-date\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
            assert!(parse(payload).0.is_empty());
        }
    }

    phase content_lines_fold_escape_and_quote_per_rfc_5545;
    {
        {
            let payload = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nSUMMARY:Hello Wo\r\n rld\\, again\\; line1\\nline2\r\nDTSTART:20260615T090000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
            let (events, _) = parse(payload);
            assert_eq!(events[0].title, "Hello World, again; line1\nline2");
        }
        {
            let mut model =
                crate::model::seed_demo(Date::new(2026, 6, 15).expect("valid date"));
            let event = model.events.first_mut().expect("demo event");
            let guest = Attendee {
                id: Uuid::new_v4(),
                name: "Doe, Jane; PhD: Ops".into(),
                email: "jane@example.com".into(),
                status: InviteStatus::Accepted,
            };
            event.attendees = vec![guest.clone()].into();
            let exported = export(&model);
            assert!(
                exported.contains("CN=\"Doe, Jane; PhD: Ops\":mailto:jane@example.com")
                    || pim_lines::unfold(&exported)
                        .iter()
                        .any(|line| line
                            .contains("CN=\"Doe, Jane; PhD: Ops\":mailto:jane@example.com")),
                "{exported}"
            );
            let (events, _) = parse(&exported);
            let attendee = events
                .iter()
                .flat_map(|event| event.attendees.iter())
                .find(|attendee| attendee.id == guest.id)
                .expect("attendee round trips");
            assert_eq!(attendee, &guest);
        }
        {
            let payload = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:33333333-3333-3333-3333-333333333333@example\r\nDTSTART:20261020T090000Z\r\nATTENDEE;DELEGATED-FROM=\"mailto:boss@example.com\";CN=\"Lee: Ops\";PARTSTAT=ACCEPTED:mailto:lee@example.com\r\nORGANIZER;SENT-BY=\"mailto:assistant@example.com\":mailto:boss@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
            let (events, _) = parse(payload);
            let event = &events[0];
            let lee = event
                .attendees
                .iter()
                .find(|attendee| attendee.email == "lee@example.com")
                .expect("delegated attendee parsed");
            assert_eq!(lee.name, "Lee: Ops");
            assert_eq!(lee.status, InviteStatus::Accepted);
            let organizer = event.organizer.expect("organizer parsed");
            assert!(
                event.attendees.iter().any(
                    |attendee| attendee.id == organizer && attendee.email == "boss@example.com"
                )
            );
        }
        {
            let mut model =
                crate::model::seed_demo(Date::new(2026, 6, 15).expect("valid date"));
            model.events.first_mut().expect("demo event").title = "Line one\r\nLine two".into();
            let exported = export(&model);
            assert!(
                exported.split("\r\n").all(|line| !line.contains('\r')),
                "a raw CR would end the content line early"
            );
            let (events, _) = parse(&exported);
            assert!(
                events
                    .iter()
                    .any(|event| event.title == "Line one\nLine two")
            );
        }
        {
            let payload = "begin:vcalendar\r\nbegin:vevent\r\nsummary:Quiet\r\ndtstart;value=date:20260615\r\nend:vevent\r\nend:vcalendar\r\n";
            let (events, _) = parse(payload);
            assert_eq!(events[0].title, "Quiet");
            assert!(events[0].all_day);
        }
        {
            let title = format!("Planning {}", "界".repeat(40));
            let event = Event {
                id: Uuid::new_v4(),
                title: title.clone(),
                start: Date::new(2026, 9, 15).unwrap()
                    .at(9, 0, 0, 0),
                end: Date::new(2026, 9, 15).unwrap()
                    .at(10, 0, 0, 0),
                ..Event::default()
            };
            let ics = export(&Calendar {
                events: vec![event].into(),
                tasks: Keyed::default(),
                projects: Keyed::default(),
                ..Calendar::default()
            });
            assert!(
                ics.split("\r\n").all(|line| line.len() <= 75),
                "every physical content line respects RFC 5545's 75-octet limit"
            );
            let (events, _) = parse(&ics);
            assert_eq!(events[0].title, title);
        }
    }

    phase a_timed_series_exports_its_end_and_exclusions_as_date_times;
    {
        let date = |day| Date::new(2026, 10, day).unwrap();
        for (zone, all_day, until, exdate) in [
            (
                "Europe/Berlin",
                false,
                "UNTIL=20261026T080000Z",
                "EXDATE;TZID=Europe/Berlin:20261012T090000",
            ),
            (
                "UTC",
                false,
                "UNTIL=20261026T090000Z",
                "EXDATE:20261012T090000Z",
            ),
            (
                "floating",
                false,
                "UNTIL=20261026T090000",
                "EXDATE:20261012T090000",
            ),
            (
                "floating",
                true,
                "UNTIL=20261026",
                "EXDATE;VALUE=DATE:20261012",
            ),
        ] {
            let start = date(5)
                .at(if all_day { 0 } else { 9 }, 0, 0, 0);
            let event = Event {
                id: Uuid::new_v4(),
                title: "Standup".into(),
                start,
                end: start + jiff::SignedDuration::from_mins(if all_day { 24 * 60 } else { 30 }),
                all_day,
                recurrence: Recurrence::Weekly,
                recurrence_until: Some(date(26)),
                recurrence_exdates: [date(12)].into_iter().collect(),
                timezone: zone.into(),
                ..Event::default()
            };
            let exported = export(&Calendar {
                events: vec![event.clone()].into(),
                ..Calendar::default()
            });
            assert!(
                exported.contains(&format!("RRULE:FREQ=WEEKLY;{until}\r\n")),
                "{exported}"
            );
            assert!(exported.contains(&format!("{exdate}\r\n")), "{exported}");
            let (events, _) = parse(&exported);
            let back = &events[0];
            assert_eq!(
                (
                    back.recurrence_until,
                    &back.recurrence_exdates,
                    &back.timezone
                ),
                (
                    event.recurrence_until,
                    &event.recurrence_exdates,
                    &event.timezone
                )
            );
            let window = (date(1), date(31) + jiff::Span::new().days(60));
            assert_eq!(
                back.occurrences_in(window.0, window.1),
                vec![date(5), date(19), date(26)]
            );
            assert_eq!(
                back.occurrences_in(window.0, window.1),
                event.occurrences_in(window.0, window.1)
            );
        }
    }
}

/// What a calendar keeps of an invitation tells the server it is stored
/// on to write to nobody, whatever the text says of that itself, and
/// loses nothing else of the lines that name somebody.
#[test]
fn the_kept_text_of_an_invitation_leaves_writing_to_people_to_nobody() {
    let invitation = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\n\
             UID:offsite-9@example.org\r\nSUMMARY:Offsite\r\nDTSTART:20261119T140000Z\r\n\
             ORGANIZER;CN=\"Bo: Ops\";SCHEDULE-AGENT=SERVER:mailto:Bo@Example.test\r\n\
             ATTENDEE;DELEGATED-FROM=\"mailto:a@example.test\",\"mailto:b@example.test\";\
             SCHEDULE-FORCE-SEND=REQUEST;PARTSTAT=NEEDS-ACTION:mailto:ada@example.test\r\n\
             ATTENDEE;RSVP=TRUE:MAILTO:cy@example.test\r\n\
             ATTENDEE:urn:uuid:5d2b\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let kept = kept_text(invitation);
    let lines = pim_lines::unfold(&kept);
    let holds = |line: &str| lines.iter().any(|kept| kept == line);
    assert!(
        !lines.iter().any(|line| line.starts_with("METHOD")),
        "{lines:?}"
    );
    assert!(
        holds("ORGANIZER;SCHEDULE-AGENT=CLIENT;CN=\"Bo: Ops\":mailto:Bo@Example.test"),
        "{lines:?}"
    );
    assert!(
        holds(
            "ATTENDEE;SCHEDULE-AGENT=CLIENT;DELEGATED-FROM=\"mailto:a@example.test\",\
                 \"mailto:b@example.test\";PARTSTAT=NEEDS-ACTION:mailto:ada@example.test"
        ),
        "{lines:?}"
    );
    assert!(
        holds("ATTENDEE;SCHEDULE-AGENT=CLIENT;RSVP=TRUE:MAILTO:cy@example.test"),
        "{lines:?}"
    );
    assert!(
        holds("ATTENDEE;SCHEDULE-AGENT=CLIENT:urn:uuid:5d2b"),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|line| {
            line.contains("SCHEDULE-AGENT=SERVER") || line.contains("SCHEDULE-FORCE-SEND")
        }),
        "{lines:?}"
    );
    // The same event either way, and the same people named.
    assert_eq!(parse(&kept).0, parse(invitation).0);
    let people = named(&kept);
    assert_eq!(people.organizers, ["bo@example.test"]);
    assert_eq!(
        people.attendees,
        ["ada@example.test", "cy@example.test", "urn:uuid:5d2b"]
    );
    assert_eq!(people, named(invitation));
    // And it says of itself whether anything is left to a server.
    assert!(leaves_writing_to_its_server(invitation));
    assert!(!leaves_writing_to_its_server(&kept));
    assert!(!leaves_writing_to_its_server(
        "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    ));
    assert!(!leaves_writing_to_its_server(
        "BEGIN:VEVENT\r\nATTENDEE;schedule-agent=none:mailto:bo@example.test\r\nEND:VEVENT\r\n"
    ));
}

/// Beside what its server holds, the kept text changes what no line says
/// of who writes to the person it names (RFC 6638, 3.2.1.2): a line the
/// server writes for goes up as the server holds it, whatever the text
/// makes of it, and everybody else is marked for the client, whatever
/// the text says of that itself.
#[test]
fn the_kept_text_beside_what_a_server_holds_changes_what_no_line_says_of_who_writes() {
    let held = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\n\
             UID:lunch-4@example.org\r\nDTSTART:20261118T120000Z\r\n\
             ORGANIZER;CN=Ada:mailto:ada@example.test\r\n\
             ATTENDEE;PARTSTAT=ACCEPTED;SCHEDULE-STATUS=\"2.0\":mailto:bo@example.test\r\n\
             ATTENDEE;SCHEDULE-AGENT=SERVER;SCHEDULE-FORCE-SEND=REQUEST:MAILTO:Eve@Elsewhere.test\r\n\
             ATTENDEE;SCHEDULE-AGENT=CLIENT:mailto:cy@example.test\r\n\
             ATTENDEE;SCHEDULE-AGENT=NONE:mailto:di@example.test\r\n\
             END:VEVENT\r\nEND:VCALENDAR\r\n";
    let changed = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\n\
             UID:lunch-4@example.org\r\nDTSTART:20261118T130000Z\r\n\
             ORGANIZER;SCHEDULE-AGENT=CLIENT;CN=Ada L:mailto:ada@example.test\r\n\
             ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:BO@example.test\r\n\
             ATTENDEE;PARTSTAT=TENTATIVE:mailto:eve@elsewhere.test\r\n\
             ATTENDEE;SCHEDULE-AGENT=SERVER:mailto:cy@example.test\r\n\
             ATTENDEE:mailto:di@example.test\r\n\
             ATTENDEE;SCHEDULE-AGENT=SERVER;SCHEDULE-FORCE-SEND=REQUEST:mailto:fay@example.test\r\n\
             END:VEVENT\r\nEND:VCALENDAR\r\n";
    let naming = |text: &str| -> Vec<String> {
        pim_lines::unfold(text)
            .into_iter()
            .filter(|line| line.starts_with("ORGANIZER") || line.starts_with("ATTENDEE"))
            .collect()
    };
    let kept = kept_beside(changed, Some(held));
    assert_eq!(
        naming(&kept),
        [
            // Three the server writes for: as it holds them, and what
            // forced it to write once does not go up again.
            "ORGANIZER;CN=Ada:mailto:ada@example.test",
            "ATTENDEE;PARTSTAT=ACCEPTED;SCHEDULE-STATUS=\"2.0\":mailto:bo@example.test",
            "ATTENDEE;SCHEDULE-AGENT=SERVER:MAILTO:Eve@Elsewhere.test",
            // Two it holds and does not write for, and one it does not
            // hold: the client's, whatever the text says.
            "ATTENDEE;SCHEDULE-AGENT=CLIENT:mailto:cy@example.test",
            "ATTENDEE;SCHEDULE-AGENT=CLIENT:mailto:di@example.test",
            "ATTENDEE;SCHEDULE-AGENT=CLIENT:mailto:fay@example.test",
        ]
    );
    assert!(!kept.contains("METHOD"), "{kept}");
    assert!(kept.contains("DTSTART:20261118T130000Z"), "{kept}");
    // Where the server holds nothing, or nothing that names anybody,
    // every line is the client's.
    assert_eq!(kept_beside(changed, None), kept_text(changed));
    let plain = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:lunch-4@example.org\r\nEND:VEVENT\r\n\
             END:VCALENDAR\r\n";
    assert_eq!(kept_beside(changed, Some(plain)), kept_text(changed));
    assert!(!leaves_writing_to_its_server(&kept_text(changed)));
}

/// Beside what its server holds, the kept text goes by the `UID` held
/// there: on the event and on each of its moved occurrences the line as
/// the server holds it, and on nothing inside them. Where the server
/// holds nothing, or nothing that says a `UID`, the text keeps its own.
#[test]
fn the_kept_text_beside_what_a_server_holds_goes_by_the_uid_held_there() {
    let held = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\n\
             UID;X-MADE-BY=elsewhere:6F1E2C3A-0B1D-4E5F-8A9B-0C1D2E3F4A5B\r\n\
             DTSTART:20261118T120000Z\r\nRRULE:FREQ=WEEKLY\r\n\
             BEGIN:VALARM\r\nUID:alarm-held\r\nACTION:DISPLAY\r\nTRIGGER:-PT10M\r\n\
             END:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let ours = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\n\
             UID:6f1e2c3a-0b1d-4e5f-8a9b-0c1d2e3f4a5b@tilo\r\nDTSTART:20261118T130000Z\r\n\
             RRULE:FREQ=WEEKLY\r\n\
             BEGIN:VALARM\r\nUID:alarm-ours\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\n\
             END:VALARM\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\n\
             UID:6f1e2c3a-0b1d-4e5f-8a9b-0c1d2e3f4a5b@tilo\r\n\
             RECURRENCE-ID:20261125T130000Z\r\nDTSTART:20261125T150000Z\r\nEND:VEVENT\r\n\
             END:VCALENDAR\r\n";
    let uid_lines = |text: &str| -> Vec<String> {
        pim_lines::unfold(text)
            .into_iter()
            .filter(|line| line.starts_with("UID"))
            .collect()
    };
    let as_held = "UID;X-MADE-BY=elsewhere:6F1E2C3A-0B1D-4E5F-8A9B-0C1D2E3F4A5B";
    let kept = kept_beside(ours, Some(held));
    assert_eq!(
        uid_lines(&kept),
        [as_held, "UID:alarm-ours", as_held],
        "{kept}"
    );
    assert!(kept.contains("DTSTART:20261118T130000Z"), "{kept}");
    // The same event to this calendar either way.
    assert_eq!(parse(&kept).0, parse(ours).0);
    // Nothing held, or nothing held that says a UID: its own.
    assert_eq!(uid_lines(&kept_beside(ours, None)), uid_lines(ours));
    let nameless = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID: \r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    assert_eq!(
        uid_lines(&kept_beside(ours, Some(nameless))),
        uid_lines(ours)
    );
}

/// A repeating entry as a server holds it. Ada organises it with Bo and
/// Eve; two more are named by addresses this calendar does not read; and
/// on the one occurrence that stands apart, Dan is invited too.
const HELD_STANDUP: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\n\
         UID:standup-7@example.org\r\nDTSTART:20261116T090000Z\r\nRRULE:FREQ=WEEKLY\r\n\
         ORGANIZER:mailto:ada@example.test\r\n\
         ATTENDEE;PARTSTAT=ACCEPTED:mailto:bo@example.test\r\n\
         ATTENDEE:mailto:eve@elsewhere.test\r\n\
         ATTENDEE;CUTYPE=GROUP;SCHEDULE-FORCE-SEND=REQUEST:urn:uuid:fay-4\r\n\
         ATTENDEE;SCHEDULE-AGENT=NONE:Mailto:Di@Example.test\r\n\
         END:VEVENT\r\nBEGIN:VEVENT\r\nUID:standup-7@example.org\r\n\
         RECURRENCE-ID:20261123T090000Z\r\nDTSTART:20261123T100000Z\r\n\
         ORGANIZER:mailto:ada@example.test\r\n\
         ATTENDEE;PARTSTAT=ACCEPTED:mailto:bo@example.test\r\n\
         ATTENDEE:mailto:eve@elsewhere.test\r\n\
         ATTENDEE;RSVP=TRUE:mailto:dan@example.test\r\n\
         END:VEVENT\r\nEND:VCALENDAR\r\n";

/// One event as this calendar writes it out.
fn written_out_alone(event: Event) -> String {
    export(&Calendar {
        events: vec![event].into(),
        ..Calendar::default()
    })
}

/// Whom each event and occurrence of a text names, in order; a line the
/// client writes for by its address alone.
fn naming_by_part(text: &str) -> Vec<Vec<String>> {
    let mut parts: Vec<Vec<String>> = Vec::new();
    for line in pim_lines::unfold(text) {
        if line == "BEGIN:VEVENT" {
            parts.push(Vec::new());
        } else if line.starts_with("ORGANIZER") || line.starts_with("ATTENDEE") {
            let said = if line.contains(";SCHEDULE-AGENT=CLIENT") {
                format!(
                    "the client's: {}",
                    line.rsplit(':').next().unwrap_or_default()
                )
            } else {
                line
            };
            if let Some(part) = parts.last_mut() {
                part.push(said);
            }
        }
    }
    parts
}

/// What goes up of an event this calendar wrote out keeps whom its
/// server holds and an event here cannot hold: somebody named by an
/// address this calendar does not read, as held, and on an occurrence
/// the lines its server holds there. An occurrence follows its event
/// only in whom this calendar took out of the event or newly put into
/// it, and an occurrence that stands apart only here names whom its
/// event goes up with. A text that was not written out from an event is
/// whole as it is.
#[test]
fn what_is_written_out_of_an_event_keeps_whom_it_cannot_hold_as_its_server_holds_them() {
    let mut event = parse(HELD_STANDUP).0.remove(0);
    assert_eq!(
        event.attendees.iter().count(),
        3,
        "Ada, Bo and Eve are read"
    );
    assert_eq!(event.occurrence_overrides.iter().count(), 1);
    // Here Eve is taken out of the event and Cy is put into it.
    let staying: Vec<Attendee> = event
        .attendees
        .iter()
        .filter(|invited| invited.email != "eve@elsewhere.test")
        .cloned()
        .collect();
    event.attendees = staying.into();
    event.attendees.push(Attendee {
        id: Id::new_v4(),
        name: "Cy".into(),
        email: "cy@example.test".into(),
        status: InviteStatus::NeedsAction,
    });
    // And one more occurrence is moved, which its server holds no part
    // for.
    event
        .occurrence_overrides
        .push(crate::model::OccurrenceOverride {
            original_date: jiff::civil::date(2026, 11, 30),
            original_time: Some(jiff::civil::time(9, 0, 0, 0)),
            cancelled: false,
            start: Some(jiff::civil::date(2026, 11, 30).at(11, 0, 0, 0)),
            end: Some(jiff::civil::date(2026, 11, 30).at(11, 15, 0, 0)),
            end_later_fold: false,
            title: None,
            location: None,
        });
    let ours = written_out_alone(event);
    let kept = exported_beside(&ours, HELD_STANDUP);
    assert_eq!(
        naming_by_part(&kept),
        [
            vec![
                "ORGANIZER:mailto:ada@example.test",
                "the client's: ada@example.test",
                "ATTENDEE;PARTSTAT=ACCEPTED:mailto:bo@example.test",
                "the client's: cy@example.test",
                // Two this calendar does not read: as held, and what
                // forced the server to write once does not go up again.
                "ATTENDEE;CUTYPE=GROUP:urn:uuid:fay-4",
                "ATTENDEE;SCHEDULE-AGENT=NONE:Mailto:Di@Example.test",
            ],
            vec![
                // As its server holds the occurrence, without Eve, who
                // was taken out of the event, and with Dan, who is
                // invited to this occurrence alone.
                "ORGANIZER:mailto:ada@example.test",
                "ATTENDEE;PARTSTAT=ACCEPTED:mailto:bo@example.test",
                "ATTENDEE;RSVP=TRUE:mailto:dan@example.test",
                // And whom the event newly names here.
                "the client's: ada@example.test",
                "the client's: cy@example.test",
            ],
            vec![
                // The occurrence moved here: whom the event goes up
                // with.
                "ORGANIZER:mailto:ada@example.test",
                "the client's: ada@example.test",
                "ATTENDEE;PARTSTAT=ACCEPTED:mailto:bo@example.test",
                "the client's: cy@example.test",
                "ATTENDEE;CUTYPE=GROUP:urn:uuid:fay-4",
                "ATTENDEE;SCHEDULE-AGENT=NONE:Mailto:Di@Example.test",
            ],
        ],
        "{kept}"
    );
    assert!(!kept.contains("SCHEDULE-FORCE-SEND"), "{kept}");
    assert!(kept.contains("UID:standup-7@example.org"), "{kept}");
    // A text that was not written out from an event says all it means:
    // nobody is put back into it.
    let whole = kept_beside(&ours, Some(HELD_STANDUP));
    assert!(
        !whole.contains("fay-4") && !whole.contains("dan@example.test"),
        "{whole}"
    );
}

/// Who organises an entry stays where its server names them by an
/// address this calendar does not read, and is the only one to organise
/// it: where the event here says who organises it, that stands instead.
#[test]
fn who_organises_an_entry_by_an_address_this_calendar_does_not_read_stays() {
    let by_other_address = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:review-5@example.org\r\n\
             DTSTART:20261116T090000Z\r\nORGANIZER:urn:uuid:ada\r\n\
             ATTENDEE:mailto:bo@example.test\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let mut event = parse(by_other_address).0.remove(0);
    assert_eq!(event.organizer, None);
    let kept = exported_beside(&written_out_alone(event.clone()), by_other_address);
    assert_eq!(
        naming_by_part(&kept),
        [vec![
            "ATTENDEE:mailto:bo@example.test",
            "ORGANIZER:urn:uuid:ada"
        ]],
        "{kept}"
    );
    event.organizer = event.attendees.iter().next().map(|invited| invited.id);
    let kept = exported_beside(&written_out_alone(event), by_other_address);
    assert_eq!(
        naming_by_part(&kept),
        [vec![
            "the client's: bo@example.test",
            "ATTENDEE:mailto:bo@example.test"
        ]],
        "{kept}"
    );
}

/// Writing an entry ends what its server schedules of it where the
/// server holds who organises it and writes to somebody it names, and
/// what is written no longer says who organises it, on the event or on
/// one of its occurrences (RFC 6638, 3.1 and 3.2.3.1). An entry its
/// server writes to nobody for, and one nobody organises, has nothing to
/// end.
#[test]
fn what_no_longer_says_who_organises_it_ends_what_its_server_schedules() {
    let entry = |parts: &[&str]| {
        let mut text = String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n");
        for (place, people) in parts.iter().enumerate() {
            text.push_str("BEGIN:VEVENT\r\nUID:lunch-4@example.org\r\n");
            if place > 0 {
                text.push_str("RECURRENCE-ID:20261125T120000Z\r\n");
            }
            text.push_str("DTSTART:20261118T120000Z\r\n");
            text.push_str(people);
            text.push_str("END:VEVENT\r\n");
        }
        text.push_str("END:VCALENDAR\r\n");
        text
    };
    let hers = "ORGANIZER:mailto:ada@example.test\r\nATTENDEE:mailto:bo@example.test\r\n";
    let hers_alone = "ORGANIZER:MAILTO:Ada@Example.test\r\n";
    let held = entry(&[hers]);
    assert!(ends_what_its_server_schedules(&entry(&[""]), &held));
    assert!(!ends_what_its_server_schedules(&entry(&[hers]), &held));
    // Her guests taken out is her change to an entry she still organises.
    assert!(!ends_what_its_server_schedules(
        &entry(&[hers_alone]),
        &held
    ));
    // Every part says it, or the entry is none its server schedules.
    assert!(ends_what_its_server_schedules(&entry(&[hers, ""]), &held));
    assert!(!ends_what_its_server_schedules(
        &entry(&[hers, hers_alone]),
        &held
    ));
    // Somebody else organises it and she is invited: the same.
    let invited =
        entry(&["ORGANIZER:mailto:eve@elsewhere.test\r\nATTENDEE:mailto:ada@example.test\r\n"]);
    assert!(ends_what_its_server_schedules(&entry(&[""]), &invited));
    assert!(ends_what_its_server_schedules(
        &entry(&[hers_alone]),
        &invited
    ));
    // Nothing to end: the client writes to everybody it names, nobody
    // organises it, or it names nobody.
    let clients = entry(&[
        "ORGANIZER;SCHEDULE-AGENT=CLIENT:mailto:ada@example.test\r\n\
             ATTENDEE;SCHEDULE-AGENT=CLIENT:mailto:bo@example.test\r\n",
    ]);
    assert!(!ends_what_its_server_schedules(&entry(&[""]), &clients));
    let unorganised = entry(&["ATTENDEE:mailto:bo@example.test\r\n"]);
    assert!(!ends_what_its_server_schedules(&entry(&[""]), &unorganised));
    assert!(!ends_what_its_server_schedules(
        &entry(&[""]),
        &entry(&[""])
    ));
}

fn outlook_zone(tzid: &str) -> String {
    format!(
        "BEGIN:VTIMEZONE\r\nTZID:{tzid}\r\n\
             BEGIN:STANDARD\r\nDTSTART:16010101T030000\r\nTZOFFSETFROM:+0200\r\n\
             TZOFFSETTO:+0100\r\nRRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=-1SU;BYMONTH=10\r\n\
             END:STANDARD\r\n\
             BEGIN:DAYLIGHT\r\nDTSTART:16010101T020000\r\nTZOFFSETFROM:+0100\r\n\
             TZOFFSETTO:+0200\r\nRRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=-1SU;BYMONTH=3\r\n\
             END:DAYLIGHT\r\nEND:VTIMEZONE\r\n"
    )
}

fn outlook_invite(zone: &str, tzid: &str, start: &str, end: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nPRODID:Microsoft Exchange Server 2010\r\n\
             VERSION:2.0\r\n{zone}BEGIN:VEVENT\r\n\
             ORGANIZER;CN=Jane Doe:mailto:jane@contoso.com\r\n\
             ATTENDEE;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE;CN=Sam Example:\r\n \
             mailto:sam@example.com\r\n\
             DESCRIPTION;LANGUAGE=en-US:Agenda to follow\\n\r\n\
             UID:040000008200E00074C5B7101A82E00800000000B0E0F5A2C1E3DC0100000000000000001\r\n \
             0000000F3A1C2B4D5E6F708192A3B4C5D6E7F80\r\n\
             SUMMARY;LANGUAGE=en-US:Quarterly planning\r\n\
             DTSTART;TZID={tzid}:{start}\r\nDTEND;TZID={tzid}:{end}\r\n\
             CLASS:PUBLIC\r\nPRIORITY:5\r\nDTSTAMP:20261020T081500Z\r\nTRANSP:OPAQUE\r\n\
             STATUS:CONFIRMED\r\nSEQUENCE:0\r\nLOCATION;LANGUAGE=en-US:Room 4.12\r\n\
             X-MICROSOFT-CDO-BUSYSTATUS:TENTATIVE\r\nX-MICROSOFT-CDO-ALLDAYEVENT:FALSE\r\n\
             BEGIN:VALARM\r\nDESCRIPTION:REMINDER\r\nTRIGGER;RELATED=START:-PT15M\r\n\
             ACTION:DISPLAY\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

fn imported_at(payload: &str) -> (String, String) {
    let parsed = parse_calendar(payload);
    assert_eq!(parsed.events.len(), 1, "the invite's event is kept");
    let event = &parsed.events[0];
    let instant = crate::timezone::wall_to_utc(&event.timezone, event.start)
        .map(|(instant, _)| instant.to_string())
        .unwrap_or_default();
    (event.timezone.clone(), instant)
}
