//! Names as Discord takes them. Text channel names are lower-case slugs,
//! and webhook usernames may not contain "discord" or "clyde". A name is
//! cleaned the same way every time, so the reconcile compares like with like
//! and never loops on a name Discord would rewrite.

/// Discord's limit for a channel name.
pub const MAX_CHANNEL_NAME_CHARS: usize = 100;
/// Discord's limit for a webhook username.
pub const MAX_USERNAME_CHARS: usize = 80;
/// The slug of a name with nothing Discord keeps.
pub const FALLBACK_SLUG: &str = "channel";
/// The username of an author whose name has nothing Discord keeps.
pub const FALLBACK_USERNAME: &str = "Tilo member";

/// The slug Discord would make of `name` for a text channel: lower case,
/// spaces and separators as `-`, other punctuation and symbols dropped,
/// never empty.
#[must_use]
pub fn channel_slug(name: &str) -> String {
    let mut slug = String::new();
    let mut dash = false;
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() || c == '_' {
            if dash && !slug.is_empty() {
                slug.push('-');
            }
            dash = false;
            slug.push(c);
        } else if c.is_whitespace() || c == '-' || c == '.' || c == '/' {
            dash = true;
        }
    }
    let slug: String = slug.chars().take(MAX_CHANNEL_NAME_CHARS).collect();
    let slug = slug.trim_end_matches('-').to_owned();
    if slug.is_empty() {
        FALLBACK_SLUG.to_owned()
    } else {
        slug
    }
}

/// The name a channel of `kind` shows in Discord: text channels slugged,
/// categories and voice channels as written (trimmed, bounded).
#[must_use]
pub fn discord_channel_name(kind: discord_wire::op::ChannelKind, name: &str) -> String {
    use discord_wire::op::ChannelKind;
    match kind {
        ChannelKind::Text | ChannelKind::Thread => channel_slug(name),
        ChannelKind::Voice | ChannelKind::Category => {
            let trimmed: String = name.trim().chars().take(MAX_CHANNEL_NAME_CHARS).collect();
            if trimmed.is_empty() {
                FALLBACK_SLUG.to_owned()
            } else {
                trimmed
            }
        }
    }
}

/// Names for `names` in order, unique: a name already taken gets `-2`,
/// `-3`… appended, so two Tilo channels that slug alike stay apart.
#[must_use]
pub fn unique(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut taken = std::collections::BTreeSet::new();
    names
        .into_iter()
        .map(|name| {
            let mut candidate = name.clone();
            let mut n = 2_u32;
            while taken.contains(&candidate) {
                candidate = format!("{name}-{n}");
                n = n.saturating_add(1);
            }
            taken.insert(candidate.clone());
            candidate
        })
        .collect()
}

/// A webhook username Discord accepts for `name`. Discord refuses names
/// containing "discord" or "clyde" and treats `@`, `#`, `:` and code fences
/// as markup, so those are removed; an automated author gets the robot mark
/// because a webhook has no other way to say so.
#[must_use]
pub fn webhook_username(name: &str, automated: bool) -> String {
    let mut cleaned: String = name
        .chars()
        .filter(|c| !matches!(c, '@' | '#' | ':' | '`') && !c.is_control())
        .collect();
    // Removing one word can join the halves of another, so repeat until
    // neither is left.
    loop {
        let before = cleaned.len();
        for banned in ["discord", "clyde"] {
            cleaned = remove_ascii_case_insensitive(&cleaned, banned);
        }
        if cleaned.len() == before {
            break;
        }
    }
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let base = if cleaned.is_empty() {
        FALLBACK_USERNAME.to_owned()
    } else {
        cleaned
    };
    let named = if automated {
        format!("🤖 {base}")
    } else {
        base
    };
    named.chars().take(MAX_USERNAME_CHARS).collect()
}

fn remove_ascii_case_insensitive(text: &str, needle: &str) -> String {
    let mut out = text.to_owned();
    while let Some(at) = out.to_ascii_lowercase().find(needle) {
        out.replace_range(at..at.saturating_add(needle.len()), "");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use discord_wire::op::ChannelKind;

    #[test]
    fn slug_lowercases_and_dashes() {
        assert_eq!(channel_slug("General Chat!!"), "general-chat");
        assert_eq!(channel_slug("  road map / 2026 "), "road-map-2026");
        assert_eq!(channel_slug("Ünïcode Ok"), "ünïcode-ok");
    }

    #[test]
    fn slug_of_nothing_kept_is_never_empty() {
        assert_eq!(channel_slug("🎉"), FALLBACK_SLUG);
        assert_eq!(channel_slug("!!!"), FALLBACK_SLUG);
        assert_eq!(channel_slug(""), FALLBACK_SLUG);
    }

    #[test]
    fn slug_is_bounded_and_stable() {
        let long = "a".repeat(300);
        assert_eq!(channel_slug(&long).chars().count(), MAX_CHANNEL_NAME_CHARS);
        let once = channel_slug("Hello World");
        assert_eq!(
            channel_slug(&once),
            once,
            "a slug slugs to itself, so the reconcile settles"
        );
    }

    #[test]
    fn slug_collisions_are_numbered() {
        let names = unique([
            "general".to_owned(),
            "general".to_owned(),
            "general".to_owned(),
        ]);
        assert_eq!(names, ["general", "general-2", "general-3"]);
    }

    #[test]
    fn categories_and_voice_keep_their_names() {
        assert_eq!(
            discord_channel_name(ChannelKind::Category, " Chat Room "),
            "Chat Room"
        );
        assert_eq!(discord_channel_name(ChannelKind::Voice, "Voice"), "Voice");
        assert_eq!(discord_channel_name(ChannelKind::Text, "Voice"), "voice");
    }

    #[test]
    fn webhook_username_is_cleaned() {
        assert_eq!(webhook_username("Discord fan", false), "fan");
        assert_eq!(webhook_username("my CLYDE @bot #1 ```", false), "my bot 1");
        assert_eq!(webhook_username("discord", false), FALLBACK_USERNAME);
        assert_eq!(webhook_username("Scribe", true), "🤖 Scribe");
        assert_eq!(
            webhook_username(&"x".repeat(200), false).chars().count(),
            MAX_USERNAME_CHARS
        );
        assert_eq!(
            webhook_username("disclydecord", false),
            FALLBACK_USERNAME,
            "removing one can reveal another"
        );
    }
}
