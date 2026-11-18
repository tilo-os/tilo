//! The types Camera's rows are items of: a row says what it is — its type's
//! noun and the app's name — so search and an agent name it as they name a
//! resource.

use ui::tilo_resource::words::RowType;

pub(crate) struct Capture;

impl RowType for Capture {
    const KIND: &'static str = "camera.capture";
    const LABEL: &'static str = "Capture";
}
