//! A derived record describes its stored layout.

use tilo_resource::{Collection, FieldShape, Json, Model, Resource, Shape, resource};

#[derive(Model, Default)]
pub struct Board {
    pub title: String,
    #[model(rename = "cols")]
    pub columns: Collection<Column>,
    pub archived: Option<Meta>,
    pub filter: Json<Vec<String>>,
    #[model(skip)]
    pub scroll: f64,
}

#[derive(Model, Default)]
pub struct Column {
    pub name: String,
    pub limit: u32,
}

#[derive(Model, Default)]
pub struct Meta {
    pub by: String,
}

#[resource(kind = "test.board", label = "Board")]
impl Board {
    /// Rename the board.
    #[action]
    pub fn rename(&mut self, title: String) {
        self.title = title;
    }
}

#[test]
fn a_derived_record_lists_its_stored_fields() {
    const COLUMN: Shape = Shape::Record(&[
        FieldShape {
            key: "name",
            shape: Shape::Value,
        },
        FieldShape {
            key: "limit",
            shape: Shape::Value,
        },
    ]);
    const META: Shape = Shape::Record(&[FieldShape {
        key: "by",
        shape: Shape::Value,
    }]);
    assert_eq!(
        Board::shape(),
        Some(Shape::Record(&[
            FieldShape {
                key: "title",
                shape: Shape::Value,
            },
            FieldShape {
                key: "cols",
                shape: Shape::Collection(&COLUMN),
            },
            FieldShape {
                key: "archived",
                shape: META,
            },
            FieldShape {
                key: "filter",
                shape: Shape::Value,
            },
        ])),
    );
}
