//! Where other members are in a document a surface draws, as the surface places them: a point in
//! the document's own units and the rectangle they selected around it.
//!
//! An app declares its own place with `presence`; the platform says it and hands everyone else's
//! back as `spots(r)`. A surface that draws the document — a canvas, a timeline, a sheet — takes
//! these and places them on its geometry, so the members at work there are drawn by one shared
//! mark wherever the document is shown, never by each app's own copy.

use tilo_ui_graph::component::{CaseDescription, EnumDescription};

use tilo_ui_base::component::{PropType, Type, Value, case_field_description, wrong};
use tilo_ui_base::presence::{identity_rgb, presence_colour};
use tilo_ui_base::tokens::PresenceHue;
use vello::peniko::Color;

/// One member's place in a document: where they point and what they selected there.
#[derive(Clone, Debug, PartialEq)]
pub struct PeerSpot {
    /// Whose place it is.
    pub who: tilo_member_types::Actor,
    /// What the workspace calls them, for the chip above their caret.
    pub name: String,
    /// The presence palette slot they wear, where the workspace gave them one.
    pub hue: Option<u8>,
    /// Across the document, in its own units: where they point.
    pub x: f64,
    /// Down the document, in its own units.
    pub y: f64,
    /// Across the document: where what they selected begins.
    pub sx: f64,
    /// Down the document: where what they selected begins.
    pub sy: f64,
    /// How wide what they selected is, in the same units; zero with `tall` for none.
    pub wide: f64,
    /// How tall what they selected is, in the same units.
    pub tall: f64,
}

impl PeerSpot {
    /// Their colour: the slot the workspace gave them, else the palette's for who they are.
    #[must_use]
    pub fn color(&self) -> Color {
        let hue = self.hue.map_or_else(
            || PresenceHue::of_member(&self.who),
            |slot| PresenceHue::from_slot(usize::from(slot)),
        );
        presence_colour(identity_rgb(hue))
    }

    /// Whether the place is software's.
    #[must_use]
    pub fn is_agent(&self) -> bool {
        self.who
            .member_id()
            .is_none_or(|member| !member.is_person())
    }

    /// What they selected, when they selected anything: the rectangle's top left and its size,
    /// in the document's own units.
    #[must_use]
    pub fn selection(&self) -> Option<(f64, f64, f64, f64)> {
        (self.wide != 0.0 || self.tall != 0.0).then_some((self.sx, self.sy, self.wide, self.tall))
    }
}

impl PropType for PeerSpot {
    fn ty() -> Type {
        Type::Cases {
            name: "SpotPeer".to_owned(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Case { case, fields } = value else {
            return Err(wrong(&Self::ty(), value));
        };
        if case != "peer" {
            return Err(format!("a spot's peer is a `peer`, not `{case}`"));
        }
        let field = |name: &str| {
            fields
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value)
        };
        let number = |name: &str| match field(name) {
            Some(Value::Float(value)) => Ok(*value),
            Some(Value::None) | None => Ok(0.0),
            Some(other) => Err(format!("`{name}` is a number, not {other:?}")),
        };
        let (who, name, hue) = match field("who") {
            Some(Value::Actor {
                identity,
                name,
                hue,
                ..
            }) => (identity.clone(), name.clone(), *hue),
            _ => return Err("`peer` needs `who`, the member it is of".to_owned()),
        };
        Ok(Self {
            who,
            name,
            hue,
            x: number("x")?,
            y: number("y")?,
            sx: number("sx")?,
            sy: number("sy")?,
            wide: number("wide")?,
            tall: number("tall")?,
        })
    }
}

/// The description of [`PeerSpot`], as the catalogue lists it in its `enums`.
#[must_use]
pub fn peer_spot_description() -> EnumDescription {
    EnumDescription {
        name: "SpotPeer".to_owned(),
        doc:
            "Another member's place in a document a surface draws: where they point and what they \
              selected there, in the document's own units."
                .to_owned(),
        cases: vec![CaseDescription {
            name: "peer".to_owned(),
            doc: "One member at work in the document.".to_owned(),
            fields: vec![
                case_field_description("who", Type::Actor, None, "Whose place it is."),
                case_field_description(
                    "x",
                    Type::Float,
                    None,
                    "Across the document: where they point.",
                ),
                case_field_description("y", Type::Float, None, "Down the document."),
                case_field_description(
                    "sx",
                    Type::Float,
                    None,
                    "Across the document: where what they selected begins.",
                ),
                case_field_description(
                    "sy",
                    Type::Float,
                    None,
                    "Down the document: where what they selected begins.",
                ),
                case_field_description(
                    "wide",
                    Type::Float,
                    Some(Value::Float(0.0)),
                    "How wide their selection is; zero with `tall` for none.",
                ),
                case_field_description(
                    "tall",
                    Type::Float,
                    Some(Value::Float(0.0)),
                    "How tall their selection is.",
                ),
            ],
        }],
    }
}
