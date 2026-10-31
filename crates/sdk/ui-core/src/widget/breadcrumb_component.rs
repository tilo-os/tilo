//! [`Breadcrumb`] and its segments as described components.

use crate::component::{Component, Described, Event, ResourceId};
use crate::icons::Icon;

use super::Breadcrumb;

/// Where a page is, as the path to it: each place it is in, a chevron between them, the page
/// itself last. Pressing a place goes there. Where the path does not fit, its middle gives way
/// first, the first place and the page itself staying.
#[derive(Component, Default)]
#[component(name = "Breadcrumb", role = "Group")]
pub struct BreadcrumbProps {
    /// The places, outermost first; the last is the page itself, which is not pressed.
    #[component(slot(default))]
    pub segments: Vec<BreadcrumbSegmentProps>,
    /// A glyph before the first place: a drive, Home.
    pub root_icon: Option<Icon>,
    /// The empty space after the last place was pressed: offer the path as a field to type in.
    #[component(event)]
    pub edit: Event,
}

/// One place of a path: what it is called, and going there.
#[derive(Component, Default)]
#[component(
    name = "BreadcrumbSegment",
    role = "Link",
    label = "label",
    presents = "item"
)]
pub struct BreadcrumbSegmentProps {
    /// The place's stable key.
    #[component(key)]
    pub key: String,
    /// What the place is called: "Documents".
    #[component(required, positional)]
    pub label: String,
    /// The resource the place is: going there opens it.
    pub item: Option<ResourceId>,
    /// The place was pressed: go there.
    #[component(event)]
    pub activate: Event,
}

crate::component::part_slot!(BreadcrumbSegmentProps);

impl Described for Breadcrumb {
    type Props = BreadcrumbProps;

    fn from_props(key: &str, props: BreadcrumbProps) -> Self {
        let BreadcrumbProps {
            segments,
            root_icon,
            edit,
        } = props;
        let mut labels = Vec::with_capacity(segments.len());
        let mut handlers = Vec::with_capacity(segments.len());
        for segment in segments {
            labels.push(segment.label);
            handlers.push(segment.activate.handler());
        }
        let goes = handlers.iter().any(Option::is_some);
        let mut trail = Self::new(labels);
        if let Some(icon) = root_icon {
            trail = trail.root_icon(icon);
        }
        if goes {
            trail = trail.on_view(key, move |index| {
                if let Some(Some(go)) = handlers.get_mut(index) {
                    go();
                }
            });
        }
        if let Some(mut edit) = edit.handler() {
            trail = trail.on_edit_requested(move || edit());
        }
        trail
    }
}
