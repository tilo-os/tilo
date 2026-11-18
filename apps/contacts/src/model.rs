//! The contact resource's types are the model crate's; the wording, the picture encoding and the
//! search's month names are the window's.

pub use contacts_model::model::*;

/// The birthday in the toolkit's worded date, e.g. `Dec 10, 1815`, or `Dec 10` when the year is
/// unknown.
pub fn pretty(birthday: &Birthday) -> String {
    fn civil_date(year: i32, month: u32, day: u32) -> Option<ui::format::Date> {
        ui::format::Date::new(
            i16::try_from(year).ok()?,
            i8::try_from(month).ok()?,
            i8::try_from(day).ok()?,
        )
        .ok()
    }
    match *birthday {
        Birthday::Full { year, month, day } => {
            civil_date(year, month, day).map_or_else(|| birthday.iso(), ui::format::month_day_year)
        }
        // Any leap year holds every month and day a year-less date can name.
        Birthday::MonthDay { month, day } => {
            civil_date(2000, month, day).map_or_else(|| birthday.iso(), ui::format::month_day)
        }
    }
}

/// The picture's encoded bytes, if the contact has one.
pub fn photo_bytes(contact: &Contact) -> Option<Vec<u8>> {
    if contact.photo.is_empty() {
        return None;
    }
    ui::held_images::picture_bytes(&contact.photo)
}

/// Take `encoded` (any picture) as the contact's picture, made a small square. Returns whether
/// it decoded.
pub fn set_photo(contact: &mut Contact, encoded: &[u8]) -> bool {
    match ui::held_images::portrait(encoded) {
        Some(portrait) => {
            contact.photo = ui::held_images::picture_text(&portrait);
            true
        }
        None => false,
    }
}

/// Whether `query` matches anywhere in the resource: name, organization, title, any phone, email
/// or address, notes or group.
pub fn matches(contact: &Contact, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let mut hay = String::new();
    hay.push_str(&contact.first);
    hay.push(' ');
    hay.push_str(&contact.last);
    hay.push(' ');
    hay.push_str(&contact.org);
    hay.push(' ');
    hay.push_str(&contact.title);
    hay.push(' ');
    hay.push_str(&contact.website);
    hay.push(' ');
    if let Some(birthday) = contact.birthday {
        hay.push_str(&birthday.iso());
        hay.push(' ');
        hay.push_str(&pretty(&birthday));
        hay.push(' ');
        // The month's whole name too, so "december" finds the birthday.
        hay.push_str(&ui::format::month_name(
            i8::try_from(birthday.month()).unwrap_or_default(),
        ));
        hay.push(' ');
    }
    hay.push_str(&contact.notes);
    for p in &contact.phones {
        hay.push(' ');
        hay.push_str(&p.label);
        hay.push(' ');
        hay.push_str(&p.value);
    }
    for e in &contact.emails {
        hay.push(' ');
        hay.push_str(&e.label);
        hay.push(' ');
        hay.push_str(&e.value);
    }
    for a in &contact.addresses {
        hay.push(' ');
        hay.push_str(&a.label);
        hay.push(' ');
        hay.push_str(&a.street);
        hay.push(' ');
        hay.push_str(&a.city);
        hay.push(' ');
        hay.push_str(&a.region);
        hay.push(' ');
        hay.push_str(&a.postal);
        hay.push(' ');
        hay.push_str(&a.country);
        hay.push(' ');
        hay.push_str(&a.one_line());
    }
    for g in &contact.groups {
        hay.push(' ');
        hay.push_str(&g.value);
    }
    normalize_text(&hay).contains(&normalize_text(query))
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        #[allow(
            clippy::too_many_lines,
            reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
        )]
        fn the_contact_model_reads_birthdays_names_and_the_books_order;

        phase birthday_parses_formats_and_round_trips;
        // Birthdays parse ISO and common typed forms, round-trip and pretty-print.
        {
            {
                let b = Birthday::parse("1815-12-10").expect("valid ISO date");
                assert_eq!(b.year(), Some(1815));
                assert_eq!(b.month(), 12);
                assert_eq!(b.day(), 10);
                assert_eq!(
                    b,
                    Birthday::Full {
                        year: 1815,
                        month: 12,
                        day: 10
                    }
                );
            }

            {
                assert_eq!(
                    Birthday::parse("2001/03/09"),
                    Some(Birthday::Full {
                        year: 2001,
                        month: 3,
                        day: 9
                    })
                );
                assert_eq!(
                    Birthday::parse("20010309"),
                    Some(Birthday::Full {
                        year: 2001,
                        month: 3,
                        day: 9
                    })
                );
                assert_eq!(
                    Birthday::parse("03/09/2001"),
                    Some(Birthday::Full {
                        year: 2001,
                        month: 3,
                        day: 9
                    })
                );
                assert_eq!(
                    Birthday::parse("09.03.2001"),
                    Some(Birthday::Full {
                        year: 2001,
                        month: 3,
                        day: 9
                    })
                );
                assert_eq!(
                    Birthday::parse("25/12/1990"),
                    Some(Birthday::Full {
                        year: 1990,
                        month: 12,
                        day: 25
                    })
                );
                assert_eq!(Birthday::parse("not a date"), None);
                assert_eq!(Birthday::parse(""), None);
                assert_eq!(Birthday::parse("2001-13-40"), None);
            }

            {
                assert_eq!(
                    Birthday::parse("12-10"),
                    Some(Birthday::MonthDay { month: 12, day: 10 })
                );
                assert_eq!(
                    Birthday::parse("--12-10"),
                    Some(Birthday::MonthDay { month: 12, day: 10 })
                );
                assert_eq!(
                    Birthday::parse("--1210"),
                    Some(Birthday::MonthDay { month: 12, day: 10 })
                );
                let b = Birthday::parse("12-10").unwrap();
                assert_eq!(b.year(), None, "a year-less birthday has no year");
                assert_eq!((b.month(), b.day()), (12, 10));
            }

            {
                for b in [
                    Birthday::Full {
                        year: 1815,
                        month: 12,
                        day: 10,
                    },
                    Birthday::Full {
                        year: 2000,
                        month: 2,
                        day: 29,
                    },
                    Birthday::MonthDay { month: 6, day: 23 },
                ] {
                    assert_eq!(
                        Birthday::parse(&b.iso()),
                        Some(b),
                        "{} round-trips",
                        b.iso()
                    );
                }
            }

            {
                assert_eq!(
                    pretty(&Birthday::Full {
                        year: 1815,
                        month: 12,
                        day: 10
                    }),
                    "Dec 10, 1815"
                );
                assert_eq!(
                    pretty(&Birthday::MonthDay { month: 12, day: 10 }),
                    "Dec 10"
                );
            }
        }

        phase display_names_and_field_matching;
        {
            {
                let mut c = Contact::new();
                assert_eq!(c.display_name(), "Unnamed contact");
                c.org = "ACME".into();
                assert_eq!(c.display_name(), "ACME");
                c.first = "Jane".into();
                c.last = "Doe".into();
                assert_eq!(c.display_name(), "Jane Doe");
            }

            {
                let mut c = Contact::new();
                c.first = "Grace".into();
                c.emails.push(Labeled::new("work", "grace@navy.mil"));
                assert!(matches(&c, &"navy".to_lowercase()));
                assert!(matches(&c, &"grace".to_lowercase()));
                assert!(!matches(&c, &"zzz".to_lowercase()));
            }
        }

        phase book_filters_sorts_and_groups;
        {
            {
                let store = book(sample_contacts());
                let ids = store.filtered_sorted("", None, SortKey::Name);
                let names: Vec<String> = ids
                    .iter()
                    .map(|id| store.contact(*id).unwrap().last.clone())
                    .collect();
                let pos = |n: &str| names.iter().position(|x| x == n).unwrap();
                assert!(pos("Berners-Lee") < pos("Hopper"));
                assert!(pos("Hopper") < pos("Lovelace"));

                let friends = store.filtered_sorted("", Some("Friends"), SortKey::Name);
                assert!(friends.iter().all(|id| {
                    store
                        .contact(*id)
                        .unwrap()
                        .groups
                        .iter()
                        .any(|g| g.value.eq_ignore_ascii_case("Friends"))
                }));
                assert!(!friends.is_empty());
            }

            {
                let mut a = Contact::new();
                a.groups = vec![
                    GroupEntry::new("Work"),
                    GroupEntry::new("work"),
                    GroupEntry::new("Family"),
                ]
                .into();
                let store = book(vec![a]);
                let groups = store.all_groups();
                assert_eq!(groups.len(), 2, "Work/work collapse: {groups:?}");
            }

            {
                let mut first = Contact::new();
                first.groups.push(GroupEntry::new("Cafe\u{301}"));
                let mut second = Contact::new();
                second.groups.push(GroupEntry::new("CAFÉ"));
                let store = book(vec![first.clone(), second]);

                assert_eq!(store.all_groups(), vec!["CAFÉ".to_string()]);
                let visible = store.filtered_sorted("", Some("café"), SortKey::Name);
                assert_eq!(visible.len(), 2);
                assert!(matches(&first, "CAFÉ"));
            }
        }
    }

    fn book(contacts: Vec<Contact>) -> crate::contact_cards::Book {
        crate::contact_cards::Book {
            contacts: contacts.into(),
            removed: ui::tilo_resource::Set::default(),
        }
    }
}
