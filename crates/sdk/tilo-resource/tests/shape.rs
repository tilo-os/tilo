use tilo_resource::{Collection, FieldShape, Model, Resource, Shape, resource};

#[derive(Model, Default)]
pub struct Board {
    pub title: String,
    #[model(rename = "cols")]
    pub columns: Collection<Column>,
    pub archived: Option<Meta>,
    pub filter: serde_json::Value,
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
/// A stored scalar field with every flag at its ordinary value: the
/// fields `Board`'s shape spells out differ from it only where they are
/// named.
const SCALAR: FieldShape = FieldShape {
    key: "",
    owner: "",
    name: "",
    ty: "",
    title: "",
    shape: Shape::Value,
    unit: false,
    readonly: false,
    atom: false,
    label: false,
    label_part: 0,
    quiet_remove: false,
    trust: tilo_resource::Trust::Ordinary,
    schema: <String as tilo_resource::Model>::value_schema,
    field_type: <String as tilo_resource::Model>::field_type,
    text: tilo_resource::stored_text::<String>,
    absent: tilo_resource::absent_scalar::<String>,
    resource: None,
};

#[test]
fn a_derived_record_lists_its_stored_fields() {
    const COLUMN: Shape = Shape::Record(&[
        FieldShape {
            key: "name",
            owner: "column",
            name: "name",
            ty: "String",
            title: "Name",
            ..SCALAR
        },
        FieldShape {
            key: "limit",
            owner: "column",
            name: "limit",
            ty: "u32",
            title: "Limit",
            ..SCALAR
        },
    ]);
    const META: Shape = Shape::Record(&[FieldShape {
        key: "by",
        owner: "meta",
        name: "by",
        ty: "String",
        title: "By",
        ..SCALAR
    }]);
    assert_eq!(
        Board::shape(),
        Some(Shape::Record(&[
            FieldShape {
                key: "title",
                owner: "board",
                name: "title",
                ty: "String",
                title: "Title",
                ..SCALAR
            },
            FieldShape {
                key: "cols",
                owner: "board",
                name: "columns",
                ty: "Collection<Column>",
                title: "Columns",
                shape: Shape::Collection(&COLUMN),
                ..SCALAR
            },
            FieldShape {
                key: "archived",
                owner: "board",
                name: "archived",
                ty: "Option<Meta>",
                title: "Archived",
                shape: META,
                ..SCALAR
            },
            FieldShape {
                key: "filter",
                owner: "board",
                name: "filter",
                ty: "serde_json::Value",
                title: "Filter",
                ..SCALAR
            },
        ])),
    );
}
