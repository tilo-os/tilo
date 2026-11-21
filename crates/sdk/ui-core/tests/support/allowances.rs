//! The exemptions the design checks honour, as code.
//!
//! A hit the design audits cannot fix — a chart drawing its own content, a
//! constant that is a duration rather than a pixel length — is allowed for the
//! item that holds it: one [`Allowance`] names the file, the enclosing item and
//! the [`Rule`] with the reason it is not debt. The compiler checks the table's
//! shape, [`Items`] finds the item a hit sits in, and a change that moves or
//! removes an allowance makes the hit count again, which fails the ratchet in
//! the run that sees it. Editing a comment can never move an exemption.
//!
//! The same table holds the two inline exemptions the Python policy checks
//! read: a dimension compared to a literal that is not a breakpoint
//! ([`Rule::Breakpoint`]) and a socket bound by a path that is short by
//! construction ([`Rule::SocketPath`]). `tools/policy/rust_source.py` parses
//! this file for those, so keep each entry one `allowance(...)` call.

/// What an allowance lets through: one ratchet pattern, or one of the inline
/// policy checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rule {
    // The spacing ratchet.
    PixelConst,
    TaffyLength,
    LiteralGap,
    LiteralSpacer,
    // The typography ratchets.
    LiteralFontSize,
    LegacySizeApi,
    FontSizeConst,
    TypeStepArithmetic,
    RawTypeStep,
    RendererScaleHelper,
    RawOpenTypeFeature,
    // The theme and app-paint ratchets.
    LiteralColor,
    WhiteOrBlack,
    ThemeBlindWhite,
    LiteralColorNew,
    RampSlot,
    LegacyAlias,
    DesktopGround,
    LiteralRadius,
    LiteralAlpha,
    RawPaint,
    GlyphIcon,
    TransparentShot,
    DeclaredBorder,
    // The copy ratchet.
    AllCapsLabel,
    MiddleDotJoin,
    ToUppercase,
    // The inline policy checks.
    Breakpoint,
    SocketPath,
}

impl Rule {
    /// The key the rule's ratchet and its baseline use. The inline checks have
    /// no pattern; their key names the family.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::PixelConst => "pixel_const",
            Self::TaffyLength => "taffy_length",
            Self::LiteralGap => "literal_gap",
            Self::LiteralSpacer => "literal_spacer",
            Self::LiteralFontSize => "literal_font_size",
            Self::LegacySizeApi => "legacy_size_api",
            Self::FontSizeConst => "font_size_const",
            Self::TypeStepArithmetic => "type_step_arithmetic",
            Self::RawTypeStep => "raw_type_step",
            Self::RendererScaleHelper => "renderer_scale_helper",
            Self::RawOpenTypeFeature => "raw_opentype_feature",
            Self::LiteralColor => "literal_color",
            Self::WhiteOrBlack => "white_or_black",
            Self::ThemeBlindWhite => "theme_blind_white",
            Self::LiteralColorNew => "literal_color_new",
            Self::RampSlot => "ramp_slot",
            Self::LegacyAlias => "legacy_alias",
            Self::DesktopGround => "desktop_ground",
            Self::LiteralRadius => "literal_radius",
            Self::LiteralAlpha => "literal_alpha",
            Self::RawPaint => "raw_paint",
            Self::GlyphIcon => "glyph_icon",
            Self::TransparentShot => "transparent_shot",
            Self::DeclaredBorder => "declared_border",
            Self::AllCapsLabel => "all_caps_label",
            Self::MiddleDotJoin => "middle_dot_join",
            Self::ToUppercase => "to_uppercase",
            Self::Breakpoint => "breakpoint",
            Self::SocketPath => "socket_path",
        }
    }

    /// Whether a design ratchet counts this rule (rather than an inline check).
    #[must_use]
    pub const fn is_ratchet(self) -> bool {
        !matches!(self, Self::Breakpoint | Self::SocketPath)
    }
}

/// One reviewed exemption: a file, the item that holds the hit, the rule and
/// why the hit is content rather than debt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Allowance {
    /// Path relative to the repository root.
    pub file: &'static str,
    /// The innermost named item (`fn`, `const`, `impl`, …) the hit sits in.
    pub item: &'static str,
    pub rule: Rule,
    pub why: &'static str,
}

/// A table entry. A free function rather than a struct literal so the Python
/// checks read it with one shape, however rustfmt wraps the line.
#[must_use]
pub const fn allowance(
    file: &'static str,
    item: &'static str,
    rule: Rule,
    why: &'static str,
) -> Allowance {
    Allowance {
        file,
        item,
        rule,
        why,
    }
}

/// Every exemption the design checks honour.
pub const ALLOWANCES: &[Allowance] = &[
    // A chart, a card, a room or an audio meter is the person's content, so it
    // paints itself; no widget can draw it for the app.
    allowance(
        "apps/settings/src/panes/appearance/theme_tiles.rs",
        "paint_window",
        Rule::DesktopGround,
        "a theme tile is a miniature of that theme, drawn in its own colours",
    ),
    allowance(
        "apps/canvas/src/panels/start.rs",
        "sides",
        Rule::GlyphIcon,
        "the multiplication sign of a size is text, not an icon",
    ),
    allowance(
        "apps/settings/src/panes/display.rs",
        "resolution_text",
        Rule::GlyphIcon,
        "the multiplication sign of a resolution is text, not an icon",
    ),
    allowance(
        "apps/video-editor/src/agent_render.rs",
        "checked",
        Rule::GlyphIcon,
        "the multiplication sign of a size, text rather than an icon",
    ),
    allowance(
        "apps/poker/src/card.rs",
        "paint_back",
        Rule::LiteralAlpha,
        "the back's field is its owner's presence colour, washed out under its lattice",
    ),
    allowance(
        "apps/settings/src/panes/appearance/theme_tiles.rs",
        "paint_sidebar",
        Rule::LiteralAlpha,
        "a theme tile is a miniature of that theme, drawn in its own colours",
    ),
    allowance(
        "apps/settings/src/panes/appearance/theme_tiles.rs",
        "paint_tile_content",
        Rule::LiteralAlpha,
        "a theme tile is a miniature of that theme, drawn in its own colours",
    ),
    allowance(
        "apps/settings/src/panes/audio/level_meter.rs",
        "draw",
        Rule::LiteralAlpha,
        "the level meter draws live audio levels, content rather than chrome",
    ),
    allowance(
        "apps/settings/src/panes/power/history_chart.rs",
        "draw_charge",
        Rule::LiteralAlpha,
        "the charge chart draws measured battery data, content rather than chrome",
    ),
    allowance(
        "apps/terminal/src/main_body.rs",
        "draw_bell_flash",
        Rule::LiteralAlpha,
        "how strongly the grid's content is washed at the bell's peak",
    ),
    allowance(
        "apps/calculator/src/chrome.rs",
        "PANEL_MIN_WIDTH",
        Rule::PixelConst,
        "the width a formula row fits in, measured from its content, not spacing",
    ),
    allowance(
        "apps/canvas/src/chart.rs",
        "AXIS_WIDTH",
        Rule::PixelConst,
        "the chart is the person's content, in canvas units, not chrome",
    ),
    allowance(
        "apps/maps/src/canvas.rs",
        "DOCK_REACH",
        Rule::PixelConst,
        "a presence tag's reach round a pin's head, the picture of who is there, not spacing",
    ),
    allowance(
        "apps/settings/src/panes/speakers/room_canvas.rs",
        "VIEW_MARGIN_MM",
        Rule::PixelConst,
        "a distance in the room, in millimetres, not pixels",
    ),
    allowance(
        "apps/slides/src/app/clipboard.rs",
        "PASTE_OFFSET",
        Rule::PixelConst,
        "an offset on the slide, in slide units, not pixels",
    ),
    allowance(
        "apps/video-editor/src/agent_render.rs",
        "AGENT_MAX_SECONDS",
        Rule::PixelConst,
        "seconds of video, not a length on screen",
    ),
    allowance(
        "apps/video-editor/src/export/stepped.rs",
        "KEY_EVERY_S",
        Rule::PixelConst,
        "a span of seconds in the encoded file, not spacing",
    ),
    allowance(
        "apps/video-editor/src/gpu/seek_source.rs",
        "RUNNING_MAX_RATE",
        Rule::PixelConst,
        "a playback speed multiple, not spacing",
    ),
    allowance(
        "apps/video-player/src/preferences.rs",
        "DEFAULT_RATE",
        Rule::PixelConst,
        "a playback rate, not pixels",
    ),
    allowance(
        "crates/sdk/ui-core/src/widget/arrange/page.rs",
        "MEASURE_REM",
        Rule::PixelConst,
        "a width in rem, which the text size scales, not pixels",
    ),
    allowance(
        "crates/sdk/ui-core/src/widget/arrange/units.rs",
        "COMPACT_BELOW",
        Rule::PixelConst,
        "a class boundary in rem, which the text size scales, not pixels",
    ),
    allowance(
        "crates/sdk/ui-core/src/widget/arrange/units.rs",
        "EXPANDED_BELOW",
        Rule::PixelConst,
        "a class boundary in rem, which the text size scales, not pixels",
    ),
    allowance(
        "crates/sdk/ui-core/src/widget/arrange/units.rs",
        "MEDIUM_BELOW",
        Rule::PixelConst,
        "a class boundary in rem, which the text size scales, not pixels",
    ),
    allowance(
        "crates/sdk/ui-core/src/widget/popover.rs",
        "SHEET_WIDTH",
        Rule::PixelConst,
        "the window width a panel becomes a sheet under, read from the window's own rect every layout pass, not spacing",
    ),
    allowance(
        "crates/sdk/ui-core/src/widget/primitives/status_cluster.rs",
        "BADGE_PAST_CORNER",
        Rule::PixelConst,
        "an offset from a glyph's corner, a fraction of the glyph square rather than spacing",
    ),
    allowance(
        "crates/sdk/ui-core/src/widget/virtual_list_component.rs",
        "MESSAGE_ESTIMATE",
        Rule::PixelConst,
        "a height reckoned before layout measures it, not spacing",
    ),
    allowance(
        "crates/sdk/ui-core/src/widget/virtual_list_component.rs",
        "ROW_ESTIMATE",
        Rule::PixelConst,
        "a height reckoned before layout measures it, not spacing",
    ),
    allowance(
        "apps/poker/src/card.rs",
        "fill_suit",
        Rule::RawPaint,
        "a suit printed on a card's face, content",
    ),
    allowance(
        "apps/poker/src/card.rs",
        "paint_back",
        Rule::RawPaint,
        "the back of a playing card is the picture of a card, content",
    ),
    allowance(
        "apps/poker/src/card.rs",
        "paint_slot",
        Rule::RawPaint,
        "the outline of a card not dealt yet, drawn on the board as content",
    ),
    allowance(
        "apps/settings/src/panes/appearance/theme_tiles.rs",
        "draw",
        Rule::RawPaint,
        "a theme tile is a miniature of that theme, drawn in its own colours",
    ),
    allowance(
        "apps/settings/src/panes/appearance/theme_tiles.rs",
        "paint_sidebar",
        Rule::RawPaint,
        "a theme tile is a miniature of that theme, drawn in its own colours",
    ),
    allowance(
        "apps/settings/src/panes/appearance/theme_tiles.rs",
        "paint_tile_content",
        Rule::RawPaint,
        "a theme tile is a miniature of that theme, drawn in its own colours",
    ),
    allowance(
        "apps/settings/src/panes/appearance/theme_tiles.rs",
        "paint_window",
        Rule::RawPaint,
        "a theme tile is a miniature of that theme, drawn in its own colours",
    ),
    allowance(
        "apps/settings/src/panes/appearance/theme_tiles.rs",
        "tile_bar",
        Rule::RawPaint,
        "a theme tile is a miniature of that theme, drawn in its own colours",
    ),
    allowance(
        "apps/settings/src/panes/audio/level_meter.rs",
        "draw",
        Rule::RawPaint,
        "the level meter draws live audio levels, content rather than chrome",
    ),
    allowance(
        "apps/settings/src/panes/power/history_chart.rs",
        "draw_charge",
        Rule::RawPaint,
        "the charge chart draws measured battery data, content rather than chrome",
    ),
    allowance(
        "apps/settings/src/panes/power/history_chart.rs",
        "rule",
        Rule::RawPaint,
        "the charge chart draws measured battery data, content rather than chrome",
    ),
    allowance(
        "apps/settings/src/panes/speakers/room_canvas.rs",
        "draw",
        Rule::RawPaint,
        "the room canvas draws the room and its speakers, content rather than chrome",
    ),
    allowance(
        "apps/settings/src/panes/speakers/room_canvas.rs",
        "draw_room",
        Rule::RawPaint,
        "the room canvas draws the room and its speakers, content rather than chrome",
    ),
    allowance(
        "apps/settings/src/panes/speakers/room_canvas.rs",
        "draw_speakers",
        Rule::RawPaint,
        "the room canvas draws the room and its speakers, content rather than chrome",
    ),
    allowance(
        "apps/terminal/src/main_body.rs",
        "draw_bell_flash",
        Rule::RawPaint,
        "the bell washes the grid's content, which the terminal paints",
    ),
    allowance(
        "crates/sdk/ui-core/src/widget/picture.rs",
        "paint",
        Rule::WhiteOrBlack,
        "the colour and difference blends take pure white as their operand; no theme colour is one.",
    ),
    // A dimension compared to a copy of itself: content geometry, a protocol
    // bound, or a size re-read from the laid-out rect every pass. None decides
    // a layout once and keeps the answer.
    allowance(
        "apps/calculator/src/chrome.rs",
        "PANEL_MIN_WIDTH",
        Rule::Breakpoint,
        "the narrowest a formula row fits, measured from its content",
    ),
    allowance(
        "apps/calculator/src/model.rs",
        "for_width",
        Rule::Breakpoint,
        "re-chosen from the laid-out rect on every pass",
    ),
    allowance(
        "apps/chip-design/src/verification/operators.rs",
        "validate_softmax",
        Rule::Breakpoint,
        "a reported tensor's bound, not a layout",
    ),
    allowance(
        "apps/pdf-viewer/src/app.rs",
        "WIDTH",
        Rule::Breakpoint,
        "a PDF page's width in points, not a layout breakpoint",
    ),
    allowance(
        "crates/session/notifications-service/src/hints.rs",
        "parse_image_data",
        Rule::Breakpoint,
        "an icon's pixel bound, not a layout width",
    ),
    // A fixed path in the check's own directory, short by construction.
    allowance(
        "crates/dev/tilo-local/src/check.rs",
        "trap",
        Rule::SocketPath,
        "the check's own directory, kept short by construction",
    ),
    // The X11 display socket is `/tmp/.X11-unix/X<display>`, short by construction.
    allowance(
        "crates/session/x11-transfer/src/bridge.rs",
        "connect",
        Rule::SocketPath,
        "the X server's socket lives at a fixed short path under /tmp",
    ),
];

/// The line ranges (half-open, 0-based) of each named item in `source`, with
/// its name.
///
/// Comment and literal contents are masked first, so a brace inside a string, a
/// char literal or a raw string never moves an item's end, and a lifetime is
/// not mistaken for a char. A bodyless item (`const X: f64 = 3.0;`,
/// `mod tests;`) ends at its `;`.
#[must_use]
pub fn item_spans(source: &str) -> Vec<(usize, usize, String)> {
    let masked = mask(source);
    let lines: Vec<&str> = masked.lines().collect();
    let mut opened: Vec<(usize, String, Option<i32>)> = Vec::new();
    let mut spans = Vec::new();
    let mut depth = 0i32;
    for (index, line) in lines.iter().enumerate() {
        let before = depth;
        let awaiting = opened.iter().any(|(_, _, open)| open.is_none());
        if !awaiting && let Some(name) = item_start(line) {
            opened.push((index, name, None));
        }
        let pending: Vec<usize> = opened
            .iter()
            .enumerate()
            .filter(|(_, (_, _, open))| open.is_none())
            .map(|(at, _)| at)
            .collect();
        let decisive = line.char_indices().find(|(_, c)| *c == '{' || *c == ';');
        match decisive {
            Some((_, '{')) => {
                for at in pending {
                    opened[at].2 = Some(before);
                }
            }
            Some((_, _)) => {
                if let Some(&at) = pending.last() {
                    let (start, name, _) = opened.remove(at);
                    spans.push((start, index.saturating_add(1), name));
                }
            }
            None => {}
        }
        depth = depth.saturating_add(i32::try_from(line.matches('{').count()).unwrap_or(0));
        depth = depth.saturating_sub(i32::try_from(line.matches('}').count()).unwrap_or(0));
        loop {
            let closes = opened
                .iter()
                .enumerate()
                .rev()
                .find(|(_, (_, _, open))| open.is_some_and(|open| depth <= open))
                .map(|(at, _)| at);
            let Some(at) = closes else { break };
            let (start, name, _) = opened.remove(at);
            spans.push((start, index.saturating_add(1), name));
        }
    }
    for (start, name, _) in opened {
        spans.push((start, lines.len(), name));
    }
    spans
}

/// Every named item in `source`, ready to be asked which one holds a line.
pub struct Items {
    spans: Vec<(usize, usize, String)>,
    /// Per line, the span that holds it (`None` at file level).
    line_span: Vec<Option<usize>>,
}

impl Items {
    #[must_use]
    pub fn of(source: &str) -> Self {
        let spans = item_spans(source);
        let mut line_span = vec![None; source.lines().count()];
        // Later starts are the inner items: assigning in start order leaves the
        // innermost name on a line a nested item also covers.
        let mut order: Vec<usize> = (0..spans.len()).collect();
        order.sort_by_key(|at| spans[*at].0);
        for at in order {
            let (start, end, _) = &spans[at];
            for line in line_span.iter_mut().take(*end).skip(*start) {
                *line = Some(at);
            }
        }
        Self { spans, line_span }
    }

    /// The innermost item a 0-based `line` sits in, or `""`.
    #[must_use]
    pub fn at(&self, line: usize) -> &str {
        self.line_span
            .get(line)
            .copied()
            .flatten()
            .map_or("", |at| self.spans[at].2.as_str())
    }
}

/// The name an item declaration at `line` opens, when the line starts one.
fn item_start(line: &str) -> Option<String> {
    let mut rest = line.trim_start();
    // Visibility, then the modifiers that may precede the item keyword.
    for prefix in ["pub(crate)", "pub(super)", "pub(self)", "pub"] {
        if let Some(after) = rest.strip_prefix(prefix) {
            if after.starts_with(' ') {
                rest = after.trim_start();
            }
            break;
        }
    }
    loop {
        let mut stripped = false;
        for modifier in ["default ", "async ", "unsafe ", "extern \"C\" "] {
            if let Some(after) = rest.strip_prefix(modifier) {
                rest = after;
                stripped = true;
                break;
            }
        }
        if !stripped {
            break;
        }
    }
    // `const fn` is a function, not a constant.
    if let Some(after) = rest.strip_prefix("const fn")
        && (after.is_empty() || after.starts_with([' ', '\t']))
    {
        let name: String = after
            .trim_start()
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        return (!name.is_empty()).then_some(name);
    }
    let (kind, after) = keyword(rest)?;
    if kind == "impl" {
        return Some(impl_name(after));
    }
    let name: String = after
        .trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// The item keyword `rest` starts with, and what follows it.
fn keyword(rest: &str) -> Option<(&'static str, &str)> {
    for kind in [
        "fn", "const", "static", "struct", "enum", "union", "trait", "mod", "type", "impl",
    ] {
        if let Some(after) = rest.strip_prefix(kind)
            && (after.is_empty() || after.starts_with([' ', '(', '<', '{']))
        {
            return Some((kind, after));
        }
    }
    None
}

/// The type an `impl` block is for: after `for` when it names a trait, else the
/// first path.
fn impl_name(after: &str) -> String {
    let header = after
        .split(['{', ';'])
        .next()
        .unwrap_or(after)
        .trim()
        .to_owned();
    let path = header
        .find(" for ")
        .map_or(header.as_str(), |at| &header[at.saturating_add(5)..]);
    let path = path.trim_start_matches('<');
    path.chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':')
        .collect::<String>()
        .rsplit("::")
        .next()
        .unwrap_or("")
        .to_owned()
}

/// `source` with comment and literal contents blanked, newlines kept, so a
/// brace or semicolon in either never counts.
fn mask(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        if c == '/' && chars.get(at.saturating_add(1)) == Some(&'/') {
            while at < chars.len() && chars[at] != '\n' {
                out.push(' ');
                at = at.saturating_add(1);
            }
            continue;
        }
        if c == '/' && chars.get(at.saturating_add(1)) == Some(&'*') {
            let mut depth = 1i32;
            push_blank(&mut out, &chars, at, at.saturating_add(2));
            at = at.saturating_add(2);
            while at < chars.len() && depth > 0 {
                if chars[at] == '/' && chars.get(at.saturating_add(1)) == Some(&'*') {
                    depth = depth.saturating_add(1);
                    push_blank(&mut out, &chars, at, at.saturating_add(2));
                    at = at.saturating_add(2);
                } else if chars[at] == '*' && chars.get(at.saturating_add(1)) == Some(&'/') {
                    depth = depth.saturating_sub(1);
                    push_blank(&mut out, &chars, at, at.saturating_add(2));
                    at = at.saturating_add(2);
                } else {
                    push_blank(&mut out, &chars, at, at.saturating_add(1));
                    at = at.saturating_add(1);
                }
            }
            continue;
        }
        if (c == 'r' || c == 'b')
            && let Some((body, hashes)) = raw_string(&chars, at)
        {
            let mut end = body;
            while end < chars.len() {
                if chars[end] == '"'
                    && (0..hashes)
                        .all(|k| chars.get(end.saturating_add(1).saturating_add(k)) == Some(&'#'))
                {
                    end = end.saturating_add(1).saturating_add(hashes);
                    break;
                }
                end = end.saturating_add(1);
            }
            push_blank(&mut out, &chars, at, end);
            at = end;
            continue;
        }
        if c == 'b' && chars.get(at.saturating_add(1)) == Some(&'"') {
            push_blank(&mut out, &chars, at, at.saturating_add(1));
            at = at.saturating_add(1);
            continue;
        }
        if c == '"' {
            let mut end = at.saturating_add(1);
            while end < chars.len() {
                if chars[end] == '\\' {
                    end = end.saturating_add(2);
                    continue;
                }
                if chars[end] == '"' {
                    end = end.saturating_add(1);
                    break;
                }
                end = end.saturating_add(1);
            }
            push_blank(&mut out, &chars, at, end);
            at = end;
            continue;
        }
        if c == '\'' {
            let close = if chars.get(at.saturating_add(1)) == Some(&'\\') {
                (at.saturating_add(2)..chars.len().min(at.saturating_add(12)))
                    .find(|&k| chars[k] == '\'')
            } else if chars.get(at.saturating_add(2)) == Some(&'\'') {
                Some(at.saturating_add(2))
            } else {
                None
            };
            if let Some(close) = close {
                push_blank(&mut out, &chars, at, close.saturating_add(1));
                at = close.saturating_add(1);
                continue;
            }
        }
        out.push(c);
        at = at.saturating_add(1);
    }
    out
}

/// The body start and hash count of a raw string opening at `at`, if one does.
fn raw_string(chars: &[char], at: usize) -> Option<(usize, usize)> {
    let mut scan = at;
    if chars[scan] == 'b' {
        scan = scan.saturating_add(1);
        if chars.get(scan) != Some(&'r') {
            return None;
        }
    } else if chars.get(scan) != Some(&'r') {
        return None;
    }
    scan = scan.saturating_add(1);
    let mut hashes: usize = 0;
    while chars.get(scan) == Some(&'#') {
        hashes = hashes.saturating_add(1);
        scan = scan.saturating_add(1);
    }
    (chars.get(scan) == Some(&'"')).then_some((scan.saturating_add(1), hashes))
}

fn push_blank(out: &mut String, chars: &[char], from: usize, to: usize) {
    for c in chars.iter().take(to).skip(from) {
        out.push(if *c == '\n' { '\n' } else { ' ' });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item_at(source: &str, needle: &str) -> String {
        let line = source
            .lines()
            .position(|line| line.contains(needle))
            .expect("the needle is in the source");
        Items::of(source).at(line).to_owned()
    }

    #[test]
    fn an_item_holds_the_lines_of_its_body() {
        let source = "const TOP: f64 = 3.0;\n\nfn outer() {\n    let f = || {\n        scene.fill(a);\n    };\n    scene.fill(b);\n}\n\nimpl Widget for Probe {\n    fn draw(&mut self) {\n        scene.fill(c);\n    }\n}\n";
        assert_eq!(item_at(source, "TOP"), "TOP");
        assert_eq!(item_at(source, "scene.fill(a)"), "outer");
        assert_eq!(item_at(source, "scene.fill(b)"), "outer");
        assert_eq!(item_at(source, "scene.fill(c)"), "draw");
    }

    #[test]
    fn a_brace_in_a_literal_or_comment_does_not_move_an_item_end() {
        let source = "fn draw() {\n    let s = \"}\";\n    let c = '}';\n    // }\n    let r = r#\"} }\"#;\n    let life: &'static str = \"x\";\n    scene.fill(a);\n}\nconst AFTER: f64 = 4.0;\n";
        assert_eq!(item_at(source, "scene.fill(a)"), "draw");
        assert_eq!(item_at(source, "AFTER"), "AFTER");
    }

    #[test]
    fn a_multi_line_signature_and_a_bodyless_item_keep_their_own_lines() {
        let source = "pub fn looks(\n    width: f64,\n    height: f64,\n) -> bool {\n    width < 460.0\n}\n#[cfg(test)]\npub const MAX: usize = 120;\nconst AFTER: f64 = 8.0;\n";
        assert_eq!(item_at(source, "width < 460.0"), "looks");
        assert_eq!(item_at(source, "MAX"), "MAX");
        assert_eq!(item_at(source, "AFTER"), "AFTER");
    }

    #[test]
    fn every_allowance_names_a_rule_once_per_item() {
        let mut seen = std::collections::BTreeSet::new();
        for entry in ALLOWANCES {
            assert!(
                seen.insert((entry.file, entry.item, entry.rule)),
                "{} {} {:?} is listed twice",
                entry.file,
                entry.item,
                entry.rule
            );
            assert!(
                entry.why.trim().len() >= 12,
                "{} {} has no reason a reviewer can check",
                entry.file,
                entry.item
            );
        }
    }

    #[test]
    fn every_allowance_names_an_item_its_file_has() {
        let repository = super::super::ratchet::repository_root();
        for entry in ALLOWANCES {
            let path = repository.join(entry.file);
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("{} is not readable: {error}", path.display()));
            let items = Items::of(&source);
            assert!(
                items.spans.iter().any(|(_, _, name)| name == entry.item),
                "{} has no item `{}`",
                entry.file,
                entry.item
            );
        }
    }

    #[test]
    fn every_ratchet_allowance_excuses_a_hit_that_is_there() {
        let repository = super::super::ratchet::repository_root();
        let ratchets = [
            crate::design_token_audit::theme_ratchet(),
            crate::design_token_audit::app_paint_ratchet(),
            crate::design_token_audit::chrome_border_ratchet(),
            crate::spacing_token_audit::spacing_ratchet(),
            crate::typography_token_audit::typography_ratchet(),
            crate::typography_token_audit::opentype_ratchet(),
            crate::copy_audit::copy_ratchet(),
        ];
        let mut used = std::collections::BTreeSet::new();
        for ratchet in &ratchets {
            let (files, unreached) = ratchet.files(&repository);
            assert!(unreached.is_empty(), "{}: {unreached:?}", ratchet.name);
            used.extend(ratchet.count_with(&repository, &files, ALLOWANCES).1);
        }
        let stale: Vec<String> = ALLOWANCES
            .iter()
            .enumerate()
            .filter(|(at, entry)| entry.rule.is_ratchet() && !used.contains(at))
            .map(|(_, entry)| format!("{} {} {:?}", entry.file, entry.item, entry.rule))
            .collect();
        assert!(
            stale.is_empty(),
            "these ratchet allowances excuse no hit any more: {stale:?}"
        );
    }
}
