//! The one way every app draws who did something, so no app can forget to
//! say that a bot is a bot. A person gets their round avatar; an agent its
//! identity mark; a bot a rounded-square tile and a `BOT` chip that is never
//! elided. A bot may show another name (an appearance): it is shown, the chip
//! stays, and a name that looks like a room member's carries its service.

use unicode_normalization::UnicodeNormalization;
use vello::kurbo::{Point, Rect};
use vello::peniko::{Color, ImageData};

use crate::agent_identity::{AgentIdentityStyle, draw_agent_identity_mark};
use crate::i18n::widget_format;
use crate::paint::PaintCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{AvatarFace, ChipFace, ChipLead};

/// Who an item is by.
#[derive(Clone, Copy, Debug)]
pub enum Author<'a> {
    /// A person, by name and identity (which picks their colour), with the
    /// picture they set, already decoded by the app's image store.
    Person {
        name: &'a str,
        identity: &'a str,
        avatar: Option<&'a ImageData>,
        color: Option<Color>,
    },
    /// An agent acting for a person.
    Agent {
        name: &'a str,
        for_person: Option<&'a str>,
        color: Color,
    },
    /// An installed bot, by its name and installer, with the appearance it
    /// showed, the Tilo person its persona is linked to, and the image its
    /// tile shows (the appearance's avatar, else the bot's icon).
    Bot {
        bot_name: &'a str,
        installed_by: &'a str,
        appearance: Option<&'a tilo_actions::Appearance>,
        linked: Option<&'a str>,
        image: Option<&'a ImageData>,
    },
}

/// Where an author line's parts go.
#[derive(Clone, Debug, PartialEq)]
pub struct AuthorLine {
    pub mark: Rect,
    pub name: Rect,
    /// The name as drawn: labelled and, if need be, elided.
    pub shown: String,
    /// The `BOT` chip, for a bot; the host opens the installation's page when
    /// it is clicked.
    pub chip: Option<Rect>,
}

/// Whether `a` and `b` would read as the same name: equal once reduced to a
/// confusable skeleton and case-folded.
///
/// The skeleton is a compact form of UTS #39's: compatibility forms and
/// accents are dropped, the Cyrillic and Greek letters that are drawn like
/// Latin ones become those, and `l`/`I`/`1`/`|`, `0`/`O` and `rn`/`m` fold
/// together. The full confusables table would add over 100 KiB to every web
/// bundle drawing an author, for scripts no look-alike of a room member's
/// Latin-script name is likely to use.
#[must_use]
pub fn looks_alike(a: &str, b: &str) -> bool {
    skeleton(a) == skeleton(b)
}

fn skeleton(text: &str) -> String {
    let folded: String = text
        .trim()
        .nfkd()
        .filter(|c| !('\u{300}'..='\u{36F}').contains(c))
        .map(|c| match c {
            'I' | '1' | '|' | '\u{399}' | '\u{406}' | '\u{4C0}' | '\u{3B9}' | '\u{456}'
            | '\u{4CF}' => 'l',
            '0' => 'o',
            other => CONFUSABLE
                .iter()
                .find(|(from, _)| *from == other)
                .map_or(other, |(_, to)| *to),
        })
        .flat_map(char::to_lowercase)
        .collect();
    folded.replace("rn", "m")
}

/// Cyrillic and Greek letters drawn like a Latin one, with that letter.
const CONFUSABLE: &[(char, char)] = &[
    ('\u{430}', 'a'),
    ('\u{410}', 'a'),
    ('\u{391}', 'a'),
    ('\u{3B1}', 'a'),
    ('\u{412}', 'b'),
    ('\u{392}', 'b'),
    ('\u{44C}', 'b'),
    ('\u{441}', 'c'),
    ('\u{421}', 'c'),
    ('\u{3F2}', 'c'),
    ('\u{501}', 'd'),
    ('\u{435}', 'e'),
    ('\u{415}', 'e'),
    ('\u{395}', 'e'),
    ('\u{4BB}', 'h'),
    ('\u{41D}', 'h'),
    ('\u{397}', 'h'),
    ('\u{458}', 'j'),
    ('\u{408}', 'j'),
    ('\u{41A}', 'k'),
    ('\u{39A}', 'k'),
    ('\u{3BA}', 'k'),
    ('\u{41C}', 'm'),
    ('\u{39C}', 'm'),
    ('\u{39D}', 'n'),
    ('\u{43E}', 'o'),
    ('\u{41E}', 'o'),
    ('\u{39F}', 'o'),
    ('\u{3BF}', 'o'),
    ('\u{440}', 'p'),
    ('\u{420}', 'p'),
    ('\u{3A1}', 'p'),
    ('\u{3C1}', 'p'),
    ('\u{51B}', 'q'),
    ('\u{455}', 's'),
    ('\u{405}', 's'),
    ('\u{422}', 't'),
    ('\u{3A4}', 't'),
    ('\u{3C5}', 'u'),
    ('\u{3BD}', 'v'),
    ('\u{51D}', 'w'),
    ('\u{445}', 'x'),
    ('\u{425}', 'x'),
    ('\u{3A7}', 'x'),
    ('\u{3C7}', 'x'),
    ('\u{443}', 'y'),
    ('\u{423}', 'y'),
    ('\u{3A5}', 'y'),
    ('\u{3B3}', 'y'),
    ('\u{396}', 'z'),
];

/// A service id shown as a word: `discord` → `Discord`.
fn service_label(service: &str) -> String {
    let mut chars = service.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

impl<'a> Author<'a> {
    /// The name the author is shown under.
    #[must_use]
    pub fn shown_name(&self) -> &'a str {
        match *self {
            Self::Person { name, .. } | Self::Agent { name, .. } => name,
            Self::Bot {
                bot_name,
                appearance,
                ..
            } => appearance.map_or(bot_name, |appearance| appearance.name.as_str()),
        }
    }

    #[must_use]
    pub fn is_bot(&self) -> bool {
        matches!(self, Self::Bot { .. })
    }

    /// The shown name among the room's current `members`: a bot's
    /// appearance that looks like one of them carries its persona's service
    /// (or, without one, the bot's name).
    #[must_use]
    pub fn label(&self, members: &[&str]) -> String {
        let Self::Bot {
            bot_name,
            appearance: Some(appearance),
            ..
        } = *self
        else {
            return self.shown_name().to_owned();
        };
        if !members
            .iter()
            .any(|member| looks_alike(member, &appearance.name))
        {
            return appearance.name.clone();
        }
        let service = appearance.persona.as_ref().map_or_else(
            || bot_name.to_owned(),
            |persona| service_label(&persona.service),
        );
        widget_format(
            "author-persona-suffix",
            &[
                ("name", appearance.name.as_str().into()),
                ("service", service.into()),
            ],
        )
    }

    /// What the chip says about a bot: the name it showed, the bot and who
    /// added it.
    #[must_use]
    pub fn details(&self) -> Option<String> {
        let Self::Bot {
            bot_name,
            installed_by,
            appearance,
            ..
        } = *self
        else {
            return None;
        };
        Some(match appearance {
            Some(appearance) => widget_format(
                "author-bot-details-as",
                &[
                    ("name", appearance.name.as_str().into()),
                    ("bot", bot_name.into()),
                    ("installer", installed_by.into()),
                ],
            ),
            None => widget_format(
                "author-bot-details",
                &[("bot", bot_name.into()), ("installer", installed_by.into())],
            ),
        })
    }

    /// The profile line for a persona linked to a Tilo person; the author is
    /// still the bot.
    #[must_use]
    pub fn linked_line(&self) -> Option<String> {
        let Self::Bot {
            linked: Some(person),
            ..
        } = *self
        else {
            return None;
        };
        Some(widget_format(
            "author-bot-linked",
            &[("person", person.into())],
        ))
    }

    /// Lay the author out in one line from `origin`, at most `max_width`
    /// wide. A bot's chip is placed first, so only the name gives way.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a line width in logical pixels fits f32"
    )]
    pub fn layout(
        &self,
        members: &[&str],
        origin: Point,
        max_width: f64,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> AuthorLine {
        let style = theme.text(TextRole::Body).strong();
        let side = ChipFace::height(theme);
        let gap = f64::from(theme.tokens.spacing.s2);
        let mark = Rect::from_origin_size(origin, (side, side));
        let chip_width = self.is_bot().then(|| {
            ChipFace::intrinsic_width(
                &widget_format("author-bot-chip", &[]),
                ChipLead::None,
                false,
                text,
                theme,
            )
        });
        let reserved = side + gap + chip_width.map_or(0.0, |width| width + gap);
        let room = (max_width - reserved).max(0.0);
        let shown = text.elide_to_width_style(&self.label(members), room as f32, &style);
        let (width, _) = text.measure_style(&shown, None, &style);
        let x = mark.x1 + gap;
        let name = Rect::new(x, origin.y, x + f64::from(width), origin.y + side);
        let chip = chip_width.map(|width| {
            let x = name.x1 + gap;
            Rect::new(x, origin.y, x + width, origin.y + side)
        });
        AuthorLine {
            mark,
            name,
            shown,
            chip,
        }
    }

    /// Paint a line [`Self::layout`] placed.
    pub fn paint(&self, line: &AuthorLine, ctx: &mut PaintCtx<'_>) {
        match *self {
            Self::Person {
                name,
                identity,
                avatar,
                color,
            } => {
                let face = AvatarFace::new(name, identity).image(avatar);
                let face = match color {
                    Some(color) => face.presence(color),
                    None => face,
                };
                face.paint(ctx, line.mark);
            }
            Self::Agent { color, .. } => draw_agent_identity_mark(
                ctx.scene,
                ctx.transform,
                line.mark,
                color,
                ctx.theme,
                AgentIdentityStyle::Solid,
            ),
            Self::Bot {
                bot_name, image, ..
            } => AvatarFace::new(self.shown_name(), bot_name)
                .square()
                .image(image)
                .paint(ctx, line.mark),
        }
        let style = ctx.theme.text(TextRole::Body).strong();
        let ink = ctx.theme.tokens.colors.text_primary;
        ctx.text(
            &line.shown,
            &style,
            ink,
            Point::new(line.name.x0, line.name.y0),
            None,
        );
        if let Some(chip) = line.chip {
            let label = widget_format("author-bot-chip", &[]);
            ChipFace::new(chip, &label).paint(ctx.scene, ctx.transform, ctx.text, ctx.theme);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::{Appearance, Persona};

    fn alice() -> Appearance {
        Appearance::new(
            "alice",
            None,
            Some(Persona {
                service: "discord".into(),
                account: "80351110224678912".into(),
            }),
        )
        .unwrap()
    }

    fn bridge(appearance: Option<&Appearance>) -> Author<'_> {
        Author::Bot {
            bot_name: "Discord Bridge",
            installed_by: "Ada",
            appearance,
            linked: None,
            image: None,
        }
    }

    #[test]
    fn a_bot_shows_its_appearance_name_or_its_own() {
        let alice = alice();
        assert_eq!(bridge(Some(&alice)).shown_name(), "alice");
        assert_eq!(bridge(None).shown_name(), "Discord Bridge");
        assert!(bridge(None).is_bot());
    }

    #[test]
    fn the_details_name_the_bot_and_who_added_it() {
        let alice = alice();
        assert_eq!(
            bridge(Some(&alice)).details().unwrap(),
            "alice, via Discord Bridge (bot, added by Ada)"
        );
        assert_eq!(
            bridge(None).details().unwrap(),
            "Discord Bridge (bot, added by Ada)"
        );
        let person = Author::Person {
            name: "Ada",
            identity: "ada",
            avatar: None,
            color: None,
        };
        assert_eq!(person.details(), None);
    }

    #[test]
    fn a_name_that_looks_like_a_members_carries_its_service() {
        let alice = alice();
        assert_eq!(bridge(Some(&alice)).label(&["Bo"]), "alice");
        assert_eq!(bridge(Some(&alice)).label(&["Alice"]), "alice · Discord");
        // Cyrillic "а" is a confusable of Latin "a".
        let cyrillic = Appearance::new("\u{430}lice", None, alice.persona.clone()).unwrap();
        assert_eq!(
            bridge(Some(&cyrillic)).label(&["alice"]),
            "\u{430}lice · Discord"
        );
        // Without a persona, the bot's own name is the suffix.
        let bare = Appearance::new("alice", None, None).unwrap();
        assert_eq!(
            bridge(Some(&bare)).label(&["Alice"]),
            "alice · Discord Bridge"
        );
        assert!(looks_alike("paypal", "pаypаl"));
        assert!(!looks_alike("alice", "bob"));
        assert!(looks_alike("Iris", "1ris"));
        assert!(looks_alike("modern", "modem"));
        assert!(looks_alike("Ren\u{e9}", "rene"));
        assert!(looks_alike("ＡＤＡ", "ada"));
    }

    #[test]
    fn the_bot_chip_stays_whole_inside_the_column_when_the_name_is_elided() {
        let mut text = TextRenderer::new();
        let theme = Theme::dark();
        let long = Appearance::new(
            "Alexandria Ocasio-Smithington the Third of Somewhere",
            None,
            None,
        )
        .unwrap();
        let author = bridge(Some(&long));
        let max = 160.0;
        let line = author.layout(&[], Point::ZERO, max, &mut text, &theme);
        let chip = line.chip.expect("a bot always has its chip");
        assert!(chip.x1 <= max + 0.5, "{chip:?}");
        assert!(chip.width() > 0.0);
        assert_ne!(line.shown, long.name, "the name is elided, not the chip");
        assert!(line.name.x1 <= chip.x0);

        let person = Author::Person {
            name: "Ada",
            identity: "ada",
            avatar: None,
            color: None,
        };
        assert!(
            person
                .layout(&[], Point::ZERO, max, &mut text, &theme)
                .chip
                .is_none()
        );
    }

    #[test]
    fn a_linked_persona_is_said_on_the_profile() {
        let alice = alice();
        let linked = Author::Bot {
            bot_name: "Discord Bridge",
            installed_by: "Ada",
            appearance: Some(&alice),
            linked: Some("Alice Smith"),
            image: None,
        };
        assert_eq!(
            linked.linked_line().unwrap(),
            "Linked to Alice Smith on Tilo"
        );
        assert_eq!(linked.shown_name(), "alice", "the author is still the bot");
    }
}
