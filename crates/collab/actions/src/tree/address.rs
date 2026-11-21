//! An address, taken apart: the resources it walks, each inside the one
//! before, and the action after `#` (`notes/Todo/body#append-paragraph`).

/// One step of an address: the name of a resource among the others under its parent, and
/// what it is pinned to after `@` (`browser@4711`, one window of an app), when
/// anything is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    /// The name as written before the pin.
    pub name: String,
    /// What follows the last unescaped `@`, as written.
    pub pin: Option<String>,
}

impl Segment {
    /// A segment that is `name` whole, pinned to nothing.
    #[must_use]
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            pin: None,
        }
    }

    /// The segment as it was written: its name, then `@` and its pin.
    #[must_use]
    pub fn written(&self) -> String {
        match &self.pin {
            Some(pin) => format!("{}@{pin}", self.name),
            None => self.name.clone(),
        }
    }
}

/// A path of resources, each inside the one before, and the action it ends
/// in, if it names one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Address {
    /// The resources walked, outermost first; never empty once parsed.
    pub segments: Vec<Segment>,
    /// The action after `#`.
    pub action: Option<String>,
}

impl Address {
    /// Parse `text`. A name containing `/`, `#` or `\` writes it with a
    /// leading backslash; an `@` written so is part of the name, not a pin.
    ///
    /// # Errors
    /// `text` names no resource.
    pub fn parse(text: &str) -> Result<Self, String> {
        // Each part as written, and the place of its last unescaped `@`.
        let mut parts: Vec<(String, Option<usize>)> = vec![(String::new(), None)];
        let mut action = String::new();
        let mut in_action = false;
        let mut escaped = false;
        for character in text.trim().chars() {
            if escaped {
                escaped = false;
                push_char(&mut parts, &mut action, in_action, character);
                continue;
            }
            match character {
                '\\' => escaped = true,
                '#' if !in_action => in_action = true,
                '/' if !in_action => parts.push((String::new(), None)),
                '@' if !in_action => {
                    if let Some((part, pin)) = parts.last_mut() {
                        *pin = Some(part.len());
                        part.push('@');
                    }
                }
                other => push_char(&mut parts, &mut action, in_action, other),
            }
        }
        let action = in_action
            .then_some(action)
            .map(|action| action.trim().to_owned())
            .filter(|action| !action.is_empty());
        let segments: Vec<Segment> = parts
            .into_iter()
            .filter_map(|(part, pin)| segment(&part, pin))
            .collect();
        if segments.is_empty() {
            return Err("a path starts with an app: notes/Todo".to_owned());
        }
        Ok(Self { segments, action })
    }

    /// The address as it names a resource, whole: every segment as written,
    /// without an action after `#`.
    #[must_use]
    pub fn resource(&self) -> String {
        self.segments
            .iter()
            .map(Segment::written)
            .collect::<Vec<_>>()
            .join("/")
    }
}

/// The segment `part` is, trimmed as a whole, with its pin split off at
/// `pin`; `None` for a part that is blank.
fn segment(part: &str, pin: Option<usize>) -> Option<Segment> {
    let start = part.len().saturating_sub(part.trim_start().len());
    let trimmed = part.trim();
    if trimmed.is_empty() {
        return None;
    }
    let pin = pin
        .and_then(|at| at.checked_sub(start))
        .filter(|at| *at < trimmed.len());
    Some(match pin {
        Some(at) => Segment {
            name: trimmed.get(..at).unwrap_or_default().to_owned(),
            pin: Some(
                trimmed
                    .get(at.saturating_add(1)..)
                    .unwrap_or_default()
                    .to_owned(),
            ),
        },
        None => Segment::named(trimmed),
    })
}

fn push_char(
    parts: &mut [(String, Option<usize>)],
    action: &mut String,
    in_action: bool,
    character: char,
) {
    if in_action {
        action.push(character);
    } else if let Some((part, _)) = parts.last_mut() {
        part.push(character);
    }
}

#[cfg(test)]
mod tests {
    use super::{Address, Segment};

    #[test]
    fn an_address_is_resources_and_an_action() {
        assert_eq!(
            Address::parse("notes/Todo/body#append-paragraph").unwrap(),
            Address {
                segments: vec![
                    Segment::named("notes"),
                    Segment::named("Todo"),
                    Segment::named("body")
                ],
                action: Some("append-paragraph".into()),
            }
        );
        assert_eq!(
            Address::parse("  notes ").unwrap(),
            Address {
                segments: vec![Segment::named("notes")],
                action: None,
            }
        );
        // A title with a slash or a hash writes it escaped.
        let parsed = Address::parse(r"notes/Q1\/Q2 plan\#1#rename").unwrap();
        assert_eq!(parsed.segments[1], Segment::named("Q1/Q2 plan#1"));
        assert_eq!(parsed.action.as_deref(), Some("rename"));
        assert_eq!(super::super::path_name("Q1/Q2 plan#1"), r"Q1\/Q2 plan\#1");
        assert!(Address::parse("").is_err());
        assert!(Address::parse("#rename").is_err());
    }

    #[test]
    fn a_pin_follows_the_last_at_and_writes_back_as_it_was() {
        let parsed = Address::parse("browser@4711/page#read-page").unwrap();
        assert_eq!(
            parsed.segments[0],
            Segment {
                name: "browser".into(),
                pin: Some("4711".into()),
            }
        );
        assert_eq!(parsed.resource(), "browser@4711/page");
        // An escaped `@` is part of the name, and every form writes back whole.
        for written in ["a@b@c", r"me\@example.com", "notes@", "notes @ 1", "@x"] {
            let parsed = Address::parse(written).unwrap();
            assert_eq!(parsed.resource(), written.replace('\\', ""), "{written}");
        }
        assert_eq!(
            Address::parse(r"me\@example.com").unwrap().segments[0],
            Segment::named("me@example.com")
        );
        assert_eq!(
            Address::parse("a@b@c").unwrap().segments[0].pin.as_deref(),
            Some("c")
        );
    }
}
