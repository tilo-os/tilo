//! Exact-origin site protection and information, owned by browser chrome.
use std::collections::BTreeSet;
use std::path::Path;

pub(crate) fn origin(input: &str) -> Option<String> {
    if input.len() > 8192 {
        return None;
    }
    let url = url::Url::parse(input).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.origin().ascii_serialization())
}
#[derive(Default, Debug, PartialEq)]
pub(crate) struct Exceptions(BTreeSet<String>);
impl Exceptions {
    fn set(&mut self, url: &str, disabled: bool) {
        if let Some(origin) = origin(url) {
            if disabled {
                if self.0.len() < 1024 {
                    self.0.insert(origin);
                }
            } else {
                self.0.remove(&origin);
            }
        }
    }
    #[cfg(test)]
    fn contains(&self, url: &str) -> bool {
        origin(url).is_some_and(|o| self.0.contains(&o))
    }
    fn save(&self, path: &Path) -> std::io::Result<()> {
        tilo_files::native::atomic::write_creating_parent(path, &serde_json::to_vec(&self.0)?)
    }
    fn load(path: &Path) -> Self {
        if !std::fs::metadata(path).is_ok_and(|m| m.len() <= 1024 * 1024) {
            return Self::default();
        }
        let values: Vec<String> = std::fs::read(path)
            .ok()
            .and_then(|v| serde_json::from_slice(&v).ok())
            .unwrap_or_default();
        let mut rules = Self::default();
        for value in values.into_iter().take(1024) {
            rules.set(&value, true);
        }
        rules
    }
}
use ui::time::{Duration, Instant};
use ui::{
    LocalState,
    icons::Icon,
    text_render::TextKind,
    tokens::TextRole,
    webview::{SharedEngine, SiteStatus, WebViewCommand, WebViewEvent},
    widget::{
        Button, ButtonRole, Column, Container, Property, PropertyList, Row, ScrollAxis, Scrollable,
        Spacer, Text, ToggleGroup, Widget, toggle_group::Segment,
    },
};
#[derive(Clone, Debug)]
enum Pending {
    Open(bool),
    Close,
    Action {
        origin: String,
        generation: u64,
        kind: String,
    },
    Refresh,
}
pub(crate) struct SiteControls {
    pub open: bool,
    information: bool,
    committed: Option<String>,
    status: Option<SiteStatus>,
    pending: LocalState<Option<Pending>>,
    sequence: u32,
    query: Option<(u32, Instant)>,
    action: Option<(u32, String, u64, String, Instant)>,
    pending_reload: Option<(String, u64)>,
    next_poll: Instant,
    notice: String,
    filters: crate::native_blocker::Status,
    persistent: bool,
    runtime_exceptions: std::sync::Arc<std::sync::Mutex<Exceptions>>,
}
impl Default for SiteControls {
    fn default() -> Self {
        Self {
            open: false,
            information: false,
            committed: None,
            status: None,
            pending: LocalState::new(None),
            sequence: 0,
            query: None,
            action: None,
            pending_reload: None,
            next_poll: ui::time::now(),
            notice: String::new(),
            filters: crate::native_blocker::status(),
            persistent: false,
            runtime_exceptions: std::sync::Arc::default(),
        }
    }
}
impl SiteControls {
    pub fn share_runtime_state(&mut self, other: &Self) {
        self.runtime_exceptions = other.runtime_exceptions.clone();
    }

    pub fn initialize(&mut self, engine: &SharedEngine, persistent: bool) {
        self.persistent = persistent;
        if engine.supports_native_filtering() {
            if persistent {
                let exceptions = crate::storage::config_dir()
                    .map(|p| Exceptions::load(&p.join("site-exceptions.json")))
                    .unwrap_or_default();
                if let Ok(mut runtime) = self.runtime_exceptions.lock() {
                    *runtime = exceptions;
                }
            }
            let exceptions = self
                .runtime_exceptions
                .lock()
                .map(|rules| rules.0.iter().cloned().collect())
                .unwrap_or_default();
            engine.send(WebViewCommand::SetSiteExceptions(exceptions));
        }
    }
    pub fn navigation(&mut self, url: &str) {
        self.committed = origin(url);
        self.open = false;
        self.status = None;
        self.query = None;
        self.action = None;
        self.pending_reload = None;
        self.pending.set(None);
        self.notice.clear();
        self.next_poll = ui::time::now();
    }
    pub fn close(&mut self) {
        self.open = false;
        self.query = None;
        self.pending.set(None);
    }
    fn id(&mut self) -> u32 {
        self.sequence = self.sequence % (i32::MAX as u32) + 1;
        self.sequence
    }
    pub fn apply(&mut self, event: WebViewEvent) -> bool {
        match event {
            WebViewEvent::SiteStatus(status)
                if self.open
                    && self.committed.as_deref() == Some(status.origin.as_str())
                    && self.query.is_some_and(|(id, _)| id == status.request_id) =>
            {
                self.query = None;
                let mut comparable = status.clone();
                if let Some(old) = &self.status {
                    comparable.request_id = old.request_id;
                }
                let changed = self.status.as_ref() != Some(&comparable);
                self.status = Some(status);
                changed
            }
            WebViewEvent::SiteResult {
                request_id,
                origin,
                generation,
                action,
                success,
                message,
            } if self.committed.as_deref() == Some(origin.as_str()) => {
                if action == "status" && self.query.is_some_and(|(id, _)| id == request_id) {
                    self.query = None;
                    self.notice = message;
                    self.status = None;
                    return true;
                }
                let Some((id, expected, document, kind, _)) = &self.action else {
                    return false;
                };
                if *id != request_id
                    || *expected != origin
                    || *document != generation
                    || *kind != action
                {
                    return false;
                }
                self.action = None;
                if action != "reload" || !success {
                    self.notice = message;
                }
                if success && matches!(action.as_str(), "enable" | "disable") {
                    if let Ok(mut exceptions) = self.runtime_exceptions.lock() {
                        exceptions.set(&origin, action == "disable");
                        if self.persistent
                            && let Some(path) =
                                crate::storage::config_dir().map(|p| p.join("site-exceptions.json"))
                            && exceptions.save(&path).is_err()
                        {
                            self.notice =
                                "Changed for this session; could not save the preference".into();
                        }
                    }
                    // This belongs to the accepted action, not the popover's
                    // replaceable button-intent slot. Dismissal must not lose it.
                    self.pending_reload = Some((origin, generation));
                }
                self.next_poll = ui::time::now();
                true
            }
            _ => false,
        }
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.open.then_some(self.next_poll)
    }
    fn start_action(
        &mut self,
        engine: &SharedEngine,
        origin: String,
        generation: u64,
        kind: String,
    ) -> bool {
        if self.action.is_some()
            || self.committed.as_deref() != Some(origin.as_str())
            || self
                .status
                .as_ref()
                .is_none_or(|status| status.generation != generation)
        {
            return false;
        }
        let request_id = self.id();
        engine.send(WebViewCommand::SiteAction {
            request_id,
            origin: origin.clone(),
            generation,
            action: kind.clone(),
        });
        if kind != "reload" {
            self.notice = "Working…".into();
        }
        self.action = Some((request_id, origin, generation, kind, ui::time::now()));
        true
    }
    pub fn sync(&mut self, engine: &SharedEngine) -> bool {
        let mut dirty = false;
        if self.action.is_none()
            && let Some((origin, generation)) = self.pending_reload.take()
        {
            dirty |= self.start_action(engine, origin, generation, "reload".into());
        }
        if let Some(pending) = self.pending.get() {
            self.pending.set(None);
            dirty = true;
            match pending {
                Pending::Open(information) => {
                    self.open = true;
                    self.information = information;
                    self.next_poll = ui::time::now();
                    self.notice.clear();
                }
                Pending::Close => self.close(),
                Pending::Refresh => crate::native_blocker::refresh(engine, true),
                Pending::Action {
                    origin,
                    generation,
                    kind,
                } => {
                    self.start_action(engine, origin, generation, kind);
                }
            }
        }
        let filters = crate::native_blocker::status();
        if self.filters != filters {
            self.filters = filters;
            dirty |= self.open;
        }
        let now = ui::time::now();
        if self.open && now >= self.next_poll {
            self.next_poll = now + Duration::from_secs(1);
            if self
                .query
                .is_some_and(|(_, sent)| sent.elapsed() > Duration::from_secs(5))
            {
                self.query = None;
                self.notice = "Site information timed out; retrying…".into();
                dirty = true;
            }
            if self
                .action
                .as_ref()
                .is_some_and(|(_, _, _, _, sent)| sent.elapsed() > Duration::from_secs(20))
            {
                self.action = None;
                self.notice = "Action timed out; refresh the page before retrying".into();
                dirty = true;
            }
            if self.query.is_none()
                && let Some(origin) = self.committed.clone()
            {
                let request_id = self.id();
                self.query = Some((request_id, now));
                engine.send(WebViewCommand::RequestSiteStatus { request_id, origin });
            }
        }
        dirty
    }
    fn button(&self, id: &str, label: &str, pending: Pending) -> Button {
        Button::new(id, label, None).on_effect(self.command(pending))
    }
    fn command(&self, pending: Pending) -> impl FnMut() + 'static {
        let state = self.pending.clone();
        move || state.set(Some(pending.clone()))
    }
    /// The closure that opens the panel on its information (`true`) or its
    /// protection (`false`) page.
    pub fn open_command(&self, information: bool) -> impl FnMut() + 'static {
        self.command(Pending::Open(information))
    }
    pub fn launcher(&self) -> Button {
        Button::icon_only("browser.site.info", Icon::Info, "Site information")
            .tooltip("Connection, permissions and stored data")
            .on_effect(self.open_command(true))
    }
    /// The panel's own page switcher: protection, or the connection and the
    /// data the site holds.
    fn pages(&self) -> ToggleGroup {
        let pending = self.pending.clone();
        ToggleGroup::new(
            vec![
                Segment::new("Protection"),
                Segment::new("Connection and data"),
            ],
            usize::from(self.information),
        )
        .on_view("browser.site.pages", move |index| {
            pending.set(Some(Pending::Open(index == 1)));
        })
    }
    #[allow(
        clippy::too_many_lines,
        reason = "the two bounded site-panel pages share their action context"
    )]
    pub fn panel(&self, width: f32, height: f32) -> Box<dyn Widget> {
        let line =
            |text: &str| Box::new(Text::new(text).with_role(TextRole::Body)) as Box<dyn Widget>;
        let note = |text: &str| {
            Box::new(Text::new(text).with_role(TextRole::Caption).dimmed()) as Box<dyn Widget>
        };
        let facts = |rows: Vec<Property>| Box::new(PropertyList::new(rows)) as Box<dyn Widget>;
        let title = if self.information {
            "Site information"
        } else {
            "Ads and trackers"
        };
        let mut column = Column::new()
            .with_intrinsic_height()
            .push(Box::new(
                Row::new()
                    .no_wrap()
                    .push(Box::new(Text::new(title).with_role(TextRole::Title)))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(self.button(
                        "browser.site.close",
                        "Done",
                        Pending::Close,
                    ))),
            ))
            .push(Box::new(
                Text::new(
                    self.committed
                        .as_deref()
                        .unwrap_or("No HTTP or HTTPS origin"),
                )
                .with_role(TextRole::Caption)
                .with_kind(TextKind::Identifier)
                .dimmed(),
            ))
            .push(Box::new(self.pages()));
        if let Some(status) = &self.status {
            let action = |kind: &str| Pending::Action {
                origin: status.origin.clone(),
                generation: status.generation,
                kind: kind.into(),
            };
            if self.information {
                let connection = match status.security.as_str() {
                    "secure" if status.content_status == 0 => "Connection is secure",
                    "secure" => "Encrypted connection with mixed content",
                    "insecure" => "HTTP connection is not encrypted",
                    _ => "Connection security is not verified",
                };
                column = column.push(Box::new(
                    Text::new(connection).with_role(TextRole::Subheading),
                ));
                let mut rows = Vec::new();
                if !status.subject.is_empty() {
                    rows.push(
                        Property::new("Certificate", status.subject.clone())
                            .with_kind(TextKind::Identifier),
                    );
                    rows.push(Property::new("Issuer", status.issuer.clone()));
                    rows.push(Property::new("Connection", tls_version(status.tls_version)));
                    if status.certificate_status != 0 {
                        rows.push(
                            Property::new(
                                "Certificate problem",
                                format!("Status code {}", status.certificate_status),
                            )
                            .with_kind(TextKind::Numeric),
                        );
                    }
                    rows.push(
                        Property::new("Expires", certificate_date(status.expires))
                            .with_kind(TextKind::Numeric),
                    );
                }
                for (name, value) in &status.permissions {
                    rows.push(Property::new(
                        permission_label(name),
                        permission_state(value),
                    ));
                }
                if !rows.is_empty() {
                    column = column.push(facts(rows));
                }
                column = column
                    .push(Box::new(
                        self.button("browser.site.revoke", "Block permissions", action("revoke"))
                            .role(ButtonRole::Destructive)
                            .with_enabled(self.action.is_none()),
                    ))
                    .push(note(
                        "Blocks camera, microphone, location and notifications for this site. Reload to end existing media sessions.",
                    ))
                    .push(Box::new(
                        self.button("browser.site.clear", "Clear site data", action("clear"))
                            .role(ButtonRole::Destructive)
                            .with_enabled(self.action.is_none()),
                    ))
                    .push(note(
                        "Clears this site’s storage and matching cookies. Cookies may also be used on other ports and subdomains. Reload to reset data still in this page.",
                    ));
            } else {
                let state = if !status.global_enabled {
                    "Protection is off in Settings"
                } else if status.excepted {
                    "Protection is off for this site"
                } else {
                    "Protection is on for this site"
                };
                column = column
                    .push(Box::new(Text::new(state).with_role(TextRole::Subheading)))
                    .push(line(&format!(
                        "{} requests blocked or replaced in this page",
                        status.blocked
                    )))
                    .push(note(
                        "This includes the site’s embedded frames. Requests without a known top-level page stay protected and are not counted here.",
                    ))
                    .push(Box::new(
                        self.button(
                            "browser.site.toggle",
                            if status.excepted {
                                "Turn on and reload"
                            } else {
                                "Turn off and reload"
                            },
                            action(if status.excepted { "enable" } else { "disable" }),
                        )
                        .with_enabled(self.action.is_none() && self.pending_reload.is_none()),
                    ))
                    .push(line(&self.filters.message));
                if let Some(updated) = self.filters.updated {
                    column = column.push(note(&format!(
                        "Filters last updated {} UTC",
                        unix_date(updated)
                    )));
                }
                column = column.push(Box::new(
                    self.button(
                        "browser.site.filters.retry",
                        "Update filters",
                        Pending::Refresh,
                    )
                    .with_enabled(!self.filters.busy),
                ));
            }
        } else if self.committed.is_some() {
            column = column.push(line("Reading site information…"));
        }
        if !self.notice.is_empty() {
            column = column.push(line(&self.notice));
        }
        Box::new(
            Container::new()
                .with_focus_trap()
                .with_no_padding()
                .with_width(width)
                .with_height(height)
                .with_child(Box::new(
                    Scrollable::new(ScrollAxis::Vertical)
                        .with_focus_reveal()
                        .with_height(height)
                        .with_child(Box::new(Container::new().with_child(Box::new(column)))),
                )),
        )
    }
}
/// How the engine's permission names and states read in the panel.
fn permission_label(name: &str) -> String {
    match name {
        "camera" => "Camera",
        "microphone" => "Microphone",
        "location" => "Location",
        "notifications" => "Notifications",
        other => other,
    }
    .to_owned()
}
fn permission_state(state: &str) -> String {
    match state {
        "ask" => "Ask",
        "blocked" => "Blocked",
        "allowed" | "granted" => "Allowed",
        other => other,
    }
    .to_owned()
}
// Calendar conversion without a timezone or page-controlled formatter.
fn unix_date(seconds: u64) -> String {
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX);
    let (y, m, d) = ui::date::civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        (seconds % 86400) / 3600,
        (seconds % 3600) / 60
    )
}
fn tls_version(version: u32) -> &'static str {
    match version {
        3 => "TLS 1.0",
        4 => "TLS 1.1",
        5 => "TLS 1.2",
        6 => "TLS 1.3",
        7 => "QUIC",
        _ => "TLS version unavailable",
    }
}
fn certificate_date(base_micros: f64) -> String {
    let unix = base_micros / 1_000_000.0 - 11_644_473_600.0;
    if unix.is_finite() && unix > 0.0 {
        format!("{} UTC", unix_date(unix as u64))
    } else {
        "unavailable".into()
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    struct NativeRecordingEngine(std::sync::Arc<ui::webview::test_engine::TestEngine>);
    impl ui::webview::WebViewEngine for NativeRecordingEngine {
        fn supports_native_filtering(&self) -> bool {
            true
        }
        fn send(&self, command: WebViewCommand) {
            self.0.send(command);
        }
        fn frames(&self) -> ui::webview::FrameSlot {
            self.0.frames()
        }
        fn drain_events(&self) -> Vec<WebViewEvent> {
            self.0.drain_events()
        }
    }
    /// Origin canonicalisation, exceptions, private sharing and certificate text.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn site_origins_exceptions_and_certificates_are_canonical() {
        // Phase `canonical_origin_rejects_nonweb_and_preserves_ports`.
        {
            assert_eq!(
                origin("HTTPS://User:pass@EXAMPLE.com:443/path?q=1#x"),
                Some("https://example.com".into())
            );
            assert_eq!(
                origin("http://[::1]:8080/a"),
                Some("http://[::1]:8080".into())
            );
            for url in [
                "data:text/html,test",
                "about:blank",
                "file:///tmp/a",
                "https://",
                "null",
            ] {
                assert_eq!(origin(url), None);
            }
        }

        // Phase `exceptions_roundtrip_exact_origins_and_reenable`.
        {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("sites.json");
            let mut rules = Exceptions::default();
            rules.set("https://EXAMPLE.com:443/path", true);
            rules.save(&path).unwrap();
            let mut loaded = Exceptions::load(&path);
            assert!(loaded.contains("https://example.com/other"));
            assert!(!loaded.contains("http://example.com/"));
            assert!(!loaded.contains("https://example.com:444/"));
            loaded.set("https://example.com/", false);
            assert!(!loaded.contains("https://example.com/"));
        }

        // Phase `private_tabs_share_runtime_exceptions_without_disk_persistence`.
        {
            let recorder = ui::webview::test_engine::TestEngine::new();
            let engine: SharedEngine = std::sync::Arc::new(NativeRecordingEngine(recorder.clone()));
            let normal = tempfile::tempdir().unwrap();
            let normal_file = normal.path().join("site-exceptions.json");
            let mut normal_exceptions = Exceptions::default();
            normal_exceptions.set("https://normal.test", true);
            normal_exceptions.save(&normal_file).unwrap();
            let normal_before = std::fs::read(&normal_file).unwrap();
            let mut first = SiteControls::default();
            let mut second = SiteControls::default();
            first.initialize(&engine, false);
            second.share_runtime_state(&first);
            first.committed = Some("https://private.test".into());
            first.action = Some((
                7,
                "https://private.test".into(),
                3,
                "disable".into(),
                ui::time::now(),
            ));
            assert!(first.apply(WebViewEvent::SiteResult {
                request_id: 7,
                origin: "https://private.test".into(),
                generation: 3,
                action: "disable".into(),
                success: true,
                message: "disabled".into(),
            }));
            assert!(
                second
                    .runtime_exceptions
                    .lock()
                    .unwrap()
                    .contains("https://private.test/child")
            );
            second.initialize(&engine, false);
            assert!(recorder.commands().iter().any(|command| {
                matches!(command, WebViewCommand::SetSiteExceptions(origins) if origins == &["https://private.test".to_string()])
            }));
            assert_eq!(std::fs::read(normal_file).unwrap(), normal_before);
        }

        // Phase `certificate_dates_and_protocol_versions_are_human_readable`.
        {
            assert_eq!(unix_date(0), "1970-01-01 00:00");
            assert_eq!(
                certificate_date(11_644_473_600.0 * 1_000_000.0 + 86400.0 * 1_000_000.0),
                "1970-01-02 00:00 UTC"
            );
            assert_eq!(tls_version(6), "TLS 1.3");
        }
    }
    /// Panel viewport, navigation discards and bounded polling.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn site_panel_is_scrollable_and_discards_stale_state_on_navigation() {
        // Phase `navigation_discards_count_pending_actions_and_stale_status`.
        {
            let mut controls = SiteControls::default();
            controls.open = true;
            controls.committed = Some("https://one.test".into());
            controls.query = Some((7, ui::time::now()));
            controls.notice = "old notice".into();
            controls.navigation("https://two.test/page");
            assert!(!controls.open);
            assert!(controls.query.is_none());
            assert!(controls.notice.is_empty());
            assert_eq!(controls.committed.as_deref(), Some("https://two.test"));
            assert!(!controls.apply(WebViewEvent::SiteResult {
                request_id: 7,
                origin: "https://one.test".into(),
                generation: 1,
                action: "clear".into(),
                success: true,
                message: "cleared".into()
            }));
            assert!(controls.notice.is_empty());
        }

        // Phase `site_panel_has_a_visible_scroll_viewport_and_actionable_children`.
        {
            fn find(w: &dyn Widget, id: &str) -> Option<vello::kurbo::Rect> {
                if w.test_id() == Some(id) {
                    return Some(w.get_rect());
                }
                w.children().iter().find_map(|c| find(*c, id))
            }

            let mut controls = SiteControls::default();
            controls.open = true;
            controls.committed = Some("https://example.test".into());
            let theme = ui::theme::Theme::dark();
            let mut panel = controls.panel(480.0, 444.0);
            let mut tree = taffy::TaffyTree::<()>::new();
            let mut nodes = std::collections::HashMap::new();
            let mut text = ui::text_render::TextRenderer::new();
            let node = panel.build_layout(&mut tree, &theme, &mut nodes);
            tree.compute_layout_with_measure(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(480.0),
                    height: taffy::AvailableSpace::Definite(444.0),
                },
                |known, available, node, _, _| {
                    nodes.get(&node).map_or(taffy::Size::ZERO, |ptr| unsafe {
                        (**ptr).measure(known, available, &mut text)
                    })
                },
            )
            .unwrap();
            panel.apply_layout(&tree, node, vello::kurbo::Point::ORIGIN);
            let scroll = panel.children()[0];
            assert!(
                scroll.get_rect().height() > 100.0,
                "scroll viewport: {:?}",
                scroll.get_rect()
            );
            let done = find(panel.as_ref(), "browser.site.close").unwrap();
            assert!(done.height() > 0.0);
            assert!(scroll.get_rect().contains(done.center()));
            let mut doc = automerge::AutoCommit::new();
            let mut handled;
            let mut focus = None;
            for event in [
                winit::event::WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: winit::dpi::PhysicalPosition::new(done.center().x, done.center().y),
                },
                winit::event::WindowEvent::MouseInput {
                    device_id: winit::event::DeviceId::dummy(),
                    state: winit::event::ElementState::Pressed,
                    button: winit::event::MouseButton::Left,
                },
                winit::event::WindowEvent::MouseInput {
                    device_id: winit::event::DeviceId::dummy(),
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                },
            ] {
                handled = false;
                panel.handle_event(
                    &event,
                    &mut ui::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &winit::event::Modifiers::default(),
                        state: &mut doc,
                        path: &automerge::ROOT,
                        handled: &mut handled,
                        focus_target: &mut focus,
                        node_id: node,
                        taffy: &tree,
                    },
                );
            }
            let intent = ui::widget::controls::take_control_invocation(panel.as_mut())
                .expect("pointer emits the Done operation");
            assert_eq!(intent.address.resource, "tilo.control:browser.site.close");
            let result = panel
                .start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        ui::tilo_actions::OperationKind::Effect,
                        "tilo.ui.control.invoke",
                        &serde_json::to_vec("browser.site.close").unwrap(),
                    ),
                    &mut doc,
                    &automerge::ROOT,
                )
                .unwrap();
            assert!(result.is_some());
            assert!(matches!(controls.pending.get(), Some(Pending::Close)));
        }

        // Phase `polling_is_bounded_and_stale_buttons_cannot_act_after_navigation`.
        {
            let capture = ui::webview::test_engine::TestEngine::new();
            let engine: SharedEngine = capture.clone();
            let mut controls = SiteControls::default();
            controls.committed = Some("https://one.test".into());
            controls.open = true;
            for _ in 0..100 {
                controls.sync(&engine);
            }
            assert_eq!(
                capture
                    .commands()
                    .iter()
                    .filter(|c| matches!(c, WebViewCommand::RequestSiteStatus { .. }))
                    .count(),
                1
            );
            controls.navigation("https://two.test");
            controls.pending.set(Some(Pending::Action {
                origin: "https://one.test".into(),
                generation: 1,
                kind: "clear".into(),
            }));
            controls.sync(&engine);
            assert!(
                !capture
                    .commands()
                    .iter()
                    .any(|c| matches!(c, WebViewCommand::SiteAction { .. }))
            );
        }
    }

    fn protection_toggle(kind: &str) -> SiteControls {
        SiteControls {
            committed: Some("https://one.test".into()),
            status: Some(SiteStatus {
                request_id: 1,
                origin: "https://one.test".into(),
                generation: 3,
                blocked: 0,
                global_enabled: true,
                excepted: kind == "enable",
                effective: kind != "enable",
                security: "secure".into(),
                certificate_status: 0,
                tls_version: 0,
                content_status: 0,
                subject: String::new(),
                issuer: String::new(),
                expires: 0.0,
                permissions: Vec::new(),
            }),
            action: Some((
                7,
                "https://one.test".into(),
                3,
                kind.into(),
                ui::time::now(),
            )),
            ..SiteControls::default()
        }
    }

    fn toggle_reply(kind: &str, success: bool) -> WebViewEvent {
        WebViewEvent::SiteResult {
            request_id: 7,
            origin: "https://one.test".into(),
            generation: 3,
            action: kind.into(),
            success,
            message: "toggle result".into(),
        }
    }

    /// Protection reload success, cancellation and stale replies.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn protection_toggle_reloads_only_after_a_current_success() {
        // Phase `successful_protection_toggle_reloads_once_even_after_panel_dismissal`.
        {
            for kind in ["enable", "disable"] {
                for dismiss in [false, true] {
                    let recorder = ui::webview::test_engine::TestEngine::new();
                    let engine: SharedEngine = recorder.clone();
                    let mut controls = protection_toggle(kind);
                    assert!(controls.apply(toggle_reply(kind, true)));
                    assert_eq!(
                        controls
                            .runtime_exceptions
                            .lock()
                            .unwrap()
                            .contains("https://one.test"),
                        kind == "disable"
                    );
                    assert!(
                        !controls.apply(toggle_reply(kind, true)),
                        "duplicate toggle reply"
                    );
                    if dismiss {
                        controls.close();
                    }
                    controls.sync(&engine);
                    controls.sync(&engine);
                    let reloads: Vec<_> = recorder.commands().into_iter().filter(|command| {
                        matches!(command, WebViewCommand::SiteAction { action, .. } if action == "reload")
                    }).collect();
                    assert_eq!(reloads.len(), 1, "{kind}, dismiss={dismiss}");
                    let WebViewCommand::SiteAction {
                        request_id,
                        origin,
                        generation,
                        ..
                    } = &reloads[0]
                    else {
                        unreachable!()
                    };
                    assert_ne!(*request_id, 7);
                    assert_eq!(origin, "https://one.test");
                    assert_eq!(*generation, 3);
                    assert!(controls.apply(WebViewEvent::SiteResult {
                        request_id: *request_id,
                        origin: origin.clone(),
                        generation: *generation,
                        action: "reload".into(),
                        success: true,
                        message: "Reloading".into(),
                    }));
                    controls.sync(&engine);
                    assert_eq!(recorder.commands().iter().filter(|command| matches!(command, WebViewCommand::SiteAction { action, .. } if action == "reload")).count(), 1);
                }
            }
        }

        // Phase `protection_reload_is_cancelled_by_navigation_or_new_document`.
        {
            for target in [
                "https://two.test/page",
                "https://one.test/other",
                "new-generation",
            ] {
                let recorder = ui::webview::test_engine::TestEngine::new();
                let engine: SharedEngine = recorder.clone();
                let mut controls = protection_toggle("disable");
                assert!(controls.apply(toggle_reply("disable", true)));
                if target == "new-generation" {
                    controls.status.as_mut().unwrap().generation = 4;
                } else {
                    controls.navigation(target);
                }
                controls.sync(&engine);
                assert!(
                    !recorder
                        .commands()
                        .iter()
                        .any(|command| matches!(command, WebViewCommand::SiteAction { .. })),
                    "{target}"
                );
            }
        }

        // Phase `failed_or_stale_protection_replies_do_not_reload`.
        {
            for case in [
                "failure",
                "request",
                "origin",
                "generation",
                "action",
                "navigation",
            ] {
                let recorder = ui::webview::test_engine::TestEngine::new();
                let engine: SharedEngine = recorder.clone();
                let mut controls = protection_toggle("disable");
                let mut reply = toggle_reply("disable", case != "failure");
                if let WebViewEvent::SiteResult {
                    request_id,
                    origin,
                    generation,
                    action,
                    ..
                } = &mut reply
                {
                    match case {
                        "request" => *request_id = 8,
                        "origin" => *origin = "https://two.test".into(),
                        "generation" => *generation = 4,
                        "action" => *action = "enable".into(),
                        "navigation" => controls.navigation("https://one.test/other"),
                        _ => {}
                    }
                }
                assert_eq!(controls.apply(reply), case == "failure");
                controls.sync(&engine);
                assert!(
                    !recorder
                        .commands()
                        .iter()
                        .any(|command| matches!(command, WebViewCommand::SiteAction { .. })),
                    "{case}"
                );
            }
        }
    }
}
