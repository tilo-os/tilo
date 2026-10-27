//! The people drawer: the room's people and agents with their presence and
//! the voice channel each is in, and the pinned messages beside them.

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use ui::chrome::{NavSectionFace, nav_section_height};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{AvatarFace, EmptyState, EmptyStateContent, ListRowFace, RowLeading};

use super::{avatar_status, clock_label, day_label, presence_label};
use crate::app::{ChatApp, Hit};
use crate::model::{Message, Presence, User};
use crate::paint::push_clip;
use ui::chrome;

impl ChatApp {
    /// The workspace's people (and agents) with their presence and, when
    /// connected, the voice channel they are in.
    pub(crate) fn draw_members(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let panel = self.thread_rect();
        let head = self.draw_panel_header(
            PaintCtx::new(scene, tf, tr, theme),
            panel,
            self.roster_label(),
            Hit::ToggleRightFlap,
            hits,
        );
        let rows = self.roster_rows();
        let body = Rect::new(panel.x0, head.y1, panel.x1, panel.y1);
        push_clip(scene, tf, body);
        let inset = f64::from(theme.tokens.spacing.s2);
        let mut y = body.y0;
        let mut ctx = PaintCtx::new(scene, tf, tr, theme);
        // Who is around first, then everyone else, each under a count.
        let here = |person: &User, voice: &Option<String>| {
            voice.is_some() || person.status != Presence::Offline
        };
        let online = rows.iter().filter(|(p, v)| here(p, v)).count();
        let sections = [
            (format!("Online — {online}"), true),
            (format!("Offline — {}", rows.len() - online), false),
        ];
        for (index, (title, present)) in sections.iter().enumerate() {
            let members: Vec<_> = rows
                .iter()
                .filter(|(p, v)| here(p, v) == *present)
                .collect();
            if members.is_empty() {
                continue;
            }
            let section_h = nav_section_height(theme, index == 0 || online == 0);
            chrome::paint_nav_section(
                ctx.scene,
                tf,
                ctx.text,
                theme,
                &NavSectionFace::new(Rect::new(body.x0, y, body.x1, y + section_h), title),
            );
            y += section_h;
            for (person, voice) in members {
                if y > panel.y1 {
                    break;
                }
                // Named as the room names them, numbered when shared: the
                // same label as their lines in the timeline.
                let display = if person.is_agent && !self.is_me(&person.id) {
                    format!("{} (agent)", person.name)
                } else {
                    self.user_name(&person.id)
                };
                // A plain "Online" under every name says nothing the section
                // has not; where someone is, or that they are away, does.
                let status = match voice {
                    Some(ch) => format!("In {ch}"),
                    None if matches!(person.status, Presence::Idle | Presence::Dnd) => {
                        presence_label(person.status).to_owned()
                    }
                    None => String::new(),
                };
                let name = self.user_name(&person.id);
                let mut face = ListRowFace::new(&display)
                    .leading(RowLeading::Avatar(
                        AvatarFace::new(&name, &person.id)
                            .presence(self.identity_colour(&person.id))
                            .status(avatar_status(person.status)),
                    ))
                    .enabled(*present || !person.is_agent)
                    .selected(self.revealed_person.as_deref() == Some(person.id.as_str()));
                if !status.is_empty() {
                    face = face.secondary(&status);
                }
                let row_h = face.height(ctx.text, theme);
                let row = Rect::new(body.x0 + inset, y, body.x1 - inset, y + row_h);
                face.interaction(ui::widget::Interaction {
                    hovered: !person.is_agent && self.hovered(row),
                    ..ui::widget::Interaction::default()
                })
                .paint(&mut ctx, row);
                // A person's row opens their card; an agent has none.
                if !person.is_agent {
                    ChatApp::push_hit(
                        hits,
                        row,
                        Hit::ShowPerson {
                            person: person.id.clone(),
                            place: format!("member:{}", person.id),
                        },
                    );
                }
                y += row_h;
            }
        }
        scene.pop_layer();
    }

    /// The open channel's pinned messages, newest first, in the detail
    /// drawer: each opens where it was written.
    pub(crate) fn draw_pins(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let panel = self.thread_rect();
        let head = self.draw_panel_header(
            PaintCtx::new(scene, tf, tr, theme),
            panel,
            &crate::i18n::t("chat-pins"),
            Hit::TogglePins,
            hits,
        );
        let body = Rect::new(panel.x0, head.y1, panel.x1, panel.y1);
        let pinned: Vec<&Message> = self.messages.iter().rev().filter(|m| m.pinned).collect();
        let mut ctx = PaintCtx::new(scene, tf, tr, theme);
        if pinned.is_empty() {
            let title = crate::i18n::t("chat-pins-empty");
            let message = crate::i18n::t("chat-pins-empty-detail");
            let content = EmptyStateContent {
                icon: Some(Icon::Pin),
                message: Some(&message),
                ..EmptyStateContent::new(&title)
            };
            EmptyState::paint(&mut ctx, body, &content, self.last_mouse);
            return;
        }
        push_clip(ctx.scene, tf, body);
        let inset = f64::from(theme.tokens.spacing.s2);
        let mut y = body.y0 + inset;
        for m in pinned {
            if y > body.y1 {
                break;
            }
            let name = self.user_name(&m.author);
            let when = format!("{} — {}", day_label(m.ts), clock_label(m.ts));
            let face = ListRowFace::new(&name)
                .meta(&when)
                .secondary(&m.text)
                .two_line_secondary()
                .leading(RowLeading::Avatar(
                    AvatarFace::new(&name, &m.author).presence(self.identity_colour(&m.author)),
                ));
            let row_h = face.height(ctx.text, theme);
            let row = Rect::new(body.x0 + inset, y, body.x1 - inset, y + row_h);
            let hovered = self.hovered(row);
            face.interaction(ui::widget::Interaction {
                hovered,
                ..ui::widget::Interaction::default()
            })
            .paint(&mut ctx, row);
            ChatApp::push_hit(hits, row, Hit::RevealMessage(m.id.clone()));
            y += row_h;
        }
        ctx.scene.pop_layer();
    }
}
