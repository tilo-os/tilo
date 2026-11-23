//! The shell on a page: its columns side by side as the browser lays them out, whatever width the
//! page has, and one column at a time below the compact width.

use super::{Column, NavPane, NavShell, Part, at};
use crate::widget::Widget;

impl NavShell {
    /// The shell as a page lays it out: a column element per column shown, each a header row over
    /// its body, and the column in front alone on a narrow page.
    pub(super) fn compose_page(&self, cx: &mut crate::composition::HtmlCx<'_>) {
        cx.children();
        let g = &self.geometry;
        let metrics = &cx.theme.tokens.metrics;
        let row = self.inputs.borrow().metrics.row;
        let column_rect = |column: Column| match column {
            Column::Sidebar => g.sidebar_column,
            Column::Overview => {
                if g.overview_column.width() > 0.0 {
                    g.overview_column
                } else {
                    g.rail
                }
            }
            Column::Content => g.content_column,
            Column::Inspector => g.inspector_column,
        };
        // Which column's header each control stands in, by the column it is drawn in.
        let header_controls: Vec<Part> = self
            .exposed_parts()
            .into_iter()
            .filter(|part| {
                matches!(
                    part,
                    Part::Title(_)
                        | Part::Leading(_)
                        | Part::Action(..)
                        | Part::More(_)
                        | Part::Toggle
                        | Part::Back
                        | Part::InspectorToggle
                        | Part::History(_)
                        | Part::StatusAction
                )
            })
            .collect();
        let rect_of = |part: Part| self.part(part).map(Widget::get_rect);
        let mut regions = 0;
        for column in [
            Column::Sidebar,
            Column::Overview,
            Column::Content,
            Column::Inspector,
        ] {
            let area = column_rect(column);
            if area.width() <= 0.0 || g.floating(column) {
                continue;
            }
            let mut header: Vec<(f64, Part)> = header_controls
                .iter()
                .filter_map(|&part| {
                    let rect = rect_of(part)?;
                    let own = match part {
                        Part::Title(c) | Part::Leading(c) | Part::Action(c, _) | Part::More(c) => {
                            c == column
                        }
                        _ => {
                            let x = rect.center().x;
                            rect.width() > 0.0 && x >= area.x0 && x < area.x1
                        }
                    };
                    own.then_some((rect.x0, part))
                })
                .collect();
            // What leads the row, then the title taking what width is left, then the actions at its
            // trailing edge.
            let side = |part: Part| match part {
                Part::Toggle | Part::Back | Part::Leading(_) => 0,
                Part::Title(_) => 1,
                _ => 2,
            };
            header.sort_by(|a, b| side(a.1).cmp(&side(b.1)).then(a.0.total_cmp(&b.0)));
            // The first trailing control the row shows: a hidden one stands nowhere on the page.
            if let Some(widget) = header
                .iter()
                .filter(|(_, part)| side(*part) == 2)
                .filter_map(|&(_, part)| self.part(part))
                .find(|widget| widget.get_rect().width() > 0.0)
            {
                cx.restyle(widget, |layout| format!("{layout}margin-left:auto;"));
            }
            let mut header: Vec<&dyn Widget> = header
                .iter()
                .filter_map(|&(_, part)| self.part(part))
                .collect();
            if column == Column::Content {
                header.push(&self.sync_status);
            }
            for part in header_controls
                .iter()
                .filter(|part| matches!(part, Part::Title(_)))
            {
                if let Some(title) = self.part(*part) {
                    cx.restyle(title, |_| "flex:1;min-width:0;".to_owned());
                }
            }
            let key = column.key();
            cx.region(
                &format!("{key}-header"),
                "header",
                &format!(
                    "display:flex;flex-direction:row;align-items:center;gap:{}px;flex:none;min-height:{}px;padding:0 {}px;box-shadow:inset 0 -1px 0 var(--tilo-border);",
                    row.gap, row.height, row.inset
                ),
                &header,
            );
            // A press on the page lands where the shell laid its parts out, so each header row
            // and each column stands at the rect the shell gave it, not at the bounds of what it
            // holds: a header row shorter than the shell's would lift every row under it.
            place(cx, &format!("{key}-header"), g.header(column));
            if column == Column::Content {
                self.compose_narrow_back(cx, &format!("{}#{key}-header", cx.composed.path));
            }
            let mut members: Vec<&dyn Widget> = Vec::new();
            if column == Column::Overview {
                members.extend(self.part(Part::Controls));
                members.extend(self.part(Part::Filter));
            }
            let body = self.part(Part::Body(column)).or_else(|| {
                (column == Column::Overview)
                    .then(|| self.part(Part::Rail))
                    .flatten()
            });
            if let Some(body) = body {
                cx.restyle(body, |layout| {
                    format!(
                        "{}flex:1;min-height:0;min-width:0;",
                        crate::composition::without_sizes(layout)
                    )
                });
                members.push(body);
            }
            let width = match column {
                Column::Sidebar => Some(metrics.sidebar_width),
                Column::Overview if g.overview_column.width() > 0.0 => Some(metrics.overview_width),
                Column::Overview | Column::Content => None,
                Column::Inspector => Some(metrics.inspector_width),
            };
            let size = width.map_or_else(
                || "flex:1;min-width:0;".to_owned(),
                |width| format!("flex:none;width:{width}px;"),
            );
            let ground = match column {
                Column::Sidebar => "background:var(--tilo-sidebar);",
                Column::Inspector => "background:var(--tilo-inspector,var(--tilo-sidebar));",
                _ => "",
            };
            let edge = if regions > 0 {
                "box-shadow:inset 1px 0 0 var(--tilo-border);"
            } else {
                ""
            };
            // The header row and the body stand in one column element.
            let mut paths = vec![format!("{}#{key}-header", cx.composed.path)];
            paths.extend(
                members
                    .iter()
                    .filter_map(|widget| cx.child_path(*widget).map(str::to_owned)),
            );
            if cx.group(
                key,
                "section",
                &format!("display:flex;flex-direction:column;{size}min-height:0;{ground}{edge}"),
                |child| paths.iter().position(|path| *path == child.path),
            ) {
                place(cx, key, area);
                regions += 1;
            }
        }
        // The columns in reading order, whatever order the shell keeps its parts in.
        let shell = cx.composed.path.clone();
        cx.composed.children.sort_by_key(|child| {
            Column::ALL
                .iter()
                .position(|column| child.path == format!("{shell}#{}", column.key()))
                .unwrap_or(Column::ALL.len())
        });
        // A narrow page shows the one column in front, as a narrow window stacks them.
        let front = match self.front_pane() {
            NavPane::Sidebar => Column::Sidebar,
            NavPane::Overview => Column::Overview,
            NavPane::Content => Column::Content,
        };
        for child in &mut cx.composed.children {
            if let Some(column) = Column::ALL
                .into_iter()
                .find(|column| child.path == format!("{shell}#{}", column.key()))
            {
                child.attrs.push(("data-pane", column.key().to_owned()));
                if column == front {
                    child.attrs.push(("data-front", String::new()));
                }
            }
        }
        cx.flow(
            "display:flex;flex-direction:row;width:100%;height:100%;container:shell/inline-size;",
        );
    }

    /// A narrow page shows one column at a time: the content's header leads with a Back that
    /// returns to the column behind it, named as that column's header names it. A wide page has
    /// every column beside it and hides it.
    fn compose_narrow_back(&self, cx: &mut crate::composition::HtmlCx<'_>, header: &str) {
        use crate::composition::{Composed, Face, Own, TextLook};
        let behind = [Column::Overview, Column::Sidebar]
            .into_iter()
            .find(|&column| match column {
                Column::Overview => self.overview.is_some(),
                _ => self.sidebar.is_some(),
            });
        let Some(behind) = behind else {
            return;
        };
        let theme = cx.theme().clone();
        let ink = theme.tokens.colors.text_primary;
        let label = at(&self.headers, behind).title.title.clone();
        let mut back = Composed::new(format!("{header}#back"), "Region", [0.0; 4]);
        back.tag = Some("button");
        back.flows = true;
        back.layout = Some("flex:none;".to_owned());
        back.attrs = vec![
            ("type", "button".to_owned()),
            ("data-a", "back".to_owned()),
            ("data-narrow", String::new()),
            (
                "aria-label",
                crate::i18n::I18nManager::lookup_plain("window-toolbar-back"),
            ),
        ];
        let radius = f64::from(theme.tokens.radii.item);
        back.own = Some(Own::Face(Face {
            icon: crate::composition::glyph(crate::icons::Icon::ChevronLeft, 16.0, ink, false),
            label: label.map(|label| {
                TextLook::new(label, &theme.text(crate::tokens::TextRole::Body), Some(ink))
                    .with_max_lines(Some(1))
            }),
            detail: None,
            trailing: None,
            fill: None,
            edge: None,
            radius,
            pad: f64::from(theme.tokens.metrics.icon_label_gap),
            gap: f64::from(theme.tokens.metrics.icon_label_gap),
            spread: false,
        }));
        if let Some(row) = cx
            .composed
            .children
            .iter_mut()
            .find(|child| child.path == header)
        {
            // A wide window lays out no Back: it stands where the row does, so the page keeps it.
            back.rect = row.rect;
            row.children.insert(0, back);
        }
    }
}

/// The region of the shell's element keyed `key` stands at `rect`.
fn place(cx: &mut crate::composition::HtmlCx<'_>, key: &str, rect: vello::kurbo::Rect) {
    let path = format!("{}#{key}", cx.composed.path);
    if rect.width() > 0.0
        && let Some(region) = cx
            .composed
            .children
            .iter_mut()
            .find(|child| child.path == path)
    {
        region.rect = [rect.x0, rect.y0, rect.width(), rect.height()];
    }
}
