//! The native system-search filter strip: paint, hit targets and accessibility.
use super::{ResourceKind, SearchScope};
use crate::paint::PaintCtx;
use crate::text_render::{TextRenderer, TextStyle, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
const SEGMENT_INSET_PX: f64 = 2.0;
#[allow(
    clippy::cast_precision_loss,
    reason = "Item counts become approximate display geometry rather than durable numeric state."
)]
fn usize_to_f64_lossy(n: usize) -> f64 {
    n as f64
}
#[allow(
    clippy::cast_possible_truncation,
    reason = "Painting uses f32 coordinates after layout computes f64 geometry."
)]
fn f64_to_f32_lossy(n: f64) -> f32 {
    n as f32
}
/// Toggle `kind` in the chip selection. Content kinds combine; the empty
/// selection means everything shows.
///
/// Applications and Host Apps are the exception: they are alternative
/// launch sources, not additive content facets. A picker opened for a new
/// window starts with both sources available, and choosing Host Apps must
/// narrow to host rows rather than toggle that source out and leave the
/// Tilo-app rows behind.
pub fn toggle_kind(selected: &mut Vec<ResourceKind>, kind: ResourceKind) {
    if matches!(
        kind,
        ResourceKind::Application | ResourceKind::HostApplication
    ) {
        if selected.as_slice() == std::slice::from_ref(&kind) {
            selected.clear();
        } else {
            *selected = vec![kind];
        }
        return;
    }
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
    Scope(SearchScope),
    /// Open or close the overflow list of kinds that did not fit the row.
    More,
}
/// One kind chip: the kind it toggles, its label and its accessible name,
/// in whatever language the host shows.
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
/// One scope segment: its scope, label and accessible name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeSegment {
    pub scope: SearchScope,
    pub label: String,
    pub name: String,
}
impl ScopeSegment {
    /// The segment named in English after `label`.
    pub fn english(scope: SearchScope, label: String) -> Self {
        let name = format!("Scope: {label}");
        Self { scope, label, name }
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
    pub scopes_name: String,
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
            scopes_name: "Search scope".into(),
        }
    }
}
/// The band a palette host reserves for the filter strip: where it sits, the
/// height of one chip row, and how many rows the current layout allows for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FilterStripSlot {
    pub band: Rect,
    pub row_height: f64,
    pub reserved_rows: usize,
}

#[derive(Clone)]
pub struct SearchFilters {
    pub search_chip_rects: Vec<Rect>,
    pub search_all_chip_rect: Rect,
    pub search_more_rect: Rect,
    pub more_open: bool,
    pub search_chip_rows: usize,
    pub search_filter_control_h: f64,
    pub search_scope_segment_w: f64,
    pub search_pointer_floor_h: f64,
    pub search_band_at_draw: Rect,
    pub search_scope_own_row: bool,

    kinds: Vec<KindChip>,
    active_kind_filter: Vec<ResourceKind>,
    scopes: Vec<ScopeSegment>,
    active_scope: SearchScope,
    text: FilterText,
    band: Rect,
    row_height: f64,
    reserved_rows: usize,
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
            search_scope_segment_w: 0.0,
            search_pointer_floor_h: f64::from(crate::tokens::Metrics::BASE.min_pointer_target),
            search_band_at_draw: Rect::ZERO,
            search_scope_own_row: false,

            kinds: Vec::new(),
            active_kind_filter: Vec::new(),
            scopes: Vec::new(),
            active_scope: SearchScope::default(),
            text: FilterText::default(),
            band: Rect::ZERO,
            row_height: 36.0,
            reserved_rows: 1,
            last_cursor: None,
        }
    }
}
impl SearchFilters {
    pub fn configure(
        &mut self,
        kinds: Vec<KindChip>,
        selected: Vec<ResourceKind>,
        scopes: Vec<ScopeSegment>,
        active_scope: SearchScope,
        text: FilterText,
    ) {
        if !self.kinds.is_empty() && self.kinds != kinds {
            self.more_open = false;
        }
        self.kinds = kinds;
        self.active_kind_filter = selected;
        self.scopes = scopes;
        self.active_scope = active_scope;
        self.text = text;
    }
    /// The kinds the chips toggle, in chip order.
    pub fn kinds(&self) -> impl Iterator<Item = &ResourceKind> {
        self.kinds.iter().map(|chip| &chip.kind)
    }
    /// Paint into the CommandPalette host strip. Return the row reservation for the next layout.
    pub fn draw(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        slot: FilterStripSlot,
        hover: Option<Point>,
    ) -> usize {
        self.band = slot.band;
        self.row_height = slot.row_height;
        self.reserved_rows = slot.reserved_rows;
        self.last_cursor = hover;
        self.draw_search_banner(ctx.scene, ctx.transform, ctx.text, ctx.theme);
        self.search_chip_rows
    }
    pub fn search_banner_rect(&self) -> Rect {
        self.band
    }
    fn search_scope_list(&self) -> Vec<SearchScope> {
        self.scopes.iter().map(|segment| segment.scope).collect()
    }
    fn search_scope_pill_label(&self, scope: SearchScope) -> String {
        self.scopes
            .iter()
            .find(|segment| segment.scope == scope)
            .map(|segment| segment.label.clone())
            .unwrap_or_else(|| scope.label().into())
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
            [1, i] => self
                .scopes
                .get(*i)
                .map(|segment| FilterAction::Scope(segment.scope)),
            _ => None,
        }
    }
    pub fn hit(&self, point: Point) -> Option<FilterAction> {
        for i in 0..self.scopes.len() {
            if self.search_scope_segment_hit_rect(i).contains(point) {
                return self.action_for_path(&[1, i]);
            }
        }
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
        // `checked` is the toggle/radio state AT reads; `selected` mirrors
        // it so "which one is chosen" also answers the selection question
        // a list-style reader (and the bench's `selected` check) asks.
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

        // Bounds are the pointer targets (`selector_hit_rect`: the painted
        // pill grown to the §9.6 floor), the same rects a press is tested
        // against — what a reader names is exactly what a finger hits.
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

        let active_scope = self.active_scope;
        let scopes = self
            .scopes
            .iter()
            .enumerate()
            .map(|(idx, segment)| {
                let scope = segment.scope;
                let key = format!("palette:scope:{}", Self::search_filter_slug(scope.label()));
                bounded(
                    AccessNode::new(Role::RadioButton)
                        .with_name(segment.name.clone())
                        .with_semantic_key(key.clone())
                        .with_action_target(key)
                        .with_key(u64::try_from(idx).unwrap_or(u64::MAX)),
                    self.search_scope_segment_hit_rect(idx),
                    scope == active_scope,
                )
            })
            .collect();

        AccessNode::new(Role::Group)
            .with_name(self.text.filters_name.clone())
            .with_semantic_key("palette:filters")
            .with_children(vec![
                AccessNode::new(Role::Group)
                    .with_name(self.text.kinds_name.clone())
                    .with_children(chips),
                AccessNode::new(Role::Group)
                    .with_name(self.text.scopes_name.clone())
                    .with_children(scopes),
            ])
    }

    pub fn search_filter_rect_for_path(&self, path: &[usize]) -> Option<Rect> {
        let rect = match path {
            [0, 0] => self.search_all_chip_hit_rect(),
            [0, i] if *i == self.kinds.len().saturating_add(1) => self.search_more_hit_rect(),
            [0, i] => self.search_chip_hit_rect(i.checked_sub(1)?),
            [1, i] => self.search_scope_segment_hit_rect(*i),
            _ => return None,
        };
        (rect.area() > 0.0).then_some(rect)
    }

    pub fn search_scope_pill_rect(&self, idx: usize) -> Rect {
        let b = self.search_banner_rect();
        if b.width() <= 0.0 {
            return Rect::ZERO;
        }
        let count = self.search_scope_list().len().max(1);
        let track = self.search_scope_track_rect();
        let seg_w = (track.width() - 2.0 * SEGMENT_INSET_PX) / usize_to_f64_lossy(count);
        let x0 = track.x0 + SEGMENT_INSET_PX + usize_to_f64_lossy(idx) * seg_w;
        Rect::new(
            x0,
            track.y0 + SEGMENT_INSET_PX,
            x0 + seg_w,
            track.y1 - SEGMENT_INSET_PX,
        )
    }

    pub fn search_scope_segment_hit_rect(&self, idx: usize) -> Rect {
        let seg = self.search_scope_pill_rect(idx);
        if seg.area() <= 0.0 {
            return Rect::ZERO;
        }
        let track = self.search_scope_track_rect();
        self.selector_hit_rect(Rect::new(seg.x0, track.y0, seg.x1, track.y1))
    }

    pub fn selector_hit_rect(&self, painted: Rect) -> Rect {
        if painted.area() <= 0.0 {
            return Rect::ZERO;
        }
        // The floor is already clamped to `row_h − s1` at draw
        // (`search_pointer_floor_h`), so two rows' targets never overlap
        // however the palette scales its rows; clipping to the band keeps
        // the first and last rows' targets off the query field and the
        // results.
        let h = painted.height().max(self.search_pointer_floor_h);
        let cy = painted.center().y;
        let grown = Rect::new(painted.x0, cy - h / 2.0, painted.x1, cy + h / 2.0);
        // Clipped to the band the pill was laid out in (frozen at draw),
        // never to a band a pending row request has already moved.
        // Overflow chips under More sit below that band — keep their
        // own target rather than clipping them to a sliver or away.
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

    pub fn search_scope_measured_track_w(&self) -> f64 {
        let count = self.search_scope_list().len().max(1);
        self.search_scope_segment_w * usize_to_f64_lossy(count) + 2.0 * SEGMENT_INSET_PX
    }

    pub fn scope_switch_wants_own_row(&self) -> bool {
        let b = self.search_banner_rect();
        b.width() > 0.0 && self.search_scope_measured_track_w() > b.width() * SCOPE_TRACK_SHARE
    }

    pub fn scope_switch_has_own_row(&self) -> bool {
        self.search_scope_own_row
    }

    pub fn search_band_for_targets(&self) -> Rect {
        if self.search_band_at_draw.area() > 0.0 {
            self.search_band_at_draw
        } else {
            self.search_banner_rect()
        }
    }

    pub fn search_scope_track_rect(&self) -> Rect {
        let b = self.search_band_for_targets();
        if b.width() <= 0.0 {
            return Rect::ZERO;
        }
        // Segments are as wide as the widest label needs (measured at
        // draw). While the switch shares the first row it is capped at a
        // share of the band so the kind chips keep that row; on its own
        // row it may take the whole band.
        let measured = self.search_scope_measured_track_w();
        let w = if self.scope_switch_has_own_row() {
            measured.min(b.width())
        } else {
            measured.min(b.width() * SCOPE_TRACK_SHARE)
        };
        let h = self.search_filter_control_h;
        // Anchored to the *first* row of the band, not to the middle of
        // it: when the kind chips wrap to a second row the switch must
        // stay where it was, beside the row it shares, rather than
        // drifting down into the gap between the two rows of chips.
        let cy = b.y0 + self.row_height / 2.0;
        Rect::new(b.x1 - w, cy - h / 2.0, b.x1, cy + h / 2.0)
    }

    /// The role every filter pill and switch label is set in.
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

    pub fn scope_segment_width(&self, theme: &Theme, text_renderer: &mut TextRenderer) -> f64 {
        let style = Self::filter_label_style(theme);
        let pad = 2.0 * f64::from(theme.tokens.spacing.s3);
        self.search_scope_list()
            .into_iter()
            .map(|scope| {
                let label = self.search_scope_pill_label(scope);
                f64::from(text_renderer.measure_style(&label, None, &style).0) + pad
            })
            .fold(0.0, f64::max)
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
        // Cached before any geometry helper runs, so the switch painted
        // below and the hit rects consulted later agree.
        self.search_filter_control_h = control_h;
        // The pointer floor a target may grow to: §9.6's 32, but never past
        // `row_h − s1`, so neighbouring rows' targets keep the gutter the
        // pills keep (the palette's row is `36·text_scale`; below 100 %
        // text it is the row, not the floor, that bounds the target).
        self.search_pointer_floor_h = f64::from(tokens.metrics.min_pointer_target)
            .min(row_h - f64::from(tokens.spacing.s1))
            .max(control_h);
        self.search_scope_segment_w = self.scope_segment_width(theme, text_renderer);
        // Frozen for this frame: the band everything below is laid out in,
        // and whether the switch takes its first row alone (it wants to
        // and the palette has already granted the second row). Hit-tests
        // read these, not the palette's live getters, so a row request made
        // a few lines down cannot move the targets before the next paint.
        self.search_band_at_draw = band;
        self.search_scope_own_row =
            self.scope_switch_wants_own_row() && band.height() + 0.5 >= 2.0 * row_h;

        self.draw_search_banner_scope_switch(
            scene,
            transform,
            text_renderer,
            theme,
            &chip_text_style,
        );

        // Dynamic: one chip per kind any registered provider exposes, so
        // a new provider gets a chip for free. Empty selection means
        // "everything shows", stated by the leading "All" chip. Every
        // chip is painted regardless of width; see [`lay_out_chip_strip`].
        let labels: Vec<String> = std::iter::once(self.text.all_label.clone())
            .chain(self.kinds.iter().map(|chip| chip.label.clone()))
            .collect();
        // Chip padding is `s3` either side — the shared `Chip` primitive's
        // own padding, so a filter chip and a chip anywhere else in the
        // shell are one shape — and chips sit `s2` apart. Both are the
        // live theme's steps, so a custom or scaled theme moves them too.
        let chip_pad = f64::from(tokens.spacing.s3);
        let chip_gap = f64::from(tokens.spacing.s2);
        let natural: Vec<f64> = labels
            .iter()
            .map(|l| {
                f64::from(text_renderer.measure_style(l, None, &chip_text_style).0) + 2.0 * chip_pad
            })
            .collect();
        // The scope track's left edge bounds the chip strip's first row —
        // unless the switch has taken that row for itself (200 % text on a
        // narrow card), in which case the chips start on the next row with
        // the whole band and the switch keeps its labels whole (S1-29).
        let track = self.search_scope_track_rect();
        // `has` decides this frame's layout, `wants` what the band asks the
        // palette to reserve: on the frame the want first appears the band
        // is one row tall, so the switch still shares that row (capped) and
        // the chips stay in it — never laid into a row that does not exist
        // yet, over the first result — while the request goes out; the next
        // layout grants the row and the switch takes it.
        let own_row = self.scope_switch_has_own_row();
        let wants_own_row = self.scope_switch_wants_own_row();
        let reserved_rows = self.reserved_rows;
        let (chip_band, first_row_x1, _rows_for_chips) = if own_row {
            (
                Rect::new(band.x0, band.y0 + row_h, band.x1, band.y1),
                band.x1,
                reserved_rows.saturating_sub(1).max(1),
            )
        } else {
            (band, track.x0 - chip_pad, reserved_rows)
        };
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
                    f64_to_f32_lossy((rect.width() - chip_pad).max(1.0)),
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

        // One chip row; the scope switch may still take a row of its own.
        // Overflow kinds live under More, so the band does not grow.
        self.search_chip_rows = 1 + usize::from(wants_own_row);
    }

    pub fn draw_search_banner_scope_switch(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        chip_text_style: &TextStyle,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let ground = crate::chrome::shell::panel_ground(theme);

        let active = self.active_scope;
        let track = self.search_scope_track_rect();
        // The track is the set's outline, not a body: a hairline derived on
        // the panel ground (HIG §9.5), no fill — the segments carry the
        // state. Until 2026-09-17 it wore a `lift(0.35)` body with a
        // hairline measured against `surfaces.raised` and the chosen
        // segment an `accent[5]` fill (§9.4).
        let track_shape = RoundedRect::from_rect(
            track,
            crate::chrome::ChromeRole::Selector.radius(theme, track.height()),
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            crate::tokens::colors::Colors::hairline_on(
                ground,
                crate::chrome::shell::HAIRLINE_ALPHA,
            ),
            None,
            &track_shape,
        );
        let scopes = self.search_scope_list();
        for (idx, scope) in scopes.iter().enumerate() {
            let seg = self.search_scope_pill_rect(idx);
            let is_active = *scope == active;
            let hover = self
                .last_cursor
                .is_some_and(|p| self.search_scope_segment_hit_rect(idx).contains(p));
            // Each segment is a Selector on the shared tiers (§9.3): the
            // chosen one lifted with hairline and glow, a hovered one half
            // lifted, the rest transparent inside the track.
            let tier = crate::chrome::selection_tier(
                theme,
                crate::chrome::SelectionState::new(is_active, hover, false),
            );
            let radius = crate::chrome::ChromeRole::Selector.radius(theme, seg.height());
            let shape = RoundedRect::from_rect(seg, radius);
            if tier.fill.components[3] > 0.0 {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    tier.fill,
                    None,
                    &shape,
                );
            }
            if let Some(border) = tier.border {
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.0),
                    transform,
                    border,
                    None,
                    &shape,
                );
            }
            if let Some(glow) = tier.inner_glow {
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.0),
                    transform,
                    glow,
                    None,
                    &RoundedRect::from_rect(seg.inset(1.0), (radius - 1.0).max(0.0)),
                );
            }
            let under = if is_active || hover {
                tier.fill
            } else {
                ground
            };
            let tone = if is_active {
                colors.text_primary
            } else {
                colors.text_dim
            };
            let ink = colors.ink_on(under, tone, crate::tokens::colors::ON_FILL_MIN_CONTRAST);
            let text = self.search_scope_pill_label(*scope);
            // A segment that had to be squeezed under its measured width
            // keeps `s2` of its padding before it elides.
            let text = text_renderer.elide_to_width_style(
                &text,
                f64_to_f32_lossy(seg.width() - f64::from(tokens.spacing.s2)),
                chip_text_style,
            );
            let (tw, th) = text_renderer.measure_style(&text, None, chip_text_style);
            text_renderer.draw_style(
                scene,
                &text,
                ink,
                transform.pre_translate(
                    (
                        f64::midpoint(seg.x0, seg.x1) - f64::from(tw) / 2.0,
                        f64::midpoint(seg.y0, seg.y1) - f64::from(th) / 2.0,
                    )
                        .into(),
                ),
                None,
                chip_text_style,
            );
        }
    }
}
/// Narrowest a chip is ever squeezed to before its label is just an
/// ellipsis: the §9.6 pointer floor — a pill narrower than a finger is
/// not a target whatever it says. (Was a tuned 34 until 2026-09-17.)
const CHIP_MIN_W_PX: f64 = crate::tokens::Metrics::BASE.min_pointer_target as f64;
/// Height of a Selector at 100 % text (HIG §9.6, `metrics.selector_height`).
/// The unscaled token, for the constructor's resting value before any theme
/// is seen; the live band reads `theme.tokens.metrics.selector_height` in
/// [`SearchFilters::filter_control_height`] and grows with the measured
/// label from there.
pub const SELECTOR_HEIGHT_PX: f64 = crate::tokens::Metrics::BASE.selector_height as f64;

/// Share of the band's width the scope switch may take while it shares the
/// first row with the kind chips. Past it — 200 % text on a narrow card —
/// the switch takes the first row alone rather than eliding its labels
/// (S1-29); see [`SearchFilters::scope_switch_has_own_row`].
pub const SCOPE_TRACK_SHARE: f64 = 0.55;

/// Where each chip of the filter strip goes.
#[derive(Debug, Clone, PartialEq)]
pub struct ChipStrip {
    /// One rect per chip, in the order handed in. Every chip gets one —
    /// see [`lay_out_chip_strip`].
    pub rects: Vec<Rect>,
    /// Rows the chips want at their natural width; the host reserves
    /// this many from the palette.
    pub rows_needed: usize,
}

/// One row of filter chips, with kinds that did not fit sitting behind More.
#[derive(Debug, Clone, PartialEq)]
pub struct ChipRow {
    /// One rect per chip. Overflowed chips are [`Rect::ZERO`] until More opens.
    pub rects: Vec<Rect>,
    /// The More chip, present only when at least one kind did not fit.
    pub more: Option<Rect>,
    /// Indices into `rects` / the caller's labels that sit behind More.
    pub overflow: Vec<usize>,
}

/// Lays chips on a single row at their natural width. Leftovers stay
/// reachable through a More chip rather than wrapping to a second row.
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
    let all_w = widths.iter().copied().sum::<f64>()
        + gap * usize_to_f64_lossy(widths.len().saturating_sub(1));
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

/// Lays the filter chips out across a band that may be too narrow.
///
/// Invariant: every chip gets a rect — an unpainted chip could never be
/// clicked. Three behaviours in preference order: natural width while it
/// fits; wrap to a new row (over eliding — `Applications` → `Ap…` stops
/// saying what it filters); only once rows run out do leftovers share and
/// narrow the last row, a transient case the next layout fixes.
///
/// `first_row_x1` bounds the first row, since the scope switch anchors to
/// its right; later rows have the whole band. `gap` is the space between
/// two chips — the caller's live `spacing.s2`, passed in because this is a
/// pure geometry function without a theme in hand.
pub fn lay_out_chip_strip(
    natural: &[f64],
    band: Rect,
    first_row_x1: f64,
    rows_available: usize,
    row_h: f64,
    control_h: f64,
    gap: f64,
) -> ChipStrip {
    if natural.is_empty() || band.width() <= 0.0 {
        return ChipStrip {
            rects: Vec::new(),
            rows_needed: 1,
        };
    }
    let rows_available = rows_available.max(1);
    let row_x1 = |row: usize| if row == 0 { first_row_x1 } else { band.x1 };

    // Greedy wrap at natural width; `per_row` caps chips per row, used
    // by the balancing pass below.
    let wrap = |per_row: usize| -> Vec<Vec<usize>> {
        let mut plan: Vec<Vec<usize>> = vec![Vec::new()];
        let mut x = band.x0;
        for (i, w) in natural.iter().enumerate() {
            let w = w.max(CHIP_MIN_W_PX);
            // `plan` always starts with one row and is only ever pushed
            // to, never cleared, so `len() - 1` cannot underflow — spelled
            // with saturating_sub so the invariant doesn't need re-proving
            // at every call site.
            let row = plan.len().saturating_sub(1);
            let row_full = plan
                .get(row)
                .is_some_and(|r| !r.is_empty() && (x + w > row_x1(row) || r.len() >= per_row));
            if row_full {
                plan.push(Vec::new());
                x = band.x0;
            }
            let row = plan.len().saturating_sub(1);
            if let Some(r) = plan.get_mut(row) {
                r.push(i);
            }
            x += w + gap;
        }
        plan
    };
    let plan = wrap(usize::MAX);
    let rows_needed = plan.len();

    // Spread chips evenly over the rows used, rather than filling each
    // to the brim and leaving a lone chip on the last row. Only adopted
    // if it still fits in the same row count, so evenness never costs a
    // chip.
    let plan = if rows_needed > 1 {
        let per_row = natural.len().div_ceil(rows_needed);
        let balanced = wrap(per_row);
        if balanced.len() == rows_needed {
            balanced
        } else {
            plan
        }
    } else {
        plan
    };

    let mut rects = vec![Rect::ZERO; natural.len()];
    let place = |row: usize, ids: &[usize], widths: &[f64], rects: &mut Vec<Rect>| {
        let cy = band.y0 + usize_to_f64_lossy(row) * row_h + row_h / 2.0;
        let mut x = band.x0;
        for (slot, &id) in ids.iter().enumerate() {
            // `widths` is built alongside `ids` by every caller below, so
            // it always has one entry per id — `.get()` keeps a shape
            // mismatch from panicking instead of proving it can't happen.
            let Some(&w) = widths.get(slot) else {
                continue;
            };
            let Some(rect) = rects.get_mut(id) else {
                continue;
            };
            *rect = Rect::new(x, cy - control_h / 2.0, x + w, cy + control_h / 2.0);
            x += w + gap;
        }
    };

    let kept_rows = rows_needed.min(rows_available);
    for (row, ids) in plan.iter().take(kept_rows.saturating_sub(1)).enumerate() {
        let widths: Vec<f64> = ids
            .iter()
            .map(|&i| {
                natural
                    .get(i)
                    .copied()
                    .unwrap_or(CHIP_MIN_W_PX)
                    .max(CHIP_MIN_W_PX)
            })
            .collect();
        place(row, ids, &widths, &mut rects);
    }
    let last = kept_rows.saturating_sub(1);
    let leftover: Vec<usize> = plan
        .get(last..)
        .map(|rows| rows.iter().flatten().copied().collect())
        .unwrap_or_default();
    let widths = share_row(
        &leftover
            .iter()
            .map(|&i| natural.get(i).copied().unwrap_or(CHIP_MIN_W_PX))
            .collect::<Vec<_>>(),
        row_x1(last) - band.x0,
        gap,
    );
    place(last, &leftover, &widths, &mut rects);

    ChipStrip { rects, rows_needed }
}

/// Widths for one row of chips that must all fit inside `available`.
/// Chips narrower than an equal share keep their natural width and hand
/// the rest to the ones that are over, so squeezing costs the long
/// labels and leaves short ones alone.
pub fn share_row(natural: &[f64], available: f64, gap: f64) -> Vec<f64> {
    if natural.is_empty() {
        return Vec::new();
    }
    let n = usize_to_f64_lossy(natural.len());
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
        base + spare / usize_to_f64_lossy(over)
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

/// One pill in the search card's filter band, shared by the kind chips
/// and the "All" reset chip.
///
/// One filter chip: a Selector (HIG §9.1) whose body is its selection tier
/// (§9.3 — idle transparent, hover `lift(0.5)`, chosen `lift(1.0)` with a
/// hairline and inner glow) and whose label is ink chosen against whatever
/// that tier painted, so it stays legible on every preset. Until
/// 2026-09-17 the chosen chip wore `accent[5]` (§9.4 reserves accent for
/// live state) and the idle one a `lift(0.35)` wash of its own.
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
    let shape = RoundedRect::from_rect(rect, radius);
    let tier = crate::chrome::selection_tier(theme, state);
    if tier.fill.components[3] > 0.0 {
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tier.fill,
            None,
            &shape,
        );
    }
    if let Some(border) = tier.border {
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            border,
            None,
            &shape,
        );
    }
    if let Some(glow) = tier.inner_glow {
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            glow,
            None,
            &RoundedRect::from_rect(rect.inset(1.0), (radius - 1.0).max(0.0)),
        );
    }
    // The chosen chip's label is the control label (`text_primary`); the
    // others recede to `text_dim`. Either way the ink is measured against
    // the surface actually under it — the lifted body, or the panel ground
    // showing through an idle chip.
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
    // Control labels are medium weight (HIG §9.6); measured and drawn at
    // the same weight so the centring holds.
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
