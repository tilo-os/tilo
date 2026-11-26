//! `PeerPointers`: other people's pointers over the window of a place, each in its person's
//! colour, under their name, with what they say beside it and a reaction floating up from it.
//!
//! A shell lays it over a place's window: each pointer stands where its person put it across
//! that window, measured from the window's own well wherever the host placed one (the room the
//! widget fills may frame the well), else from the widget's own rectangle. The window is composited over a shell drawn under the
//! windows, so the pointers are drawn as a part over the windows ([`crate::lift::over_windows`]).
//! They take no input: a press where one stands is the window's. The widget reports what it
//! draws to the host's [`Wells`](super::Wells) whenever that changes, a pointer moving between
//! layouts included, so a host reads where the pointers stand from what was drawn. A pointer at rest asks for no frame; only a bubble or a
//! reaction on its way asks for the next.

use taffy::{NodeId, Style, TaffyTree};
use vello::kurbo::{Point, Rect};

use tilo_ui_base::component::{Component, Described};
use tilo_ui_base::permissions::MemberId;
use tilo_ui_base::presence::{CursorChats, PeerCursor};
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::{TaffyTreeExt, Widget};

/// Other people's pointers over the window of a place.
#[derive(Component, Default)]
#[component(
    name = "PeerPointers",
    role = "Group",
    not_in_gallery = "its pointers stand over a window a gallery has none of"
)]
pub struct PeerPointersProps {
    /// The place whose window they are over: the tab id the shell model gives the window.
    #[component(required, positional)]
    #[component(data)]
    pub place: String,
    /// The pointers, in the order the roster lists their people.
    #[component(part(default))]
    pub pointers: Vec<PeerPointerProps>,
}

/// One person's pointer over the window.
#[derive(Component, Default)]
#[component(
    name = "PeerPointer",
    role = "Image",
    label = "label",
    not_in_gallery = "it stands only among a window's pointers, over a window a gallery has none of"
)]
pub struct PeerPointerProps {
    /// Whose it is: their presence key, which keeps what they say with them.
    #[component(data)]
    pub person: String,
    /// What it says to a person who cannot see it: who it is and the window it is over; empty
    /// says nothing.
    #[component(required, positional)]
    pub label: String,
    /// The name beside it.
    pub name: String,
    /// Their colour, `rrggbb`.
    #[component(data)]
    pub colour: String,
    /// Where across the window, logical pixels from its left edge.
    pub x: f64,
    /// Where down the window, logical pixels from its top edge.
    pub y: f64,
    /// It is software's: its pointer is drawn as software's.
    pub agent: bool,
    /// What they say beside it; empty for nothing.
    pub said: String,
    /// What they say is still being typed.
    pub typing: bool,
    /// The emoji they reacted with last; empty for none.
    pub reaction: String,
    /// Which of their reactions it is: a new one plays when this moves.
    pub reaction_seq: i64,
}

tilo_ui_base::part_type!(PeerPointerProps);

/// One pointer as the widget holds it.
struct Pointer {
    user: MemberId,
    label: String,
    name: String,
    colour: vello::peniko::Color,
    at: (f64, f64),
    agent: bool,
}

/// See the module docs.
pub struct PeerPointers {
    place: String,
    pointers: Vec<Pointer>,
    /// What each says and reacted, playing on this widget's own clock.
    chats: CursorChats,
    rect: Rect,
    /// The host's wells, as of the last layout: where the window stands, and where what was drawn
    /// is reported.
    wells: Option<super::Wells>,
    layout_dirty: bool,
    /// Something changed that the last frame did not draw.
    dirty: bool,
}

impl PeerPointers {
    fn take(&mut self, props: PeerPointersProps) {
        let now = crate::time::now();
        if self.place != props.place {
            if let Some(wells) = &self.wells {
                wells.point(&self.place, self.rect, Vec::new());
            }
            self.place = props.place;
        }
        let mut pointers = Vec::with_capacity(props.pointers.len());
        for pointer in props.pointers {
            let user = MemberId::person(pointer.person.clone());
            let said = pointer.said.trim();
            self.dirty |= self.chats.say(
                &user,
                (!said.is_empty()).then_some((said, !pointer.typing)),
                now,
            );
            if let Ok(seq) = u32::try_from(pointer.reaction_seq) {
                self.dirty |= self.chats.react(&user, seq, &pointer.reaction, now);
            }
            pointers.push(Pointer {
                user,
                label: pointer.label,
                name: pointer.name,
                colour: crate::widget::avatar::colour_of(&pointer.colour)
                    .unwrap_or(vello::peniko::Color::from_rgb8(0x80, 0x80, 0x80)),
                at: (pointer.x, pointer.y),
                agent: pointer.agent,
            });
        }
        // Someone who left takes what they said with them.
        for gone in self
            .pointers
            .iter()
            .filter(|was| !pointers.iter().any(|now| now.user == was.user))
        {
            self.chats.forget(&gone.user);
        }
        let moved = pointers.len() != self.pointers.len()
            || pointers.iter().zip(&self.pointers).any(|(now, was)| {
                now.user != was.user
                    || now.at != was.at
                    || now.name != was.name
                    || now.colour != was.colour
                    || now.agent != was.agent
            });
        let relabelled = pointers.len() == self.pointers.len()
            && pointers
                .iter()
                .zip(&self.pointers)
                .any(|(now, was)| now.label != was.label);
        self.dirty |= moved || relabelled;
        self.pointers = pointers;
        self.report();
    }

    /// The window the pointers stand over: the place's well where the host placed one, else the
    /// widget's own rectangle.
    fn window(&self) -> Rect {
        self.wells
            .as_ref()
            .and_then(|wells| {
                wells
                    .well(&self.place)
                    .or_else(|| wells.well_anywhere(&self.place))
            })
            .unwrap_or(self.rect)
    }

    /// Tell the host's wells what is drawn now: each pointer where it stands across the window.
    fn report(&self) {
        if let Some(wells) = &self.wells {
            wells.point(
                &self.place,
                self.rect,
                self.pointers
                    .iter()
                    .map(|pointer| (pointer.name.clone(), pointer.at))
                    .collect(),
            );
        }
    }
}

impl Drop for PeerPointers {
    fn drop(&mut self) {
        if let Some(wells) = &self.wells {
            wells.point(&self.place, self.rect, Vec::new());
        }
    }
}

impl Described for PeerPointers {
    type Props = PeerPointersProps;

    fn from_props(_key: &str, props: PeerPointersProps) -> Self {
        let mut pointers = Self {
            place: props.place.clone(),
            pointers: Vec::new(),
            chats: CursorChats::default(),
            rect: Rect::ZERO,
            wells: None,
            layout_dirty: true,
            dirty: true,
        };
        pointers.take(props);
        pointers
    }

    fn set_props(&mut self, props: PeerPointersProps) -> bool {
        self.take(props);
        true
    }
}

impl Widget for PeerPointers {
    fn name(&self) -> &'static str {
        "PeerPointers"
    }

    /// Each pointer given words says them: whose it is and the window it is over. One given none
    /// is drawn all the same and says nothing.
    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        let window = self.window();
        Some(
            tilo_ui_base::a11y::AccessNode::new(tilo_ui_base::a11y::Role::Group).with_children(
                self.pointers
                    .iter()
                    .filter(|pointer| !pointer.label.trim().is_empty())
                    .map(|pointer| {
                        tilo_ui_base::a11y::AccessNode::new(tilo_ui_base::a11y::Role::Image)
                            .with_name(pointer.label.clone())
                            .with_bounds(Rect::from_center_size(
                                Point::new(window.x0 + pointer.at.0, window.y0 + pointer.at.1),
                                (16.0, 16.0),
                            ))
                    })
                    .collect(),
            ),
        )
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(
            Style {
                size: taffy::Size::from_percent(1.0, 1.0),
                ..Style::default()
            },
            node_map,
            self,
        )
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        if let Some(wells) = super::window_well::current() {
            // Laid out for another part now (the display it stands on came, or changed): what it
            // reported for the part before stands nowhere.
            if let Some(before) = self.wells.as_ref().filter(|before| !before.is(&wells)) {
                before.point(&self.place, self.rect, Vec::new());
            }
            self.wells = Some(wells);
        }
        self.layout_dirty = false;
        self.report();
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.dirty = false;
        let now = crate::time::now();
        self.chats.prune(now);
        let window = self.window();
        if self.pointers.is_empty() || window.width() <= 0.0 || window.height() <= 0.0 {
            return;
        }
        let motion = theme.motion();
        let cursors: Vec<PeerCursor> = self
            .pointers
            .iter()
            .map(|pointer| PeerCursor {
                pos: (window.x0 + pointer.at.0, window.y0 + pointer.at.1),
                label: pointer.name.clone(),
                color: pointer.colour,
                is_agent: pointer.agent,
                opacity: 1.0,
                name_opacity: 1.0,
                chat: self.chats.bubble(&pointer.user, now, motion),
                reaction: self.chats.reaction(&pointer.user, now, motion),
            })
            .collect();
        let area = window;
        // A pointer at a window's edge keeps its halo, and a reaction rising from one at the top
        // keeps its float: the part may paint a little past the window.
        let bounds = area.inflate(32.0, 96.0);
        crate::lift::over_windows(
            scene,
            transform,
            (
                tilo_ui_base::scene_parts::PartKey::new("peer-pointers", &self.place),
                bounds,
            ),
            |scene, transform| {
                tilo_ui_base::presence::draw_peer_cursors(
                    scene, transform, area, text, theme, &cursors,
                );
            },
        );
    }

    /// A pointer at rest asks for no frame: only something moved, or a bubble or a reaction on its
    /// way, does.
    fn needs_redraw(&self) -> bool {
        self.dirty || self.chats.animating(crate::time::now())
    }

    fn is_layout_dirty(&self) -> bool {
        self.layout_dirty
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    tilo_ui_base::described!();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pointer(key: &str, x: f64, said: &str) -> PeerPointerProps {
        PeerPointerProps {
            person: key.to_owned(),
            label: format!("{key} is in Notes"),
            name: key.to_owned(),
            colour: "3366cc".to_owned(),
            x,
            y: 20.0,
            said: said.to_owned(),
            ..PeerPointerProps::default()
        }
    }

    /// Pointers at rest ask for no frame; one that moves asks for one, and what someone says plays
    /// until it has arrived.
    #[test]
    fn pointers_at_rest_ask_for_no_frame() {
        let _settled = tilo_ui_base::motion::testing::settled();
        let mut pointers = PeerPointers::from_props(
            "",
            PeerPointersProps {
                place: "7".to_owned(),
                pointers: vec![pointer("Ada", 10.0, "")],
            },
        );
        pointers.dirty = false;
        assert!(
            !pointers.needs_redraw(),
            "a pointer at rest draws nothing again"
        );
        pointers.set_props(PeerPointersProps {
            place: "7".to_owned(),
            pointers: vec![pointer("Ada", 10.0, "")],
        });
        assert!(
            !pointers.needs_redraw(),
            "the same pointer again is no change"
        );
        pointers.set_props(PeerPointersProps {
            place: "7".to_owned(),
            pointers: vec![pointer("Ada", 40.0, "")],
        });
        assert!(
            pointers.needs_redraw(),
            "a pointer that moved is drawn where it went"
        );
        pointers.dirty = false;
        pointers.set_props(PeerPointersProps {
            place: "7".to_owned(),
            pointers: vec![pointer("Ada", 40.0, "hello")],
        });
        assert!(pointers.needs_redraw(), "what they say is drawn beside it");
    }
}
