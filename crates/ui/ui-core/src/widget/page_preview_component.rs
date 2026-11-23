//! [`PagePreview`] as a described component.

use crate::component::{Component, Described};

use super::PagePreview;

/// A page's shape in a print queue, a scan or a picker: a sheet with the lines a page has and
/// "Page n of m" under it, showing the shape and never the content.
#[derive(Component, Default)]
#[component(name = "PagePreview", role = "Image", label = "label")]
pub struct PagePreviewProps {
    /// What the page is: its file's name, for assistive technology.
    #[component(required, positional)]
    pub label: String,
    /// Which page this is, counted from 1 (clamped into the count).
    #[component(required)]
    pub page: i64,
    /// How many pages the document has.
    #[component(required)]
    pub page_count: i64,
}

impl PagePreviewProps {
    fn counted(&self) -> (String, u32, u32) {
        let count = u32::try_from(self.page_count).unwrap_or(1).max(1);
        let page = u32::try_from(self.page.max(0))
            .unwrap_or(u32::MAX)
            .clamp(1, count);
        (self.label.clone(), page, count)
    }
}

impl Described for PagePreview {
    type Props = PagePreviewProps;

    fn from_props(_key: &str, props: PagePreviewProps) -> Self {
        let (label, page, count) = props.counted();
        Self::new(label, page, count)
    }

    fn set_props(&mut self, props: PagePreviewProps) -> bool {
        let (_, page, count) = props.counted();
        let changed = self.page() != page || self.page_count() != count;
        if changed {
            let label = props.label;
            *self = Self::new(label, page, count);
        }
        true
    }
}
