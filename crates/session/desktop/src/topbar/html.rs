//! The bar as a page shows it: each part where [`TopBarChrome::lay_out_bar`] stood it, as what
//! it is. A control with words or one glyph is a real button with its face (the workspace's name,
//! the strip, the invite, the people and the media chips); the time is words; what is genuinely
//! drawn (the Tilo mark, faces with their rings, the status cluster's gauges, an application's
//! icon, the notices) is a picture of its own paint. Every control is a button a reader and the
//! keyboard reach, where its target stands.

use super::{Rect, TextRole, Theme, TopBarChrome, TopBarPresence};
use ui::composition::{HtmlCx, Own, TextLook};

/// The code that composes the bar for a page.
pub type TopBarHtml = fn(&TopBarChrome, &mut HtmlCx<'_>);

/// The bar's HTML rendering this build links; only the HTML presenter's code names it.
#[must_use]
pub fn builtin_topbar_html() -> TopBarHtml {
    TopBarChrome::compose_html
}

ui::lazy_code! {
    /// The bar's HTML rendering: linked, or with `lazy-topbar-html` handed over with the HTML
    /// presenter, the only code that composes. Every widget's `html` stands in its vtable and so
    /// in the start module of a page that never composes.
    pub static TOPBAR_HTML: TopBarHtml = builtin_topbar_html, feature = "lazy-topbar-html";
}

impl TopBarChrome {
    fn compose_html(&self, cx: &mut HtmlCx<'_>) {
        cx.element("header");
        if self.presence() == TopBarPresence::Hidden
            || self.rect.width() <= 0.0
            || self.rect.height() <= 0.0
        {
            return;
        }
        let theme = cx.theme().clone();
        let ground = theme.tokens.colors.surfaces.base;
        if self.presence() == TopBarPresence::Shown || theme.tokens.finishes.bar.is_some() {
            cx.surface(self.rect, "ground");
        }
        let controls: Vec<ui::a11y::AccessNode> = cx
            .semantics()
            .map(|node| node.children.clone())
            .unwrap_or_default();
        let mut parts = Parts {
            controls,
            covered: Vec::new(),
            version: self.paint_generation,
        };
        let mut part = |cx: &mut HtmlCx<'_>, key: &str, rect: Rect, control: Option<&str>| {
            parts.add(cx, key, rect, control);
        };

        // The leading side: the mark, the workspace's name, the strip.
        if !self.tilo_mark_rect.is_zero_area() {
            part(
                cx,
                "mark",
                self.tilo_mark_rect,
                Some(Self::CONTROL_TILO_MARK),
            );
        }
        if !self.ws_pill_rect.is_zero_area() {
            part(
                cx,
                "workspace",
                self.ws_pill_rect,
                Some(Self::CONTROL_WORKSPACE_SETTINGS),
            );
            if !self.renaming_workspace() {
                let name = self.control_name(Self::CONTROL_WORKSPACE_SETTINGS);
                let face = self.workspace_face(&name, &self.placed.workspace_label);
                face_of(cx, ui::composition::bar_control_face(&face, &theme, ground));
            }
        }
        if let Some((name, label_width)) = self.placed.strip.as_ref() {
            part(
                cx,
                "strip",
                self.ws_strip_rect,
                Some(Self::CONTROL_STRIP_NAME),
            );
            let chip = self.strip_chip(name, *label_width);
            face_of(cx, ui::composition::bar_chip_face(&chip, &theme, ground));
        }

        // The middle: the time, as words, unless a live activity's pill stands there.
        if !self.center_rect.is_zero_area() {
            part(cx, "centre", self.center_rect, None);
            self.compose_time(cx, &theme);
        }

        if !self.is_compact() {
            self.compose_session_side(cx, &theme, &mut part);
        }

        parts.compose_targets(cx);
    }

    /// The time where the centre stands, as words; a live activity's pill keeps its paint.
    fn compose_time(&self, cx: &mut HtmlCx<'_>, theme: &Theme) {
        if self.activity_is_visible() || !self.center_pill().is_empty() {
            return;
        }
        let style = theme
            .tokens
            .typography
            .style(TextRole::Body)
            .with_weight(self.clock_text_weight());
        let mut look = TextLook::new(
            self.current_clock_string(),
            &style,
            Some(theme.tokens.colors.text_primary),
        )
        .with_max_lines(Some(1));
        look.align = "center";
        if let Some(last) = cx.last_part() {
            last.own = Some(Own::Text(look));
            last.semantics = Some(
                ui::a11y::AccessNode::new(ui::a11y::Role::StaticText)
                    .with_name(self.current_clock_string())
                    .with_description(self.date_description()),
            );
        }
    }

    /// The full bar's right side, each part where it stands.
    fn compose_session_side(
        &self,
        cx: &mut HtmlCx<'_>,
        theme: &Theme,
        part: &mut impl FnMut(&mut HtmlCx<'_>, &str, Rect, Option<&str>),
    ) {
        let ground = theme.tokens.colors.surfaces.base;
        // The status cluster's gauges are drawn: one picture, its two targets over it.
        part(cx, "tray", self.tray_pill_rect, None);
        if !self.own_avatar_rect.is_zero_area() {
            part(
                cx,
                "own-face",
                self.own_avatar_rect,
                Some(Self::CONTROL_PROFILE),
            );
        }
        for slot in &self.app_tray {
            let key = Self::tray_target(&slot.key);
            part(cx, &key, slot.rect, Some(&key));
        }
        if let Some(media) = self.placed.media.as_ref() {
            part(
                cx,
                "media",
                self.media_hit_rect(),
                Some(Self::CONTROL_MEDIA),
            );
            let chip = super::placed::MediaPlaced::chip_of(
                &media.name,
                &media.glyphs,
                &media.label,
                media.tone,
            );
            face_of(cx, ui::composition::bar_chip_face(&chip, theme, ground));
        }
        if let Some((label, available)) = self.placed.sharing.as_ref() {
            part(cx, "sharing", self.sharing_indicator_rect, None);
            let chip = self.sharing_chip(theme, label, *available);
            face_of(cx, ui::composition::bar_chip_face(&chip, theme, ground));
        }
        if !self.invite_rect.is_zero_area() {
            part(cx, "invite", self.invite_rect, Some(Self::CONTROL_INVITE));
            let name = super::t("topbar-invite");
            face_of(
                cx,
                ui::composition::bar_chip_face(&self.invite_chip(&name), theme, ground),
            );
        }
        if let Some((label, pill)) = self.placed.watching.as_ref() {
            part(cx, "watching", *pill, None);
            let chip = self.watching_chip(label, pill.x1);
            face_of(cx, ui::composition::bar_chip_face(&chip, theme, ground));
        }
        if !self.avatar_stack_rect.is_zero_area() {
            part(cx, "faces", self.avatar_stack_rect, None);
        }
        if let Some(hovered) = self.placed.people_button {
            part(cx, "people", self.people_button_rect, None);
            let name = super::t("topbar-show-collaborators");
            face_of(
                cx,
                ui::composition::bar_chip_face(&self.people_chip(&name, hovered), theme, ground),
            );
        }
        if self.placed.people_count.is_some() {
            part(cx, "people-count", self.people_count_rect, None);
        }
        if let Some(arrival) = self.placed.arrival.as_ref() {
            part(cx, "arrival", arrival.rect, None);
        }
        if let Some((_, rect)) = self.placed.wave.as_ref() {
            part(cx, "wave", *rect, None);
        }
        if self.placed.continuity.is_some() {
            part(
                cx,
                "continuity",
                self.continuity_rect,
                Some(Self::CONTROL_CONTINUITY),
            );
            part(
                cx,
                "continuity-dismiss",
                self.continuity_dismiss_rect,
                Some(Self::CONTROL_CONTINUITY_DISMISS),
            );
        }
        if !self.ask.pill.is_zero_area() {
            part(cx, "ask", self.ask.pill, None);
        }
    }
}

/// The bar's parts as they are composed, and the controls they stand for.
struct Parts {
    /// The bar's controls, as it tells a reader of them.
    controls: Vec<ui::a11y::AccessNode>,
    /// The controls a part stands for.
    covered: Vec<String>,
    /// What the parts' pictures are versioned by.
    version: u64,
}

impl Parts {
    /// The part `key` at `rect`, a picture of the bar's paint until it says otherwise, and the
    /// button for `control` when it stands for one.
    fn add(&mut self, cx: &mut HtmlCx<'_>, key: &str, rect: Rect, control: Option<&str>) {
        let semantics = control.and_then(|control| {
            self.covered.push(control.to_owned());
            self.controls
                .iter()
                .find(|node| node.action_target.as_deref() == Some(control))
                .cloned()
        });
        let part = cx.part(key, "TopBarPart", rect);
        if let Some(semantics) = semantics {
            part.tag = Some("button");
            part.focusable = true;
            part.semantics = Some(semantics);
        }
        part.painted(self.version);
    }

    /// Every control no part stood for still answers where its target is: the bell and the
    /// quick settings inside the status cluster's one picture, who is here across the faces,
    /// the ask's two choices.
    fn compose_targets(&self, cx: &mut HtmlCx<'_>) {
        for node in &self.controls {
            let Some(key) = node.action_target.as_deref() else {
                continue;
            };
            let Some(rect) = node.bounds.map(ui::a11y::AccessBounds::to_rect) else {
                continue;
            };
            if self.covered.iter().any(|done| done == key) || rect.is_zero_area() {
                continue;
            }
            let control = cx.part(&format!("target-{key}"), "TopBarTarget", rect);
            control.tag = Some("button");
            control.focusable = true;
            control.semantics = Some(node.clone());
        }
    }
}

/// The part composed last shows `face` rather than its paint; it keeps its paint where the face
/// is a picture.
fn face_of(cx: &mut HtmlCx<'_>, face: Option<ui::composition::Face>) {
    if let (Some(face), Some(last)) = (face, cx.last_part()) {
        last.own = Some(Own::Face(face));
    }
}
