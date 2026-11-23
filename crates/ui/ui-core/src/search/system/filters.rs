//! The native system-search filter strip: paint, hit targets and accessibility.
use super::ResourceKind;
use crate::num::{f32_of_f64, f64_of_usize};
use crate::paint::PaintCtx;
use crate::text_render::{TextRenderer, TextStyle, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
/// Toggle `kind` in the chip selection.
pub fn toggle_kind(selected: &mut Vec<ResourceKind>, kind: ResourceKind) {
    if let Some(pos) = selected.iter().position(|k| k == &kind) {
        selected.remove(pos);
    } else {
        selected.push(kind);
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilterAction {
    AllKinds,
    ToggleKind(ResourceKind),
    /// Open or close the overflow list of kinds that did not fit the row.
    More,
}
/// One kind chip: the kind it toggles, its label and its accessible name, in whatever
/// language the host shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KindChip {
    pub kind: ResourceKind,
    pub label: String,
    pub name: String,
}
impl KindChip {
    /// The chip with the kind's stable English label.
    pub fn english(kind: ResourceKind) -> Self {
        let label = kind.label().to_owned();
        let name = format!("Filter: {label}");
        Self { kind, label, name }
    }
}
/// The band's own copy: the reset chip and the group names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterText {
    pub all_label: String,
    pub all_name: String,
    pub more_label: String,
    pub more_name: String,
    pub filters_name: String,
    pub kinds_name: String,
}
impl Default for FilterText {
    fn default() -> Self {
        Self {
            all_label: "All".into(),
            all_name: "Filter: All".into(),
            more_label: "More".into(),
            more_name: "Filter: More".into(),
            filters_name: "Search filters".into(),
            kinds_name: "Result kinds".into(),
        }
    }
}
/// The band a palette host reserves for the filter strip: where it sits, the height of
/// one chip row, and how many rows the current layout allows for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FilterStripSlot {
    pub band: Rect,
    pub row_height: f64,
}

#[derive(Clone)]
pub struct SearchFilters {
    pub search_chip_rects: Vec<Rect>,
    pub search_all_chip_rect: Rect,
    pub search_more_rect: Rect,
    pub more_open: bool,
    pub search_chip_rows: usize,
    pub search_filter_control_h: f64,
    pub search_pointer_floor_h: f64,
    pub search_band_at_draw: Rect,

    kinds: Vec<KindChip>,
    active_kind_filter: Vec<ResourceKind>,
    text: FilterText,
    band: Rect,
    row_height: f64,
    last_cursor: Option<Point>,
}
impl Default for SearchFilters {
    fn default() -> Self {
        Self {
            search_chip_rects: Vec::new(),
            search_all_chip_rect: Rect::ZERO,
            search_more_rect: Rect::ZERO,
            more_open: false,
            search_chip_rows: 1,
            search_filter_control_h: SELECTOR_HEIGHT_PX,
            search_pointer_floor_h: f64::from(crate::tokens::Metrics::BASE.min_pointer_target),
            search_band_at_draw: Rect::ZERO,

            kinds: Vec::new(),
            active_kind_filter: Vec::new(),
            text: FilterText::default(),
            band: Rect::ZERO,
            row_height: 36.0,
            last_cursor: None,
        }
    }
}
impl SearchFilters {
    pub fn configure(
        &mut self,
        kinds: Vec<KindChip>,
        selected: Vec<ResourceKind>,
        text: FilterText,
    ) {
        if !self.kinds.is_empty() && self.kinds != kinds {
            self.more_open = false;
        }
        self.kinds = kinds;
        self.active_kind_filter = selected;
        self.text = text;
    }
    /// The kinds the chips toggle, in chip order.
    pub fn kinds(&self) -> impl Iterator<Item = &ResourceKind> {
        self.kinds.iter().map(|chip| &chip.kind)
    }
    /// Paint into a host's filter band.
    pub fn draw(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        slot: FilterStripSlot,
        hover: Option<Point>,
    ) -> usize {
        self.band = slot.band;
        self.row_height = slot.row_height;
        self.last_cursor = hover;
        self.draw_search_banner(ctx.scene, ctx.transform, ctx.text, ctx.theme);
        self.search_chip_rows
    }
    pub fn search_banner_rect(&self) -> Rect {
        self.band
    }
    pub fn accessibility(&self) -> crate::a11y::AccessNode {
        self.search_filters_accessibility()
    }
    pub fn rect_for_path(&self, path: &[usize]) -> Option<Rect> {
        self.search_filter_rect_for_path(path)
    }
    pub fn action_for_path(&self, path: &[usize]) -> Option<FilterAction> {
        match path {
            [0, 0] => Some(FilterAction::AllKinds),
            [0, i] if *i == self.kinds.len().saturating_add(1) => Some(FilterAction::More),
            [0, i] => self
                .kinds
                .get(i.checked_sub(1)?)
                .map(|chip| FilterAction::ToggleKind(chip.kind.clone())),
            _ => None,
        }
    }
    pub fn hit(&self, point: Point) -> Option<FilterAction> {
        if self.search_all_chip_hit_rect().contains(point) {
            return Some(FilterAction::AllKinds);
        }
        if self.search_more_hit_rect().contains(point) {
            return Some(FilterAction::More);
        }
        for i in 0..self.kinds.len() {
            if self.search_chip_hit_rect(i).contains(point) {
                return self.action_for_path(&[0, i + 1]);
            }
        }
        None
    }
    pub fn search_filter_slug(label: &str) -> String {
        let mut slug = String::with_capacity(label.len());
        for c in label.chars() {
            if c.is_ascii_alphanumeric() {
                slug.push(c.to_ascii_lowercase());
            } else if !slug.ends_with('-') {
                slug.push('-');
            }
        }
        slug.trim_matches('-').to_string()
    }

    pub fn search_filter_key(kinds: &[ResourceKind], i: usize) -> String {
        let slug = |k: &ResourceKind| Self::search_filter_slug(k.label());
        let Some(kind) = kinds.get(i) else {
            return format!("palette:filter:{i}");
        };
        let mine = slug(kind);
        let taken = kinds.iter().take(i).any(|k| slug(k) == mine);
        if mine.is_empty() || taken {
            format!("palette:filter:{mine}-{i}")
        } else {
            format!("palette:filter:{mine}")
        }
    }

    pub fn search_filters_accessibility(&self) -> crate::a11y::AccessNode {
        use crate::a11y::{AccessNode, AccessState, Role};
        let bounded = |node: AccessNode, rect: Rect, checked: bool| {
            let node = node.with_state(AccessState {
                checked: Some(checked),
                selected: checked,
                hidden: rect.area() <= 0.0,
                ..AccessState::default()
            });
            if rect.area() > 0.0 {
                node.with_bounds(rect)
            } else {
                node
            }
        };

        // Bounds are the pointer targets (`selector_hit_rect`: the painted pill grown to
        // the §9.6 floor), the same rects a press is tested against — what a reader names
        // is exactly what a finger hits.
        let kinds: Vec<ResourceKind> = self.kinds().cloned().collect();
        let mut chips = Vec::with_capacity(kinds.len().saturating_add(1));
        chips.push(bounded(
            AccessNode::new(Role::ToggleButton)
                .with_name(self.text.all_name.clone())
                .with_semantic_key("palette:filter:all")
                .with_action_target("palette:filter:all")
                .with_key(0),
            self.search_all_chip_hit_rect(),
            self.active_kind_filter.is_empty(),
        ));
        for (i, chip) in self.kinds.iter().enumerate() {
            let kind = &chip.kind;
            let key = Self::search_filter_key(&kinds, i);
            chips.push(bounded(
                AccessNode::new(Role::ToggleButton)
                    .with_name(chip.name.clone())
                    .with_semantic_key(key.clone())
                    .with_action_target(key)
                    .with_key(u64::try_from(i.saturating_add(1)).unwrap_or(u64::MAX)),
                self.search_chip_hit_rect(i),
                self.active_kind_filter.contains(kind),
            ));
        }
        if self.search_more_rect.area() > 0.0 {
            chips.push(bounded(
                AccessNode::new(Role::ToggleButton)
                    .with_name(self.text.more_name.clone())
                    .with_semantic_key("palette:filter:more")
                    .with_action_target("palette:filter:more")
                    .with_key(u64::try_from(kinds.len().saturating_add(1)).unwrap_or(u64::MAX)),
                self.search_more_hit_rect(),
                self.more_open,
            ));
        }

        AccessNode::new(Role::Group)
            .with_name(self.text.filters_name.clone())
            .with_semantic_key("palette:filters")
            .with_children(vec![
                AccessNode::new(Role::Group)
                    .with_name(self.text.kinds_name.clone())
                    .with_children(chips),
            ])
    }

    pub fn search_filter_rect_for_path(&self, path: &[usize]) -> Option<Rect> {
        let rect = match path {
            [0, 0] => self.search_all_chip_hit_rect(),
            [0, i] if *i == self.kinds.len().saturating_add(1) => self.search_more_hit_rect(),
            [0, i] => self.search_chip_hit_rect(i.checked_sub(1)?),
            _ => return None,
        };
        (rect.area() > 0.0).then_some(rect)
    }

    pub fn selector_hit_rect(&self, painted: Rect) -> Rect {
        if painted.area() <= 0.0 {
            return Rect::ZERO;
        }
        // The floor is already clamped to `row_h − s1` at draw
        // (`search_pointer_floor_h`), so two rows' targets never overlap however the
        // palette scales its rows; clipping to the band keeps the first and last rows'
        // targets off the query field and the results.
        let h = painted.height().max(self.search_pointer_floor_h);
        let cy = painted.center().y;
        let grown = Rect::new(painted.x0, cy - h / 2.0, painted.x1, cy + h / 2.0);
        // Clipped to the band the pill was laid out in (frozen at draw), never to a band
        // a pending row request has already moved.
        let band = self.search_band_for_targets();
        if band.area() <= 0.0 || painted.intersect(band).area() <= 0.0 {
            return grown;
        }
        let clipped = grown.intersect(band);
        if clipped.area() <= 0.0 {
            painted
        } else {
            clipped
        }
    }

    pub fn search_all_chip_hit_rect(&self) -> Rect {
        self.selector_hit_rect(self.search_all_chip_rect)
    }

    pub fn search_chip_hit_rect(&self, i: usize) -> Rect {
        self.selector_hit_rect(self.search_chip_rects.get(i).copied().unwrap_or(Rect::ZERO))
    }

    pub fn search_more_hit_rect(&self) -> Rect {
        self.selector_hit_rect(self.search_more_rect)
    }

    /// The band the pills were laid out in when they were last drawn: hit targets keep to
    /// it, whatever a later layout asks for.
    pub fn search_band_for_targets(&self) -> Rect {
        if self.search_band_at_draw.area() > 0.0 {
            self.search_band_at_draw
        } else {
            self.search_banner_rect()
        }
    }

    /// The role every filter pill is set in.
    pub fn filter_label_style(theme: &Theme) -> TextStyle {
        theme.text(TextRole::Label)
    }

    pub fn filter_control_height(
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        row_h: f64,
    ) -> f64 {
        let style = Self::filter_label_style(theme);
        let (_, lh) = text_renderer.measure_style("Ag", None, &style);
        let s1 = f64::from(theme.tokens.spacing.s1);
        (f64::from(lh) + f64::from(theme.tokens.spacing.s2))
            .max(f64::from(theme.tokens.metrics.selector_height))
            .min(row_h - s1)
    }

    pub fn draw_search_banner(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let band = self.search_banner_rect();
        if band.width() <= 0.0 {
            self.search_chip_rects.clear();
            return;
        }

        let chip_text_style = Self::filter_label_style(theme);
        let row_h = self.row_height;
        let control_h = Self::filter_control_height(theme, text_renderer, row_h);
        self.search_filter_control_h = control_h;
        // The pointer floor a target may grow to: §9.6's 32, but never past `row_h − s1`,
        // so neighbouring rows' targets keep the gutter the pills keep (the palette's row
        // is `36·text_scale`; below 100 % text it is the row, not the floor, that bounds
        // the target).
        self.search_pointer_floor_h = f64::from(tokens.metrics.min_pointer_target)
            .min(row_h - f64::from(tokens.spacing.s1))
            .max(control_h);

        let labels: Vec<String> = std::iter::once(self.text.all_label.clone())
            .chain(self.kinds.iter().map(|chip| chip.label.clone()))
            .collect();
        let chip_pad = f64::from(tokens.spacing.s3);
        let chip_gap = f64::from(tokens.spacing.s2);
        let natural: Vec<f64> = labels
            .iter()
            .map(|l| {
                f64::from(text_renderer.measure_style(l, None, &chip_text_style).0) + 2.0 * chip_pad
            })
            .collect();
        let chip_band = band;
        let first_row_x1 = band.x1;
        let more_w = f64::from(
            text_renderer
                .measure_style(&self.text.more_label, None, &chip_text_style)
                .0,
        ) + 2.0 * chip_pad;
        let row = lay_out_chip_row_with_more(
            &natural,
            more_w,
            chip_band,
            first_row_x1,
            row_h,
            control_h,
            chip_gap,
        );
        let mut rects = row.rects;
        if self.more_open && !row.overflow.is_empty() {
            let menu_origin = row
                .more
                .map(|more| Point::new(more.x0, chip_band.y1 + chip_gap))
                .unwrap_or(Point::new(chip_band.x0, chip_band.y1 + chip_gap));
            for (idx, rect) in
                lay_out_more_menu(&natural, &row.overflow, menu_origin, control_h, chip_gap)
            {
                if let Some(slot) = rects.get_mut(idx) {
                    *slot = rect;
                }
            }
        }

        let mut chip_rects: Vec<Rect> = Vec::with_capacity(self.kinds.len());
        for (i, label) in labels.iter().enumerate() {
            let rect = rects.get(i).copied().unwrap_or(Rect::ZERO);
            let is_active = match i {
                0 => self.active_kind_filter.is_empty(),
                _ => i
                    .checked_sub(1)
                    .and_then(|k| self.kinds.get(k))
                    .is_some_and(|chip| self.active_kind_filter.contains(&chip.kind)),
            };
            let hover = self
                .last_cursor
                .is_some_and(|p| self.selector_hit_rect(rect).contains(p));
            if rect.area() > 0.0 {
                let label = text_renderer.elide_to_width_style(
                    label,
                    f32_of_f64((rect.width() - chip_pad).max(1.0)),
                    &chip_text_style,
                );
                draw_filter_chip(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    rect,
                    &label,
                    &chip_text_style,
                    crate::chrome::SelectionState::new(is_active, hover, false),
                );
            }
            match i {
                0 => self.search_all_chip_rect = rect,
                _ => chip_rects.push(rect),
            }
        }
        self.search_chip_rects = chip_rects;
        self.search_more_rect = row.more.unwrap_or(Rect::ZERO);
        if let Some(more) = row.more {
            let hover = self
                .last_cursor
                .is_some_and(|p| self.selector_hit_rect(more).contains(p));
            draw_filter_chip(
                scene,
                transform,
                text_renderer,
                theme,
                more,
                &self.text.more_label,
                &chip_text_style,
                crate::chrome::SelectionState::new(self.more_open, hover, false),
            );
        }

        // One chip row: overflow kinds live under More, so the band does not grow.
        self.search_chip_rows = 1;
    }
}

/// Narrowest a chip is ever squeezed to before its label is just an ellipsis: the §9.6
/// pointer floor — a pill narrower than a finger is not a target whatever it says.
const CHIP_MIN_W_PX: f64 = crate::tokens::Metrics::BASE.min_pointer_target as f64;
/// Height of a Selector at 100 % text (HIG §9.6, `metrics.selector_height`).
pub const SELECTOR_HEIGHT_PX: f64 = crate::tokens::Metrics::BASE.selector_height as f64;

/// One row of filter chips, with kinds that did not fit sitting behind More.
#[derive(Debug, Clone, PartialEq)]
pub struct ChipRow {
    pub rects: Vec<Rect>,
    /// The More chip, present only when at least one kind did not fit.
    pub more: Option<Rect>,
    /// Indices into `rects` / the caller's labels that sit behind More.
    pub overflow: Vec<usize>,
}

/// Lays chips on a single row at their natural width.
pub fn lay_out_chip_row_with_more(
    natural: &[f64],
    more_w: f64,
    band: Rect,
    first_row_x1: f64,
    row_h: f64,
    control_h: f64,
    gap: f64,
) -> ChipRow {
    if natural.is_empty() || band.width() <= 0.0 {
        return ChipRow {
            rects: Vec::new(),
            more: None,
            overflow: Vec::new(),
        };
    }
    let x1 = first_row_x1.min(band.x1);
    let available = (x1 - band.x0).max(0.0);
    let more_w = more_w.max(CHIP_MIN_W_PX);
    let widths: Vec<f64> = natural.iter().map(|w| w.max(CHIP_MIN_W_PX)).collect();
    let all_w =
        widths.iter().copied().sum::<f64>() + gap * f64_of_usize(widths.len().saturating_sub(1));
    let cy = band.y0 + row_h / 2.0;
    let place = |x: f64, w: f64| Rect::new(x, cy - control_h / 2.0, x + w, cy + control_h / 2.0);
    if all_w <= available + 0.001 {
        let mut rects = Vec::with_capacity(widths.len());
        let mut x = band.x0;
        for w in widths {
            rects.push(place(x, w));
            x += w + gap;
        }
        return ChipRow {
            rects,
            more: None,
            overflow: Vec::new(),
        };
    }
    let reserved = more_w + gap;
    let fit_budget = (available - reserved).max(0.0);
    let mut used = 0.0;
    let mut visible = 0usize;
    for (i, w) in widths.iter().enumerate() {
        let next = if visible == 0 { *w } else { used + gap + *w };
        if next > fit_budget + 0.001 && visible > 0 {
            break;
        }
        used = next;
        visible = i + 1;
    }
    visible = visible.max(1).min(widths.len());
    let mut rects = vec![Rect::ZERO; widths.len()];
    let mut x = band.x0;
    for (i, w) in widths.iter().enumerate().take(visible) {
        if let Some(rect) = rects.get_mut(i) {
            *rect = place(x, *w);
        }
        x += *w + gap;
    }
    let more = place(x, more_w);
    let overflow = (visible..widths.len()).collect();
    ChipRow {
        rects,
        more: Some(more),
        overflow,
    }
}

/// Stacks overflow chips under `origin` so every leftover kind stays a target.
pub fn lay_out_more_menu(
    natural: &[f64],
    overflow: &[usize],
    origin: Point,
    control_h: f64,
    gap: f64,
) -> Vec<(usize, Rect)> {
    let mut y = origin.y;
    let mut out = Vec::with_capacity(overflow.len());
    for &idx in overflow {
        let w = natural
            .get(idx)
            .copied()
            .unwrap_or(CHIP_MIN_W_PX)
            .max(CHIP_MIN_W_PX);
        out.push((idx, Rect::new(origin.x, y, origin.x + w, y + control_h)));
        y += control_h + gap;
    }
    out
}

/// Widths for one row of chips that must all fit inside `available`.
pub fn share_row(natural: &[f64], available: f64, gap: f64) -> Vec<f64> {
    if natural.is_empty() {
        return Vec::new();
    }
    let n = f64_of_usize(natural.len());
    let gaps = gap * (n - 1.0);
    let total: f64 = natural.iter().sum();
    if total + gaps <= available {
        return natural.iter().map(|w| w.max(CHIP_MIN_W_PX)).collect();
    }
    let base = ((available - gaps) / n).max(CHIP_MIN_W_PX);
    let spare: f64 = natural
        .iter()
        .filter(|w| **w < base)
        .map(|w| base - w)
        .sum();
    let over = natural.iter().filter(|w| **w >= base).count();
    let share_over = if over > 0 {
        base + spare / f64_of_usize(over)
    } else {
        base
    };
    natural
        .iter()
        .map(|w| {
            if *w < base {
                w.max(CHIP_MIN_W_PX)
            } else {
                share_over.min(*w).max(CHIP_MIN_W_PX)
            }
        })
        .collect()
}

/// One pill in the search card's filter band, shared by the kind chips and the "All"
/// reset chip.
pub fn draw_filter_chip(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    text_style: &TextStyle,
    state: crate::chrome::SelectionState,
) {
    let colors = &theme.tokens.colors;
    let radius = crate::chrome::ChromeRole::Selector.radius(theme, rect.height());
    let tier = crate::chrome::selection_tier(theme, state);
    tier.paint(scene, transform, rect, radius);
    let lifted = state.active || state.pressed || state.hover;
    let under = if lifted {
        tier.fill
    } else {
        crate::chrome::shell::panel_ground(theme)
    };
    let tone = if state.active {
        colors.text_primary
    } else {
        colors.text_dim
    };
    let text_color = colors.ink_on(under, tone, crate::tokens::colors::ON_FILL_MIN_CONTRAST);
    // Control labels are medium weight (HIG §9.6); measured and drawn at the same weight
    // so the centring holds.
    let (lw, lh) = text_renderer.measure_style(
        label,
        None,
        &text_style.clone().with_weight(TextWeight::Medium),
    );
    text_renderer.draw_style(
        scene,
        label,
        text_color,
        transform.pre_translate(
            (
                rect.x0 + (rect.width() - f64::from(lw)) / 2.0,
                rect.y0 + (rect.height() - f64::from(lh)) / 2.0,
            )
                .into(),
        ),
        None,
        &text_style.clone().with_weight(TextWeight::Medium),
    );
}
