//! Text typed in place: a caret in a text mark on the canvas, the words
//! changing as they are typed, and one `draw` or `edit-marks` when the
//! typing is done.

use std::borrow::Cow;
use ui::tilo_resource::Invoke;

use ui::winit::event::Ime;
use ui::winit::keyboard::{Key, ModifiersState, NamedKey};
use ui::{EditCore, Motion};

use crate::actions::{Draw, EditMarks, NewMark, RemoveMarks};
use crate::model::{Artwork, Shape};

/// A text being typed.
#[derive(Clone, Debug)]
pub struct TextSession {
    /// The mark being retyped, or the id the new one will have.
    pub id: String,
    /// Whether the mark is new: typing makes it.
    pub new: bool,
    /// The text shape as typed so far.
    pub shape: Shape,
    pub caret: EditCore,
    /// The colour a new text is drawn in, and the layer it goes on.
    pub fill: String,
    pub layer: Option<String>,
}

/// What a key did to the typing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Typed {
    /// The text or caret changed, or the key was the editor's anyway.
    Edited,
    /// The typing is done: commit it.
    Done,
    /// Not a key for the editor.
    Ignored,
}

impl TextSession {
    /// A new text at `(x, y)`, its first baseline there.
    pub fn new(
        id: String,
        (x, y): (f64, f64),
        size: f64,
        fill: String,
        layer: Option<String>,
    ) -> Self {
        Self {
            id,
            new: true,
            shape: Shape::Text {
                x,
                y,
                text: String::new(),
                size,
                font: None,
                weight: None,
                italic: false,
                align: None,
                line_height: None,
                width: None,
                tracking: None,
                path: None,
            },
            caret: EditCore::new(),
            fill,
            layer,
        }
    }

    /// Retype the text mark `id`, the caret at its end.
    pub fn edit(art: &Artwork, id: &str) -> Option<Self> {
        let mark = art.mark(id)?;
        let Shape::Text { text, .. } = &mark.shape else {
            return None;
        };
        Some(Self {
            id: id.to_owned(),
            new: false,
            shape: mark.shape.clone(),
            caret: EditCore::at(text.len()),
            fill: mark.style.fill.clone().unwrap_or_default(),
            layer: Some(mark.layer.clone()),
        })
    }

    pub fn text(&self) -> &str {
        match &self.shape {
            Shape::Text { text, .. } => text,
            _ => "",
        }
    }

    /// The text as shown: what is typed, with any input-method composition
    /// at the caret.
    pub fn shown_text(&self) -> Cow<'_, str> {
        self.caret.display_text(self.text())
    }

    /// The text shape as shown, composition included.
    pub fn shown_shape(&self) -> Shape {
        let mut shape = self.shape.clone();
        if self.caret.has_preedit()
            && let Shape::Text { text, .. } = &mut shape
        {
            *text = self.caret.display_text(text).into_owned();
        }
        shape
    }

    /// An input-method event: the composition shows at the caret and is
    /// typed in only when committed.
    pub fn ime(&mut self, ime: &Ime) {
        match ime {
            Ime::Preedit(text, cursor) if !text.is_empty() => {
                self.caret.set_preedit(text, *cursor);
            }
            Ime::Commit(text) => {
                self.caret.clear_preedit();
                self.insert(text);
            }
            Ime::Preedit(..) | Ime::Disabled => self.caret.clear_preedit(),
            Ime::Enabled => {}
        }
    }

    fn apply(&mut self, edit: &ui::Edit) {
        if let Shape::Text { text, .. } = &mut self.shape {
            edit.apply(text);
        }
    }

    /// Type `typed` at the caret.
    pub fn insert(&mut self, typed: &str) {
        let typed: String = typed
            .chars()
            .filter(|ch| *ch == '\n' || !ch.is_control())
            .collect();
        if typed.is_empty() || self.text().chars().count() >= 2000 {
            return;
        }
        let edit = self.caret.insert(&typed);
        self.apply(&edit);
    }

    /// A key pressed while typing; `typed` is the text it produces.
    pub fn key(&mut self, key: &Key, typed: Option<&str>, modifiers: ModifiersState) -> Typed {
        let word = modifiers.control_key();
        let extend = modifiers.shift_key();
        let text = self.text().to_owned();
        let motion = |motion: Motion, caret: &mut EditCore| {
            caret.motion(&text, motion, extend);
            Typed::Edited
        };
        match key {
            Key::Named(NamedKey::Escape) => Typed::Done,
            Key::Named(NamedKey::Enter) if modifiers.control_key() => Typed::Done,
            Key::Named(NamedKey::Enter) => {
                self.insert("\n");
                Typed::Edited
            }
            Key::Named(NamedKey::Backspace) => {
                let edit = self.caret.delete_backward(&text, word);
                self.apply(&edit);
                Typed::Edited
            }
            Key::Named(NamedKey::Delete) => {
                let edit = self.caret.delete_forward(&text, word);
                self.apply(&edit);
                Typed::Edited
            }
            Key::Named(NamedKey::ArrowLeft) => motion(
                if word { Motion::WordLeft } else { Motion::Left },
                &mut self.caret,
            ),
            Key::Named(NamedKey::ArrowRight) => motion(
                if word {
                    Motion::WordRight
                } else {
                    Motion::Right
                },
                &mut self.caret,
            ),
            Key::Named(NamedKey::ArrowUp) => motion(Motion::LineUp, &mut self.caret),
            Key::Named(NamedKey::ArrowDown) => motion(Motion::LineDown, &mut self.caret),
            Key::Named(NamedKey::Home) => motion(Motion::LineStart, &mut self.caret),
            Key::Named(NamedKey::End) => motion(Motion::LineEnd, &mut self.caret),
            Key::Character(ch) if modifiers.control_key() && ch.as_str() == "a" => {
                self.caret.select_all(&text);
                Typed::Edited
            }
            _ if modifiers.control_key() || modifiers.super_key() => Typed::Ignored,
            Key::Named(NamedKey::Space) => {
                self.insert(" ");
                Typed::Edited
            }
            _ => match typed.or(match key {
                Key::Character(ch) => Some(ch.as_str()),
                _ => None,
            }) {
                Some(typed) if !typed.is_empty() => {
                    self.insert(typed);
                    Typed::Edited
                }
                _ => Typed::Ignored,
            },
        }
    }

    /// The edit typing made: a new text drawn, an old one retyped, or an
    /// emptied one removed. `None` when nothing changed.
    pub fn commit(&self, art: &Artwork) -> Option<Invoke<Artwork>> {
        let empty = self.text().trim().is_empty();
        if self.new {
            return (!empty).then(|| {
                Artwork::DRAW.with(Draw {
                    layer: self.layer.clone(),
                    marks: vec![NewMark {
                        id: Some(self.id.clone()),
                        fill: Some(self.fill.clone()),
                        ..NewMark::new(self.shape.clone())
                    }],
                })
            });
        }
        let before = art.mark(&self.id)?;
        if empty {
            return Some(Artwork::REMOVE_MARKS.with(RemoveMarks {
                ids: vec![self.id.clone()],
                ..RemoveMarks::default()
            }));
        }
        (before.shape != self.shape).then(|| {
            Artwork::EDIT_MARKS.with(EditMarks {
                ids: vec![self.id.clone()],
                shape: Some(crate::actions::Taught(self.shape.clone())),
                ..EditMarks::default()
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_makes_one_draw_and_retyping_one_edit() {
        let mut session =
            TextSession::new("t-1".into(), (10.0, 50.0), 32.0, "#000000".into(), None);
        for key in ["H", "i", "!"] {
            session.key(
                &Key::Character(key.into()),
                Some(key),
                ModifiersState::empty(),
            );
        }
        session.key(
            &Key::Named(NamedKey::Backspace),
            None,
            ModifiersState::empty(),
        );
        session.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty());
        session.key(
            &Key::Character("x".into()),
            Some("x"),
            ModifiersState::empty(),
        );
        assert_eq!(session.text(), "Hi\nx");
        assert_eq!(
            session.key(&Key::Named(NamedKey::Escape), None, ModifiersState::empty()),
            Typed::Done
        );
        let art = Artwork::default();
        let Some(draw) = (session.commit(&art)).and_then(|invoke| {
            crate::actions::read_as::<crate::actions::Draw>(&invoke, Artwork::DRAW.id)
        }) else {
            panic!("a draw")
        };
        assert_eq!(draw.marks[0].id.as_deref(), Some("t-1"));

        let empty = TextSession::new("t-2".into(), (0.0, 0.0), 10.0, "#000000".into(), None);
        assert!(empty.commit(&art).is_none(), "nothing typed draws nothing");
    }

    #[test]
    fn composition_shows_until_committed_and_clusters_delete_whole() {
        let mut session = TextSession::new("t-1".into(), (0.0, 0.0), 12.0, "#000000".into(), None);
        let none = ModifiersState::empty();
        session.key(&Key::Character("a".into()), Some("a"), none);
        session.ime(&Ime::Preedit("かな".into(), Some((3, 3))));
        assert_eq!(session.text(), "a", "a composition is not typed yet");
        assert_eq!(session.shown_text(), "aかな");
        assert!(matches!(
            session.shown_shape(),
            Shape::Text { ref text, .. } if text == "aかな"
        ));
        assert_eq!(session.caret.display_caret(session.text()), "aか".len());
        session.ime(&Ime::Commit("仮名".into()));
        assert_eq!(session.text(), "a仮名");
        assert_eq!(session.shown_text(), "a仮名");

        for typed in ["e\u{301}", "👍🏽", "🇩🇪"] {
            session.ime(&Ime::Commit(typed.into()));
        }
        assert_eq!(session.text(), "a仮名e\u{301}👍🏽🇩🇪");
        let backspace = Key::Named(NamedKey::Backspace);
        session.key(&backspace, None, none);
        assert_eq!(session.text(), "a仮名e\u{301}👍🏽");
        session.key(&backspace, None, none);
        session.key(&backspace, None, none);
        assert_eq!(session.text(), "a仮名");

        // A commit replaces the selection, as typing does.
        session.key(
            &Key::Named(NamedKey::ArrowLeft),
            None,
            ModifiersState::SHIFT,
        );
        session.key(
            &Key::Named(NamedKey::ArrowLeft),
            None,
            ModifiersState::SHIFT,
        );
        session.ime(&Ime::Preedit("x".into(), None));
        session.ime(&Ime::Disabled);
        assert_eq!(
            session.shown_text(),
            "a仮名",
            "an abandoned composition leaves nothing"
        );
        session.ime(&Ime::Commit("x".into()));
        assert_eq!(session.text(), "ax");
    }
}
