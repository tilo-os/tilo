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
            name: "name",
            title: "Name",
            shape: Shape::Value,
            unit: false,
            readonly: false,
            label: false,
        },
        FieldShape {
            key: "limit",
            name: "limit",
            title: "Limit",
            shape: Shape::Value,
            unit: false,
            readonly: false,
            label: false,
        },
    ]);
    const META: Shape = Shape::Record(&[FieldShape {
        key: "by",
        name: "by",
        title: "By",
        shape: Shape::Value,
        unit: false,
        readonly: false,
        label: false,
    }]);
    assert_eq!(
        Board::shape(),
        Some(Shape::Record(&[
            FieldShape {
                key: "title",
                name: "title",
                title: "Title",
                shape: Shape::Value,
                unit: false,
                readonly: false,
                label: false,
            },
            FieldShape {
                key: "cols",
                name: "columns",
                title: "Columns",
                shape: Shape::Collection(&COLUMN),
                unit: false,
                readonly: false,
                label: false,
            },
            FieldShape {
                key: "archived",
                name: "archived",
                title: "Archived",
                shape: META,
                unit: false,
                readonly: false,
                label: false,
            },
            FieldShape {
                key: "filter",
                name: "filter",
                title: "Filter",
                shape: Shape::Value,
                unit: false,
                readonly: false,
                label: false,
            },
        ])),
    );
}
