//! Page breaks: the flow sliced into page-start offsets at line boundaries,
//! decided as its blocks arrive in order.

use super::{LaidBlock, LayoutKind};

/// One row of the flattened page-breaking grid: either a whole indivisible
/// box (a separator, a picture, a table row), or a single line of a text
/// block (retaining enough paragraph structure — `line`/`line_count` — to
/// enforce TeX-like break constraints).
#[derive(Clone, Copy)]
struct Row {
    y: f32,
    height: f32,
    block: usize,
    line: usize,
    line_count: usize,
    kind: LayoutKind,
}

/// The rows block `b` (at `block_index`) breaks into.
fn rows_of(block_index: usize, b: &LaidBlock, rows: &mut Vec<Row>) {
    let row = |y: f32, height: f32, line: usize, line_count: usize| Row {
        y,
        height,
        block: block_index,
        line,
        line_count,
        kind: b.kind,
    };
    match b.kind {
        LayoutKind::Separator | LayoutKind::Math => {
            rows.push(row(b.top, b.bottom - b.top, 0, 1));
        }
        LayoutKind::Table => {
            let table = b.table.as_ref().map_or(&[][..], |t| t.rows.as_slice());
            for (line, &(y, height, _)) in table.iter().enumerate() {
                rows.push(row(y, height, line, table.len()));
            }
        }
        LayoutKind::Figure => {
            let picture = b.figure.as_ref().map_or(0.0, |f| f.height);
            rows.push(row(b.top, picture, 0, 1));
            if !b.text.is_empty() {
                for (line, l) in b.lines.iter().enumerate() {
                    rows.push(row(l.y, b.line_height, line, b.lines.len()));
                }
            }
        }
        _ => {
            for (line, l) in b.lines.iter().enumerate() {
                rows.push(row(l.y, b.line_height, line, b.lines.len()));
            }
        }
    }
}

/// Slices the flow into page-start offsets at line boundaries as its blocks
/// arrive in order. A break is decided once the row that overflows the page
/// is known; every rule looks back from that row, never past it, so a break
/// taken early is the one the whole flow would take.
pub(super) struct Paginator {
    page_h: f32,
    rows: Vec<Row>,
    blocks: usize,
    /// The row the open page starts at, once there is one.
    page_row: Option<usize>,
    pages: Vec<f32>,
    /// How many of `pages` were handed out.
    handed: usize,
}

impl Paginator {
    pub(super) fn new(page_h: f32) -> Self {
        Self {
            page_h,
            rows: Vec::new(),
            blocks: 0,
            page_row: None,
            pages: Vec::new(),
            handed: 0,
        }
    }

    /// Page starts decided since the last call.
    pub(super) fn take_decided(&mut self) -> Vec<f32> {
        let fresh = self.pages.get(self.handed..).unwrap_or_default().to_vec();
        self.handed = self.pages.len();
        fresh
    }

    /// Whether no page start was ever handed out.
    pub(super) fn handed_none(&self) -> bool {
        self.handed == 0
    }

    pub(super) fn push(&mut self, b: &LaidBlock) {
        let block_index = self.blocks;
        self.blocks += 1;
        if self.page_h <= 1.0 {
            if self.pages.is_empty() {
                self.pages.push(0.0);
            }
            return;
        }
        let from = self.rows.len();
        rows_of(block_index, b, &mut self.rows);
        // A table's rows or a figure's caption follow their own order; keep
        // the block's rows in reading order (blocks themselves arrive in it).
        if let Some(fresh) = self.rows.get_mut(from..) {
            fresh.sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal));
        }
        self.advance();
    }

    fn advance(&mut self) {
        let rows = &self.rows;
        let mut page_row = if let Some(row) = self.page_row {
            row
        } else {
            let Some(first) = rows.first() else {
                return;
            };
            self.pages.push(first.y);
            0
        };
        while page_row < rows.len() {
            let Some(&start_row) = rows.get(page_row) else {
                break;
            };
            let page_start = start_row.y;
            let Some(overflow) = (page_row..rows.len()).find(|&index| {
                rows.get(index)
                    .is_some_and(|r| r.y + r.height - page_start > self.page_h + 0.5)
            }) else {
                break;
            };
            let mut break_at = overflow;
            let Some(&row) = rows.get(break_at) else {
                break;
            };

            // Keep at least two lines of an ordinary text block on either side
            // of a page boundary whenever that can be achieved on this page.
            if matches!(
                row.kind,
                LayoutKind::Paragraph
                    | LayoutKind::Quote
                    | LayoutKind::Verse { .. }
                    | LayoutKind::List
            ) && row.line_count >= 4
                && (row.line == 1 || row.line_count - row.line == 1)
            {
                break_at -= 1;
            }

            // A heading is semantically attached to the text following it. If
            // it would become the final item on a page, carry the complete
            // heading to the next page as long as it did not itself begin this
            // page.
            if break_at > page_row
                && let Some(&prev) = rows.get(break_at - 1)
                && let Some(&cur) = rows.get(break_at)
                && prev.kind == LayoutKind::Heading
                && prev.block != cur.block
            {
                let heading_block = prev.block;
                let heading_start = (page_row..break_at)
                    .find(|&index| rows.get(index).is_some_and(|r| r.block == heading_block))
                    .unwrap_or(break_at - 1);
                if heading_start > page_row {
                    break_at = heading_start;
                }
            }

            // An indivisible item taller than a page has no legal alternative.
            if break_at <= page_row {
                break_at = overflow.max(page_row + 1);
            }
            let Some(&brk) = rows.get(break_at) else {
                break;
            };
            self.pages.push(brk.y);
            page_row = break_at;
        }
        self.page_row = Some(page_row);
    }
}

/// Every page start of a whole flow; a flow with nothing to break has the
/// one page at the top.
#[cfg(test)]
fn paginate(blocks: &[LaidBlock], page_h: f32) -> Vec<f32> {
    let mut paginator = Paginator::new(page_h);
    for block in blocks {
        paginator.push(block);
    }
    let pages = paginator.take_decided();
    if pages.is_empty() { vec![0.0] } else { pages }
}

#[cfg(test)]
mod tests {
    use super::super::LaidLine;
    use super::*;

    fn block_with_lines(kind: LayoutKind, top: f32, line_count: usize) -> LaidBlock {
        let line_height = 20.0;
        LaidBlock {
            index: 0,
            kind,
            top,
            bottom: top + line_count as f32 * line_height,
            size: 16.0,
            line_height,
            indent: 0.0,
            lines: (0..line_count)
                .map(|line| LaidLine {
                    y: top + line as f32 * line_height,
                    glyph_scale: 1.0,
                    adjustment_ratio: 0.0,
                    rtl: false,
                    runs: Vec::new(),
                })
                .collect(),
            bullet: None,
            math: None,
            text: String::new(),
            char_len: 0,
            hard_breaks: Vec::new(),
            family: None,
            dim: false,
            scroll_width: 0.0,
            figure: None,
            table: None,
        }
    }

    #[test]
    fn pagination_avoids_single_line_widows_and_orphans() {
        let widow = block_with_lines(LayoutKind::Paragraph, 0.0, 5);
        assert_eq!(paginate(&[widow], 85.0), vec![0.0, 60.0]);

        let before = block_with_lines(LayoutKind::Separator, 0.0, 1);
        let paragraph = block_with_lines(LayoutKind::Paragraph, 40.0, 5);
        assert_eq!(paginate(&[before, paragraph], 65.0), vec![0.0, 40.0, 100.0]);
    }

    #[test]
    fn pagination_keeps_a_heading_with_following_text() {
        let intro = block_with_lines(LayoutKind::Paragraph, 0.0, 1);
        let heading = block_with_lines(LayoutKind::Heading, 25.0, 1);
        let body = block_with_lines(LayoutKind::Paragraph, 45.0, 3);
        assert_eq!(
            paginate(&[intro, heading, body], 55.0),
            vec![0.0, 25.0, 65.0]
        );
    }

    /// Breaks decided block by block are the breaks of the whole flow.
    #[test]
    fn breaks_taken_as_blocks_arrive_are_the_whole_flows() {
        let blocks: Vec<LaidBlock> = (0..12)
            .map(|n| block_with_lines(LayoutKind::Paragraph, n as f32 * 120.0, 6))
            .collect();
        let mut paginator = Paginator::new(170.0);
        let mut taken = Vec::new();
        for block in &blocks {
            paginator.push(block);
            taken.extend(paginator.take_decided());
        }
        assert_eq!(taken, paginate(&blocks, 170.0));
        assert!(taken.len() > 6);
    }
}
