//! The people who may open the system volume, as `tilo-auth boot-people`
//! lists them from the volume's `tilo-signin` tokens.

use serde::Deserialize;

use crate::glyphs::Glyphs;

/// The longest name the boot screen draws; a longer one is cut, never wrapped.
const NAME_LIMIT: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretKind {
    Pin,
    Password,
}

impl SecretKind {
    /// The word the screen uses for this secret.
    pub fn label(self) -> &'static str {
        match self {
            Self::Pin => "PIN",
            Self::Password => "Password",
        }
    }

    /// The same word inside a sentence.
    pub fn noun(self) -> &'static str {
        match self {
            Self::Pin => "PIN",
            Self::Password => "password",
        }
    }

    /// The value the Plymouth theme reads.
    pub fn wire(self) -> &'static str {
        match self {
            Self::Pin => "pin",
            Self::Password => "password",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Person {
    pub uid: u32,
    pub name: String,
    pub display_name: String,
    pub secret: SecretKind,
}

#[derive(Deserialize)]
struct Listed {
    uid: u32,
    name: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    factors: Vec<String>,
    #[serde(default)]
    secret: Option<String>,
}

impl Person {
    /// The name the boot screen shows: the display name, or the account name
    /// where the screen's font cannot draw the display name but can draw the
    /// account name.
    pub fn shown_name(&self, glyphs: &Glyphs) -> &str {
        if glyphs.draws(&self.display_name) || !glyphs.draws(&self.name) {
            &self.display_name
        } else {
            &self.name
        }
    }

    /// The face the login screen draws for this person: their presence
    /// slot, from the account name as the greeter's avatar takes it, and the
    /// initials of the name the screen shows. Initials the screen's font
    /// cannot draw are left out, and the theme draws a figure instead.
    pub fn face(&self, glyphs: &Glyphs) -> (usize, String) {
        let initials = tilo_face::initials(self.shown_name(glyphs));
        (
            tilo_face::presence_slot(&self.name),
            if glyphs.draws(&initials) {
                initials
            } else {
                String::new()
            },
        )
    }

    /// The account name under the shown name, when the two differ, as the
    /// login screen shows it.
    pub fn account(&self, glyphs: &Glyphs) -> &str {
        if self.shown_name(glyphs) == self.name {
            ""
        } else {
            &self.name
        }
    }
}

/// Parse `boot-people` output. Anything unreadable lists nobody, so the boot
/// falls back to the prompts that need no list.
pub fn parse(json: &str) -> Vec<Person> {
    let Ok(listed) = serde_json::from_str::<Vec<Listed>>(json) else {
        return Vec::new();
    };
    listed
        .into_iter()
        .filter_map(|person| {
            let name = shown(&person.name);
            if name.is_empty() {
                return None;
            }
            let display_name = match shown(&person.display_name) {
                display if display.is_empty() => name.clone(),
                display => display,
            };
            let pin = person.secret.as_deref() == Some("pin")
                || (person.secret.is_none() && person.factors.iter().any(|f| f == "pin"));
            Some(Person {
                uid: person.uid,
                name,
                display_name,
                secret: if pin {
                    SecretKind::Pin
                } else {
                    SecretKind::Password
                },
            })
        })
        .collect()
}

/// What of a name the screen may draw: one line, bounded, without control
/// characters, which would otherwise split the theme's messages.
fn shown(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control())
        .take(NAME_LIMIT)
        .collect::<String>()
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn people_parse_with_their_kind_of_secret() {
        let people = parse(
            r#"[
              {"uid":1000,"name":"anna","display_name":"Anna Example","factors":["tpm","recovery"],"secret":"pin"},
              {"uid":1001,"name":"ben","display_name":"","factors":["passphrase","recovery"]},
              {"uid":1002,"name":"cleo","display_name":"Cleo","factors":["tpm","pin"]}
            ]"#,
        );
        assert_eq!(
            people,
            vec![
                Person {
                    uid: 1000,
                    name: "anna".into(),
                    display_name: "Anna Example".into(),
                    secret: SecretKind::Pin,
                },
                Person {
                    uid: 1001,
                    name: "ben".into(),
                    display_name: "ben".into(),
                    secret: SecretKind::Password,
                },
                Person {
                    uid: 1002,
                    name: "cleo".into(),
                    display_name: "Cleo".into(),
                    secret: SecretKind::Pin,
                },
            ]
        );
    }

    #[test]
    fn a_person_has_the_face_and_account_the_login_screen_shows() {
        let people = parse(
            r#"[
              {"uid":1000,"name":"ada","display_name":"Ada Example","secret":"password"},
              {"uid":1001,"name":"ben","display_name":""},
              {"uid":1002,"name":"mei","display_name":"\u674e \u6885"},
              {"uid":1003,"name":"\u674e","display_name":"\u674e \u6885"}
            ]"#,
        );
        let [ada, ben, mei, li] = people.as_slice() else {
            panic!("four people");
        };
        let latin = Glyphs::parse("0020-007e\n00a0-024f\n").expect("ranges");
        assert_eq!(
            ada.face(&latin),
            (tilo_face::presence_slot("ada"), "AE".to_owned())
        );
        assert_eq!(ada.shown_name(&latin), "Ada Example");
        assert_eq!(ada.account(&latin), "ada");
        assert_eq!(ben.face(&latin).1, "B");
        assert_eq!(ben.account(&latin), "");
        // A display name the font cannot draw gives way to the account name.
        assert_eq!(mei.shown_name(&latin), "mei");
        assert_eq!(
            mei.face(&latin),
            (tilo_face::presence_slot("mei"), "M".to_owned())
        );
        assert_eq!(mei.account(&latin), "");
        // Where neither can be drawn, the display name stays, without initials.
        assert_eq!(li.shown_name(&latin), "\u{674e} \u{6885}");
        assert_eq!(li.face(&latin).1, "");
        // Where the font is not known, the display name is shown.
        assert_eq!(mei.shown_name(&Glyphs::any()), "\u{674e} \u{6885}");
    }

    #[test]
    fn unreadable_lists_and_nameless_people_list_nobody() {
        assert!(parse("").is_empty());
        assert!(parse("null").is_empty());
        assert!(parse("{\"uid\":1}").is_empty());
        assert!(parse(r#"[{"uid":1000,"name":"\n"}]"#).is_empty());
    }

    #[test]
    fn names_are_one_bounded_line() {
        let long = "x".repeat(200);
        let people = parse(&format!(
            r#"[{{"uid":1000,"name":"anna","display_name":"Anna\ntilo-signin-end{long}"}}]"#
        ));
        let display = &people.first().expect("one person").display_name;
        assert!(!display.contains('\n'));
        assert_eq!(display.chars().count(), NAME_LIMIT);
    }
}
