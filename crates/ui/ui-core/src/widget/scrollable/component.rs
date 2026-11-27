//! [`Scrollable`] as a described component: a column's body that scrolls.

use tilo_ui_base::widget::Element;
use tilo_ui_base::{component::Component, component::Described, component::PartViews};

use super::Scrollable;

/// A body that scrolls under its column's fixed header, showing the part of the content the
/// column fits.
#[derive(Component, Default)]
#[component(name = "Scroll", role = "ScrollView", label = "label")]
pub struct ScrollProps {
    /// Open at the end of the content and stay there while it grows, unless the person scrolled
    /// away: a transcript, whose newest line is last.
    pub follow_end: bool,
    /// Scroll sideways instead: a row wider than its column, such as a board's lists.
    pub across: bool,
    /// Scroll both ways, for a 2D surface wider and taller than its column (a board of strips);
    /// takes precedence over `across`.
    pub both: bool,
    /// Show the bar for as long as the content overflows, not only while it is scrolled: a card
    /// held to a height whose words go on below it, so the person sees there is more.
    pub shows_bar: bool,
    /// What the region holds, for assistive technology: Tab stops on a region whose content
    /// overflows and announces it ("Identity setup, step 2 of 4: Protect").
    pub label: Option<String>,
    /// Scroll a control that takes the keyboard into view: a form held to a card's height, whose
    /// next field Tab reaches below the fold. Scrolling away from the same control stays put.
    pub reveals_focus: bool,
    /// Keep the descendant that names itself this (`Widget::kept_in_view`) in view when the one
    /// that is named changes: the front strip of a row a person pans, brought into view as they
    /// move between windows.
    #[component(data)]
    pub keep: Option<String>,
    /// What the content stands for: the page a sidebar chose, the item a list opened. When it
    /// stands for another, the region shows the new one from its top, not where the last was
    /// left; while it stands for the same one, an edit keeps the place the person scrolled to.
    #[component(data)]
    pub shows: Option<String>,
    /// What scrolls.
    #[component(part(default))]
    pub content: Option<Element>,
}

impl Described for Scrollable {
    type Props = ScrollProps;

    fn from_props(_key: &str, props: ScrollProps) -> Self {
        let ScrollProps {
            follow_end,
            across,
            both,
            shows_bar,
            label,
            reveals_focus,
            keep,
            shows,
            content,
        } = props;
        let mut scroll = if both {
            Self::new(super::ScrollAxis::Both)
        } else if across {
            Self::new_horizontal().with_content_min_viewport_width()
        } else {
            Self::new_vertical()
        };
        if follow_end {
            scroll = scroll.following_end();
        }
        if shows_bar {
            scroll = scroll.always_visible();
        }
        if let Some(label) = label {
            scroll = scroll.with_label(label);
        }
        if reveals_focus {
            scroll = scroll.with_focus_reveal();
        }
        if let Some(keep) = keep {
            scroll.keep_in_view(keep);
        }
        scroll.shows(shows);
        match content {
            Some(content) => scroll.with_child(content),
            None => scroll,
        }
    }

    fn set_props(&mut self, props: ScrollProps) -> bool {
        let same_axis = if props.both {
            matches!(self.axis, super::ScrollAxis::Both)
        } else {
            matches!(
                (props.across, self.axis),
                (true, super::ScrollAxis::Horizontal) | (false, super::ScrollAxis::Vertical)
            )
        };
        if props.shows_bar == self.overlay {
            self.overlay = !props.shows_bar;
            self.bar_dirty = true;
        }
        self.label = props.label;
        self.reveal_focus = props.reveals_focus;
        if let Some(keep) = &props.keep {
            self.keep_in_view(keep.clone());
        }
        self.shows(props.shows);
        props.follow_end == self.follow_end && same_axis
    }

    fn part_views(&mut self, part: &str) -> Option<&mut dyn PartViews> {
        (part == "content").then_some(self as &mut dyn PartViews)
    }
}

impl PartViews for Scrollable {
    fn len(&self) -> usize {
        usize::from(self.child.is_some())
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element> {
        if index == 0 { self.child_mut() } else { None }
    }

    fn insert(&mut self, _index: usize, view: Element) {
        let _ = self.replace_child(view);
    }

    fn remove(&mut self, _index: usize) {
        let _ = self.replace_child(Box::new(tilo_ui_base::widget::Spacer::token(
            tilo_ui_base::tokens::SpacingStep::S0,
        )));
    }

    fn move_view(&mut self, _from: usize, _to: usize) {}

    fn replace(&mut self, _index: usize, view: Element) {
        let _ = self.replace_child(view);
    }
}

#[cfg(test)]
mod tests {
    use super::{ScrollProps, Scrollable};
    use crate::widget::Widget;
    use tilo_ui_base::component::Described;

    fn name(scroll: &Scrollable) -> Option<String> {
        Widget::accessibility(scroll).and_then(|node| node.name)
    }

    #[test]
    fn a_named_region_says_what_it_holds_as_the_page_under_it_changes() {
        let mut scroll = Scrollable::from_props(
            "body",
            ScrollProps {
                label: Some("Identity setup, step 2 of 4: Protect".to_owned()),
                reveals_focus: true,
                shows_bar: true,
                ..ScrollProps::default()
            },
        );
        assert_eq!(
            name(&scroll).as_deref(),
            Some("Identity setup, step 2 of 4: Protect")
        );
        assert!(
            scroll.reveal_focus,
            "a form region scrolls the focused field into view"
        );
        assert!(!scroll.overlay, "the bar stays while the content overflows");

        assert!(scroll.set_props(ScrollProps {
            label: Some("Identity setup, step 3 of 4: Backup".to_owned()),
            shows_bar: true,
            ..ScrollProps::default()
        }));
        assert_eq!(
            name(&scroll).as_deref(),
            Some("Identity setup, step 3 of 4: Backup")
        );
        assert!(
            !scroll.reveal_focus,
            "a region that stops asking stops revealing"
        );

        assert!(scroll.set_props(ScrollProps {
            shows_bar: true,
            ..ScrollProps::default()
        }));
        assert_eq!(
            name(&scroll),
            None,
            "a region left unnamed is unnamed again"
        );
    }
}
