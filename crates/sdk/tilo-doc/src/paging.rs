//! Page breaking shared by the word processor's sheets, its PDF export and the
//! e-book reader: a [`PageFlow`] stacks blocks down equal pages and decides where
//! each goes. A block that fits goes whole, and a block that does not splits
//! between its lines keeping [`Fragmentation::orphans`] lines before the break and
//! `widows` after, or moves whole. The first thing on a page never moves — a block
//! taller than a page overflows it — and a block that keeps with the next moves
//! with it, unless it began that page.

use std::ops::Range;

/// Rounding slack: content overshooting a page by less than this still fits,
/// so sub-pixel layout noise never breaks a page.
const SLACK: f64 = 0.5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Fragmentation {
    pub orphans: u8,
    pub widows: u8,
}

impl Default for Fragmentation {
    fn default() -> Self {
        Self {
            orphans: 2,
            widows: 2,
        }
    }
}

impl Fragmentation {
    /// Breaks between any two lines.
    pub const ANYWHERE: Self = Self {
        orphans: 1,
        widows: 1,
    };

    /// How many lines of a block of `total`, continuing at `first`, go on a page with room for
    /// `available` more: all that remain when they fit, otherwise as many as leave `widows` for the
    /// next page, or none when fewer than `orphans` would stay.
    #[must_use]
    pub fn lines_on_page(self, total: usize, first: usize, available: usize) -> usize {
        let remaining = total.saturating_sub(first);
        if remaining <= available {
            return remaining;
        }
        let take = available.min(remaining.saturating_sub(usize::from(self.widows)));
        if take < usize::from(self.orphans) {
            0
        } else {
            take
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Block {
    /// Space above the block, dropped at the top of a page.
    pub lead: f64,
    pub height: f64,
    /// Starts a fresh page unless it would be the first thing on one.
    pub break_before: bool,
    /// Moves to the next page with the block after it rather than end a page.
    pub keep_with_next: bool,
    pub fragmentation: Fragmentation,
}

impl Block {
    #[must_use]
    pub fn new(height: f64) -> Self {
        Self {
            lead: 0.0,
            height,
            break_before: false,
            keep_with_next: false,
            fragmentation: Fragmentation::default(),
        }
    }

    #[must_use]
    pub fn with_lead(mut self, lead: f64) -> Self {
        self.lead = lead;
        self
    }

    #[must_use]
    pub fn breaking_before(mut self, break_before: bool) -> Self {
        self.break_before = break_before;
        self
    }

    #[must_use]
    pub fn keeping_with_next(mut self, keep: bool) -> Self {
        self.keep_with_next = keep;
        self
    }

    #[must_use]
    pub fn fragmented(mut self, fragmentation: Fragmentation) -> Self {
        self.fragmentation = fragmentation;
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fragment {
    pub page: usize,
    pub top: f64,
    pub lines: Range<usize>,
}

/// Blocks that kept with the block being placed and moved with it from the bottom of a page to the
/// top of the next.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Carry {
    pub block: usize,
    pub from_page: usize,
    pub from_top: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub page: usize,
    pub top: f64,
    /// The pieces of a block that split; empty when it went whole.
    pub fragments: Vec<Fragment>,
    pub carry: Option<Carry>,
}

impl Placed {
    /// The page and top of line `line` of the block whose line boxes are `lines`.
    #[must_use]
    pub fn locate(&self, lines: &[(f64, f64)], line: usize) -> (usize, f64) {
        let top = |index: usize| lines.get(index).map_or(0.0, |&(top, _)| top);
        match self.fragments.iter().find(|f| f.lines.contains(&line)) {
            Some(f) => (f.page, f.top + top(line) - top(f.lines.start)),
            None => (self.page, self.top + top(line)),
        }
    }
}

/// The trailing run of keep-with-next blocks on the current page.
#[derive(Clone, Copy, Debug)]
struct Kept {
    block: usize,
    page: usize,
    top: f64,
    /// Nothing was on the page before the run; it cannot move.
    began_page: bool,
}

#[derive(Clone, Debug)]
pub struct PageFlow {
    height: f64,
    page: usize,
    y: f64,
    placed: bool,
    /// The page was started by a break, so space before a block is dropped.
    fresh: bool,
    blocks: usize,
    kept: Option<Kept>,
}

impl PageFlow {
    #[must_use]
    pub fn new(height: f64) -> Self {
        Self::resume(height, 0, 0.0, false)
    }

    /// A flow continuing `y` below the top of `page`; `placed` says whether anything is on that
    /// page already.
    #[must_use]
    pub fn resume(height: f64, page: usize, y: f64, placed: bool) -> Self {
        Self {
            height,
            page,
            y,
            placed,
            fresh: false,
            blocks: 0,
            kept: None,
        }
    }

    #[must_use]
    pub fn page(&self) -> usize {
        self.page
    }

    /// The bottom of what was placed, below the current page's top.
    #[must_use]
    pub fn y(&self) -> f64 {
        self.y
    }

    pub fn skip(&mut self, amount: f64) {
        self.y += amount;
    }

    /// Move the cursor to `y` on the current page, for content that flows
    /// beside an object instead of below it.
    pub fn set_y(&mut self, y: f64) {
        self.y = y;
    }

    pub fn place(&mut self, block: &Block) -> Placed {
        self.flow(block, None::<fn() -> Vec<(f64, f64)>>)
    }

    /// Place a block that may split between its lines. `lines` gives each line's top and height
    /// below the block's top, and is only asked for when the block does not fit whole.
    pub fn place_lines(
        &mut self,
        block: &Block,
        lines: impl FnOnce() -> Vec<(f64, f64)>,
    ) -> Placed {
        self.flow(block, Some(lines))
    }

    fn fits(&self, bottom: f64) -> bool {
        bottom <= self.height + SLACK
    }

    fn next_page(&mut self) {
        self.page = self.page.saturating_add(1);
        self.y = 0.0;
        self.placed = false;
        self.fresh = true;
        self.kept = None;
    }

    /// Break the page before a block that does not fit, taking along the keep-with-next run that
    /// would otherwise end the page.
    fn overflow_break(&mut self) -> Option<Carry> {
        let carry = self
            .kept
            .filter(|k| k.page == self.page && !k.began_page)
            .map(|k| Carry {
                block: k.block,
                from_page: self.page,
                from_top: k.top,
            });
        let y = self.y;
        self.next_page();
        if let Some(c) = carry {
            self.y = y - c.from_top;
            self.placed = true;
            self.kept = Some(Kept {
                block: c.block,
                page: self.page,
                top: 0.0,
                began_page: true,
            });
        }
        carry
    }

    fn flow<F: FnOnce() -> Vec<(f64, f64)>>(&mut self, block: &Block, lines: Option<F>) -> Placed {
        let index = self.blocks;
        self.blocks = self.blocks.saturating_add(1);
        if self.placed && block.break_before {
            self.next_page();
        }
        let mut carry = None;
        let mut lines = lines.map(|f| LazyLines::Pending(Some(f)));
        loop {
            let top = if self.fresh && !self.placed {
                self.y
            } else {
                self.y + block.lead
            };
            let began_page = !self.placed;
            if self.fits(top + block.height) {
                return self.land(block, index, top, Vec::new(), carry, began_page);
            }
            let boxes = match lines.as_mut().map(LazyLines::get) {
                Some(boxes) if !boxes.is_empty() => boxes,
                _ => {
                    if self.placed {
                        let moved = self.overflow_break();
                        carry = carry.or(moved);
                        continue;
                    }
                    return self.land(block, index, top, Vec::new(), carry, began_page);
                }
            };
            let total = boxes.len();
            let available = self.available(boxes, top, 0);
            if block.fragmentation.lines_on_page(total, 0, available) == 0 && self.placed {
                let moved = self.overflow_break();
                carry = carry.or(moved);
                continue;
            }
            let fragments = self.split(block, boxes, top);
            let (page, first_top) = fragments
                .first()
                .map_or((self.page, top), |f| (f.page, f.top));
            let began_page = began_page || fragments.len() > 1;
            let mut placed = self.land(block, index, first_top, fragments, carry, began_page);
            placed.page = page;
            return placed;
        }
    }

    fn available(&self, lines: &[(f64, f64)], origin: f64, first: usize) -> usize {
        lines
            .iter()
            .skip(first)
            .take_while(|&&(top, height)| self.fits(origin + top + height))
            .count()
    }

    /// Split a block that does not fit whole, starting with its top at `top` on the current page;
    /// the first piece may hold no lines only when the page is empty.
    fn split(&mut self, block: &Block, lines: &[(f64, f64)], top: f64) -> Vec<Fragment> {
        let total = lines.len();
        let line_top = |index: usize| lines.get(index).map_or(0.0, |&(t, _)| t);
        let mut fragments = Vec::new();
        let mut first = 0;
        let mut origin = top;
        while first < total {
            let available = self.available(lines, origin, first);
            let mut take = block.fragmentation.lines_on_page(total, first, available);
            if take == 0 {
                take = available.max(1);
            }
            let end = first.saturating_add(take).min(total);
            fragments.push(Fragment {
                page: self.page,
                top: origin + line_top(first),
                lines: first..end,
            });
            self.placed = true;
            if end == total {
                let (last_top, last_height) = lines.last().copied().unwrap_or_default();
                self.y = origin + last_top + last_height;
                break;
            }
            self.next_page();
            first = end;
            origin = -line_top(first);
        }
        fragments
    }

    /// Record a block that went at `top`; `began_page` says whether its last piece is the first
    /// thing on its page.
    fn land(
        &mut self,
        block: &Block,
        index: usize,
        top: f64,
        fragments: Vec<Fragment>,
        carry: Option<Carry>,
        began_page: bool,
    ) -> Placed {
        let piece_top = fragments.last().map_or(top, |f| f.top);
        if fragments.is_empty() {
            self.y = top + block.height;
        }
        self.placed = true;
        self.fresh = false;
        self.kept = if block.keep_with_next {
            match self.kept {
                Some(k) if k.page == self.page => Some(k),
                _ => Some(Kept {
                    block: index,
                    page: self.page,
                    top: piece_top,
                    began_page,
                }),
            }
        } else {
            None
        };
        Placed {
            page: self.page,
            top,
            fragments,
            carry,
        }
    }
}

/// Line boxes asked for at most once, the first time they are needed.
enum LazyLines<F> {
    Pending(Option<F>),
    Ready(Vec<(f64, f64)>),
}

impl<F: FnOnce() -> Vec<(f64, f64)>> LazyLines<F> {
    fn get(&mut self) -> &[(f64, f64)] {
        if let Self::Pending(f) = self {
            let boxes = f.take().map(|f| f()).unwrap_or_default();
            *self = Self::Ready(boxes);
        }
        match self {
            Self::Ready(boxes) => boxes,
            Self::Pending(_) => &[],
        }
    }
}

/// What a block is, as far as filling pages is concerned: flowing text splits between its
/// lines with orphans and widows, a heading moves with the block after it rather than end a
/// page, and anything else (a rule, a picture, a table row group) breaks where its producer
/// says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Heading,
    Other,
}

impl Kind {
    /// The lines this kind of a block of `rows` rows may leave on either side of a break.
    /// Short text and headings break anywhere: a heading never strands at the foot of a page
    /// for want of a legal split below it.
    #[must_use]
    pub fn fragmentation(self, rows: usize) -> Fragmentation {
        match self {
            Self::Text if rows >= 4 => Fragmentation::default(),
            Self::Heading | Self::Text | Self::Other => Fragmentation::ANYWHERE,
        }
    }

    /// Whether the block moves to the next page with the one after it rather than end a page.
    #[must_use]
    pub fn keeps_with_next(self) -> bool {
        matches!(self, Self::Heading)
    }
}

/// One block as the page fill sees it: what it is, the space before it, its height, and the
/// rows (lines, table rows) it may split between, each row's top and height below the
/// block's top. A block with no rows goes whole.
#[derive(Clone, Copy)]
pub struct Piece<'a> {
    pub kind: Kind,
    /// Space above the block, dropped where the block begins a page.
    pub space_before: f64,
    pub height: f64,
    pub rows: &'a [(f64, f64)],
    /// Starts a fresh page, unless it would be the first thing on one.
    pub break_before: bool,
}

/// Fills equal pages with blocks as they are set, recording where each page begins so a view
/// can show the pages it already has. Work is in the flow's coordinates: everything is laid
/// out as one column, and a break ends the page at the first row that no longer fits, so the
/// flow's height is the bottom of what was placed.
#[derive(Clone, Debug)]
pub struct Paginator {
    page_h: f64,
    flow: PageFlow,
    /// The top of each block handed over, in the flow's coordinates.
    block_tops: Vec<f64>,
    /// Where each page begins, in the flow's coordinates.
    pages: Vec<f64>,
    /// How many of `pages` were handed out.
    handed: usize,
}

impl Paginator {
    #[must_use]
    pub fn new(page_h: f64) -> Self {
        Self::resume(page_h, 0, 0.0, false)
    }

    /// A fill continuing `y` below the top of `page`; `placed` says whether anything is on
    /// that page already. A resumed fill knows no page starts before it took over.
    #[must_use]
    pub fn resume(page_h: f64, page: usize, y: f64, placed: bool) -> Self {
        Self {
            page_h,
            flow: PageFlow::resume(page_h, page, y, placed),
            block_tops: Vec::new(),
            pages: Vec::new(),
            handed: 0,
        }
    }

    /// The top of the next block's space before, in the flow's coordinates.
    #[must_use]
    pub fn cursor(&self) -> f64 {
        self.pages.last().copied().unwrap_or(0.0) + self.flow.y()
    }

    /// The page the flow is on.
    #[must_use]
    pub fn page(&self) -> usize {
        self.flow.page()
    }

    /// The bottom of what was placed, below the current page's top.
    #[must_use]
    pub fn y(&self) -> f64 {
        self.flow.y()
    }

    pub fn set_y(&mut self, y: f64) {
        self.flow.set_y(y);
    }

    pub fn skip(&mut self, amount: f64) {
        self.flow.skip(amount);
    }

    /// Place a block, breaking pages as needed. The `Placed` says where it went; every page
    /// whose beginning it decided is recorded.
    pub fn push(&mut self, piece: &Piece<'_>) -> Placed {
        if self.page_h <= 1.0 {
            // A page with no room cannot break: everything is on the one page.
            if self.pages.is_empty() {
                self.pages.push(0.0);
            }
            self.flow.skip(piece.space_before + piece.height);
            return Placed {
                page: self.flow.page(),
                top: self.flow.y(),
                fragments: Vec::new(),
                carry: None,
            };
        }
        let top = self.cursor() + piece.space_before;
        let block = Block::new(piece.height)
            .with_lead(piece.space_before)
            .breaking_before(piece.break_before)
            .keeping_with_next(piece.kind.keeps_with_next())
            .fragmented(piece.kind.fragmentation(piece.rows.len()));
        self.block_tops.push(top);
        let rows = piece.rows;
        let placed = self.flow.place_lines(&block, || rows.to_vec());
        if let Some(carry) = placed.carry
            && let Some(&carried) = self.block_tops.get(carry.block)
        {
            self.start_page(carry.from_page.saturating_add(1), carried);
        }
        if placed.fragments.is_empty() {
            self.start_page(placed.page, top);
        }
        for fragment in &placed.fragments {
            if let Some(&(row, _)) = rows.get(fragment.lines.start) {
                self.start_page(fragment.page, top + row);
            }
        }
        placed
    }

    /// Page starts decided since the last call.
    pub fn take_pages(&mut self) -> Vec<f64> {
        let fresh = self.pages.get(self.handed..).unwrap_or_default().to_vec();
        self.handed = self.pages.len();
        fresh
    }

    /// Every page start not yet taken; a flow with nothing to break has the one page at the
    /// top.
    pub fn finish_pages(&mut self) -> Vec<f64> {
        let pages = self.take_pages();
        if self.handed == 0 && pages.is_empty() {
            return vec![0.0];
        }
        pages
    }

    /// Record that page `page` starts at `y`, once.
    fn start_page(&mut self, page: usize, y: f64) {
        if page >= self.pages.len() {
            self.pages.push(y);
        }
    }
}
