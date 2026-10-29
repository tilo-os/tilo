//! The people who may open the system volume, as `tilo-auth boot-people`
//! lists them from the volume's `tilo-signin` tokens.

use serde::{Deserialize, Serialize};

/// The longest name the boot screen shows; a longer one is cut, never wrapped.
const NAME_LIMIT: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

/// What of a name the screen may show: one line, bounded, without control
/// characters, which would otherwise break the console's question.
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
