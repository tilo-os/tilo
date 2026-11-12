//! The app's name after a tab's title, where the title alone would not tell the tab from
//! another of the bar: two windows of different apps on pages of one name ("Today" in Tasks and
//! in Today) say whose each is. Two windows of one app on the same page read the same.

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use super::{TabBar, compose, tab_title_style};
use crate::text_render::TextRenderer;
use crate::theme::Theme;

/// The space between a tab's title and the app's name after it ([`TabBar::qualifier`]): the gap
/// the theme keeps between a label and the thing that qualifies it.
pub(super) fn qualifier_gap(theme: &Theme) -> f32 {
    theme.tokens.metrics.icon_label_gap
}

impl TabBar {
    /// The app tab `i` is a window of, where its title alone would not tell it from another tab
    /// of the bar: one that reads the same and is another app's ("Today" in Tasks beside Today's
    /// own window). Two windows of one app on the same page read the same, and a title that is
    /// already its app's name needs nothing after it.
    pub(super) fn qualifier(&self, i: usize) -> Option<&str> {
        let tab = self.tabs.get(i)?;
        let app = tab
            .app_name
            .as_deref()
            .map(str::trim)
            .filter(|app| !app.is_empty())?;
        let reads = |text: &str| crate::search::fold_search_text(text.trim());
        let title = reads(&tab.title);
        if title.is_empty() || title == reads(app) {
            return None;
        }
        self.tabs
            .iter()
            .any(|other| {
                other.id != tab.id
                    && reads(&other.title) == title
                    && other.app_name.as_deref().map(str::trim) != Some(app)
            })
            .then_some(app)
    }

    /// The app's name after a title another tab of the bar reads too ([`Self::qualifier`]): in
    /// the body weight and `ink`, a step quieter than the title, so the title still reads first. Drawn only where it
    /// fits whole before the pill's end; [`Self::visible_title`] leaves it the room.
    pub(super) fn draw_pill_qualifier(
        &self,
        scene: &mut Scene,
        transform: Affine,
        (tr, theme): (&mut TextRenderer, &Theme),
        (i, ink): (usize, vello::peniko::Color),
        (label, clip): (Rect, Rect),
    ) {
        let Some(app) = self.qualifier(i) else {
            return;
        };
        let style = tab_title_style(theme);
        let (app_w, _) = tr.measure_style(app, None, &style);
        let x = label.x1 + f64::from(qualifier_gap(theme));
        if x + f64::from(app_w) > clip.x1 + 0.5 {
            return;
        }
        tr.draw_style(
            scene,
            app,
            ink,
            compose(transform, Affine::translate((x, label.y0))),
            None,
            &style,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::lay_out_at_width;
    use super::*;
    use crate::text_render::TextWeight;
    use crate::widget::{TabDescriptor, Widget};

    /// Tabs of different apps on pages of one name say whose each is; two windows of one app on
    /// the same page, and a tab already named by its app, need nothing after the title.
    #[test]
    fn tabs_that_read_alike_say_which_app_each_is() {
        let tab = |id, title: &str, app: &str| TabDescriptor {
            app_name: Some(app.to_owned()),
            ..TabDescriptor::new(id, title)
        };
        let mut b = TabBar::new(
            "g1",
            vec![
                tab(1, "Today", "Tasks"),
                tab(2, "Today", "Tasks"),
                tab(3, "Today", "Today"),
                tab(4, "Inbox", "Mail"),
            ],
            1,
        );
        let said: Vec<Option<&str>> = (0..4).map(|i| b.qualifier(i)).collect();
        assert_eq!(said, [Some("Tasks"), Some("Tasks"), None, None]);
        let names: Vec<String> = Widget::accessibility(&b)
            .expect("tab bar is not presentational")
            .children
            .into_iter()
            .filter(|child| child.role == crate::a11y::Role::Tab)
            .filter_map(|child| child.name)
            .collect();
        assert_eq!(names, ["Today Tasks", "Today Tasks", "Today", "Inbox"]);

        // Alone with windows of its own app, a page needs no app's name.
        b.tabs_mut().remove(2);
        assert_eq!(b.qualifier(0), None);

        // The name asks for its own room, so the pill grows by it rather than eliding the title.
        b.tabs_mut()[1].app_name = Some("Calendar".to_owned());
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        lay_out_at_width(&mut b, 900.0);
        assert_eq!(b.qualifier(0), Some("Tasks"));
        assert_eq!(
            b.visible_title(0, &mut tr, &theme).as_deref(),
            Some("Today")
        );
        let (_, _, advance) = b.title_advance[&1].clone();
        let (today, _) = tr.measure_style(
            "Today",
            None,
            &tab_title_style(&theme).with_weight(TextWeight::Semibold),
        );
        assert!(
            advance > f64::from(today) + f64::from(qualifier_gap(&theme)),
            "{advance} holds the app's name"
        );
    }
}
