//! The note on a page, laid out by the browser.

use ui::widget::Widget;

use super::MainPanel;

impl MainPanel {
    /// The note as a page lays it out: its title, the line under it and its body in one reading
    /// column, centred in the pane at any width.
    pub(super) fn compose_page(&self, cx: &mut ui::composition::HtmlCx<'_>) {
        cx.children();
        let m = &self.metrics;
        let hang = m.text_edge - m.body_inset;
        cx.restyle(&self.title, |_| "width:100%;flex:none;".to_owned());
        if let Some(header) = self.header.built() {
            cx.restyle(header, |layout| {
                format!(
                    "{}flex:none;margin-top:{}px;",
                    ui::composition::without_sizes(layout),
                    m.header_gap
                )
            });
        }
        // The document sets its text in from its own edge by the editor's text
        // inset; the editor stands that much left of the text edge.
        cx.restyle(&self.editor, |_| {
            format!("flex:none;margin:{}px 0 0 -{hang}px;", m.body_gap)
        });
        let mut page: Vec<&dyn Widget> = vec![&self.title];
        if let Some(header) = self.header.built() {
            page.push(header);
        }
        page.push(&self.editor);
        cx.region(
            "page",
            "div",
            &format!(
                "display:flex;flex-direction:column;width:100%;max-width:{}px;margin:0 auto;padding:{}px {}px {}px {}px;",
                self.reading_width + m.text_edge + m.gutter,
                m.gutter,
                m.gutter,
                m.gutter.max(self.bottom_inset),
                m.text_edge,
            ),
            &page,
        );
        cx.flow("display:flex;flex-direction:column;flex:1;min-width:0;min-height:0;");
    }
}
