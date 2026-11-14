//! The people on the bar: the avatars of everyone here, the colours their cursor and avatar settle
//! on, who is watching or sharing, and the continuity chip beside them.

/// The sharing pill's glyph: the eye that says the windows are shared.
static SHARE_GLYPH: &[ui::icons::Icon] = &[ui::icons::Icon::Eye];

use super::*;

impl TopBarChrome {
    /// Where the person's own avatar was last drawn, for anchoring their profile card; zero when it
    /// is not shown.
    pub fn own_avatar_rect(&self) -> Rect {
        self.control_rect(Self::CONTROL_PROFILE)
    }

    /// Where the others' faces (or the sharing indicator) were last drawn,
    /// for anchoring who is here; zero when neither is shown.
    pub fn roster_rect(&self) -> Rect {
        self.control_rect(Self::CONTROL_PEOPLE)
    }

    /// What the own avatar is called to assistive technology.
    pub(super) fn profile_control_name(&self) -> String {
        let name = self.own_chip().map(|own| own.name).unwrap_or_default();
        t_args("topbar-profile", &[("name", name.into())])
    }

    /// The faces in the people control, one named control each, then the "+N" chip for those
    /// beyond them. Keyed by the roster id, so a reader's place survives someone arriving or
    /// leaving, and each stands where the pointer reaches it: a reader presses a face where a
    /// person does, and it opens that person's card.
    pub(super) fn face_accessibility(&self) -> Vec<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, Role};
        // A bar not drawn yet stands the first few of everyone, as a wide bar does.
        let drawn: Vec<usize> = if self.face_rects.is_empty() {
            (0..self.faces_shown).collect()
        } else {
            self.shown_faces.clone()
        };
        let mut faces: Vec<AccessNode> = drawn
            .iter()
            .enumerate()
            .filter_map(|(drawn, &index)| {
                let name = self.face_names.get(index)?;
                let name = if self.face_agents.get(index).copied().unwrap_or(false) {
                    t_args("topbar-face-agent", &[("name", name.as_str().into())])
                } else {
                    name.clone()
                };
                let id = self.face_ids.get(index).map_or("", String::as_str);
                let mut face = AccessNode::new(Role::Button)
                    .with_name(name)
                    .with_action_target(format!("{}{id}", Self::FACE_TARGET))
                    .with_hashed_key(("topbar-face", id));
                if let Some(rect) = self.face_rects.get(drawn) {
                    face = face.with_bounds(*rect);
                }
                Some(face)
            })
            .collect();
        if self.face_overflow > 0 {
            let mut more = AccessNode::new(Role::StaticText)
                .with_name(t_args(
                    "topbar-faces-more",
                    &[("count", self.face_overflow.into())],
                ))
                .with_hashed_key("topbar-faces-more");
            if !self.people_count_rect.is_zero_area() {
                more = more.with_bounds(self.people_count_rect);
            }
            faces.push(more);
        }
        faces
    }

    /// The person's own avatar, when the bar shows it.
    pub(crate) fn own_chip(&self) -> Option<crate::chrome::OwnChip> {
        if self.is_compact() {
            return None;
        }
        self.chrome.as_ref()?.snapshot().own
    }

    /// Read how each face looks: who is idle or away, in a call (their ring breathing), or bouncing
    /// from a wave. What the roster the bar read says of each face leads: on a session whose people
    /// come from the collaboration plane, that is everyone here, and `presence` is empty. A face
    /// the roster says nothing of falls back to `presence` , which is where a wave's bounce always
    /// comes from.
    pub(super) fn read_face_looks(
        &mut self,
        presence: &PresenceCtx,
        motion: ui::tokens::EffectiveMotion,
    ) {
        let now = ui::time::now();
        let mut anyone_in_call = false;
        let since = *self.call_since.get_or_insert(now);
        self.face_looks = self
            .face_ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let user = ui::permissions::UserId(id.clone());
                let said = self.face_presence.get(index).copied().flatten();
                let in_call =
                    said.map_or_else(|| presence.call_of(&user).is_some(), |(_, call)| call);
                anyone_in_call |= in_call;
                let dimmed = said.map_or_else(
                    || presence.availability_of(&user).dims(),
                    |(availability, _)| availability.dims(),
                );
                FaceLook {
                    dimmed,
                    call: in_call.then(|| {
                        ui::motion::Breathe::level(now.saturating_duration_since(since), motion)
                    }),
                    scale: presence.face_scale(&user, now),
                }
            })
            .collect();
        if !anyone_in_call {
            self.call_since = None;
        }
    }

    pub fn face_initials(&self) -> &[char] {
        &self.face_initials
    }

    /// Whether the workspace pill names the strip shown.
    pub(super) fn shows_strip_name(&self) -> bool {
        self.strip_name().is_some()
    }

    pub fn face_colors(&self) -> &[Color] {
        &self.face_colors
    }

    pub fn face_overflow(&self) -> usize {
        self.face_overflow
    }

    /// Pinned so a future revert to the old four-rect quad fails loudly instead of silently
    /// regressing.
    pub fn overview_glyph_icon() -> ui::icons::Icon {
        ui::icons::Icon::LayoutGrid
    }

    pub(super) fn roster_visible(&self) -> bool {
        !self.face_initials.is_empty()
            || self.face_overflow > 0
            || self.watching() > 0
            || self.sharing_summary().viewers > 0
    }

    /// People following the workspace without taking part, from the host.
    pub(super) fn watching(&self) -> u32 {
        self.chrome
            .as_ref()
            .map_or(0, |chrome| chrome.snapshot().watching)
    }

    /// How the shared windows stand with the devices that asked to see them, from the compositor
    /// authority.
    pub(super) fn sharing_summary(&self) -> SharingSummary {
        let Some(chrome) = self.chrome.as_ref() else {
            return SharingSummary::default();
        };
        let snapshot = chrome.snapshot();
        let rows = &snapshot.active_exposures;
        let distinct = |mut ids: Vec<u64>| {
            ids.compact_sort();
            ids.dedup();
            ids.len()
        };
        let peers = |counts: fn(&crate::chrome::ActiveExposure) -> bool| {
            distinct(
                rows.iter()
                    .filter(|row| counts(row))
                    .map(|row| row.peer)
                    .collect(),
            )
        };
        SharingSummary {
            viewers: peers(|_| true),
            watching: peers(|row| row.streaming),
            in_control: peers(|row| row.streaming && row.can_control),
            may_control: peers(|row| row.can_control),
            surfaces: distinct(rows.iter().map(|row| row.surface).collect()),
        }
    }

    /// Named viewers for disclosure prose. A transport id is joined to the active workspace's live
    /// roster through the presence session ; a viewer no row carries goes by what its share says
    /// the workspace calls it, and one nothing names is said in words. A handle or a key means
    /// nothing to a person and is never shown as a name. Each says whether anything of a window
    /// goes out to it: one nothing goes out to asked to see a window and waits.
    pub(super) fn sharing_viewers(&self) -> Vec<SharingViewer> {
        let Some(chrome) = self.chrome.as_ref() else {
            return Vec::new();
        };
        let snapshot = chrome.snapshot();
        let mut viewers = Vec::<SharingViewer>::new();
        for row in &snapshot.active_exposures {
            if let Some(viewer) = viewers.iter_mut().find(|viewer| viewer.peer == row.peer) {
                viewer.take(row);
            } else {
                let mut viewer = SharingViewer {
                    peer: row.peer,
                    ..SharingViewer::default()
                };
                viewer.take(row);
                viewers.push(viewer);
            }
        }
        viewers.compact_sort_by_key(|viewer| viewer.peer);
        let tree = chrome.tree();
        for viewer in &mut viewers {
            let member = tree.workspaces.iter().find_map(|workspace| {
                let here = workspace
                    .present
                    .iter()
                    .find(|here| here.session == Some(viewer.peer))?;
                workspace
                    .members
                    .iter()
                    .find(|member| member.id == here.member)
            });
            if let Some(member) = member {
                viewer.name.clone_from(&member.name);
                viewer.kind = if member.id.is_person() {
                    ViewerKind::Person
                } else {
                    ViewerKind::Agent
                };
            } else if let Some(name) = ui::presence::shown_name(&viewer.name).map(str::to_owned) {
                viewer.name = name;
                viewer.kind = ViewerKind::Named;
            } else {
                viewer.name = t("topbar-sharing-unnamed-viewer");
                viewer.kind = ViewerKind::Unnamed;
            }
        }
        viewers
    }

    /// The windows `peer` asked to see, by what the bar calls them: those that go out to it, apart
    /// by whether it may control them, and those nothing of which goes out yet, apart the same way.
    pub(super) fn sharing_targets_for_peer(&self, peer: u64) -> SharingTargets {
        let Some(chrome) = self.chrome.as_ref() else {
            return SharingTargets::default();
        };
        let snapshot = chrome.snapshot();
        // Each window once: whether it goes out, and whether this viewer may control it.
        let mut windows = Vec::<(u64, bool, bool)>::new();
        for row in snapshot
            .active_exposures
            .iter()
            .filter(|row| row.peer == peer)
        {
            if let Some((_, streaming, can_control)) = windows
                .iter_mut()
                .find(|(surface, ..)| *surface == row.surface)
            {
                *streaming |= row.streaming;
                *can_control |= row.can_control;
            } else {
                windows.push((row.surface, row.streaming, row.can_control));
            }
        }
        windows.compact_sort_by_key(|(surface, ..)| *surface);
        let tree = chrome.tree();
        let labels = |streaming: bool, can_control: bool| {
            Self::sharing_target_labels(
                &tree,
                windows
                    .iter()
                    .filter(|(_, out, control)| *out == streaming && *control == can_control)
                    .map(|(surface, ..)| *surface)
                    .collect(),
            )
        };
        SharingTargets {
            view_only: labels(true, false),
            controlled: labels(true, true),
            waiting: labels(false, false),
            waiting_control: labels(false, true),
        }
    }

    /// Glance text puts the privacy state before identity so elision can shorten a long display
    /// name without ever hiding "controls"/"viewing" or whether the participant is an agent. A
    /// viewer nothing of a window goes out to waits: the pill says it watches, or is in control,
    /// only of a window that goes out to it.
    pub(super) fn sharing_label(&self) -> Option<String> {
        let summary = self.sharing_summary();
        if summary.viewers == 0 {
            return None;
        }
        let named = self.sharing_viewers();
        let waiting = summary.viewers.saturating_sub(summary.watching);
        Some(match named.as_slice() {
            [viewer] => {
                let [controls, viewing, waiting_control, waiting] = match viewer.kind {
                    ViewerKind::Agent => [
                        "topbar-sharing-agent-controls",
                        "topbar-sharing-agent-viewing",
                        "topbar-sharing-agent-waiting-control",
                        "topbar-sharing-agent-waiting",
                    ],
                    ViewerKind::Person => [
                        "topbar-sharing-person-controls",
                        "topbar-sharing-person-viewing",
                        "topbar-sharing-person-waiting-control",
                        "topbar-sharing-person-waiting",
                    ],
                    ViewerKind::Named | ViewerKind::Unnamed => [
                        "topbar-sharing-session-controls",
                        "topbar-sharing-session-viewing",
                        "topbar-sharing-session-waiting-control",
                        "topbar-sharing-session-waiting",
                    ],
                };
                let key = match (viewer.watching, viewer.in_control, viewer.may_control) {
                    (true, true, _) => controls,
                    (true, false, _) => viewing,
                    (false, _, true) => waiting_control,
                    (false, _, false) => waiting,
                };
                t_args(key, &[("name", viewer.name.as_str().into())])
            }
            _ if summary.watching == 0 && summary.may_control > 0 => t_args(
                "topbar-sharing-count-waiting-control",
                &[
                    ("controllers", summary.may_control.into()),
                    ("waiting", waiting.into()),
                ],
            ),
            _ if summary.watching == 0 => t_args(
                "topbar-sharing-count-waiting",
                &[("waiting", waiting.into())],
            ),
            _ if waiting == 0 && summary.in_control > 0 => t_args(
                "topbar-sharing-count-controlling",
                &[
                    ("controllers", summary.in_control.into()),
                    ("viewers", summary.watching.into()),
                ],
            ),
            _ if waiting == 0 => t_args(
                "topbar-sharing-count",
                &[("viewers", summary.watching.into())],
            ),
            _ if summary.in_control > 0 => t_args(
                "topbar-sharing-count-mixed-controlling",
                &[
                    ("controllers", summary.in_control.into()),
                    ("viewers", summary.watching.into()),
                    ("waiting", waiting.into()),
                ],
            ),
            _ => t_args(
                "topbar-sharing-count-mixed",
                &[
                    ("viewers", summary.watching.into()),
                    ("waiting", waiting.into()),
                ],
            ),
        })
    }

    /// What the people control tells a reader of the shared windows: who the screen is visible to
    /// and what each may do there, and who asked to see a window and has been sent nothing of it.
    /// A viewer stands among those the screen is visible to only for the windows that go out.
    pub(super) fn sharing_disclosure(&self, viewers: &[SharingViewer]) -> String {
        let mut seeing = Vec::new();
        let mut waiting = Vec::new();
        for viewer in viewers {
            let targets = self.sharing_targets_for_peer(viewer.peer);
            let kind = match viewer.kind {
                ViewerKind::Agent => "agent",
                ViewerKind::Person => "person",
                ViewerKind::Named => "named",
                ViewerKind::Unnamed => "unnamed",
            };
            let entry = |access: &[String]| {
                t_args(
                    "topbar-viewer-entry",
                    &[
                        ("name", viewer.name.as_str().into()),
                        ("kind", kind.into()),
                        ("access", access.join("; ").into()),
                    ],
                )
            };
            let said = |key: &str, windows: &[String]| {
                (!windows.is_empty())
                    .then(|| t_args(key, &[("targets", windows.join(", ").into())]))
            };
            let sent: Vec<String> = [
                said("topbar-viewer-can-control", &targets.controlled),
                said("topbar-viewer-view-only", &targets.view_only),
            ]
            .into_iter()
            .flatten()
            .collect();
            let unsent: Vec<String> = [
                said("topbar-viewer-waiting-control", &targets.waiting_control),
                said("topbar-viewer-waiting", &targets.waiting),
            ]
            .into_iter()
            .flatten()
            .collect();
            if !sent.is_empty() {
                seeing.push(entry(&sent));
            }
            if !unsent.is_empty() {
                waiting.push(entry(&unsent));
            }
        }
        let (seeing, waiting) = (seeing.join("; "), waiting.join("; "));
        match (seeing.is_empty(), waiting.is_empty()) {
            (true, true) => t("topbar-show-people"),
            (false, true) => t_args("topbar-people-visible", &[("viewers", seeing.into())]),
            (true, false) => t_args("topbar-people-waiting", &[("viewers", waiting.into())]),
            (false, false) => t_args(
                "topbar-people-visible-and-waiting",
                &[("viewers", seeing.into()), ("waiting", waiting.into())],
            ),
        }
    }

    pub(super) fn sharing_target_labels(tree: &DesktopTree, surfaces: Vec<u64>) -> Vec<String> {
        surfaces
            .into_iter()
            .map(|surface| {
                tree.workspaces
                    .iter()
                    .find_map(|workspace| {
                        // A shared window is told by the tab it stands in on this desk.
                        workspace
                            .strips
                            .iter()
                            .flat_map(|strip| &strip.window_groups)
                            .flat_map(|group| &group.tabs)
                            .find(|tab| tab.id == surface)
                            .map(|tab| (workspace, tab))
                    })
                    // A window the tree does not list has no name a person knows; its surface
                    // number would mean nothing to them.
                    .map_or_else(
                        || t("topbar-sharing-unknown-window"),
                        |(workspace, tab)| {
                            if workspace.slot == tree.active {
                                tab.title.clone()
                            } else {
                                t_args(
                                    "topbar-sharing-window-in-workspace",
                                    &[
                                        ("window", tab.title.as_str().into()),
                                        ("workspace", workspace.name.as_str().into()),
                                    ],
                                )
                            }
                        },
                    )
            })
            .collect()
    }

    /// The person's own face as everyone here sees it, drawn by the one face painter the others'
    /// faces use ([`ui::presence::FaceStack::BAR`]), so it has the same size, the same ring in
    /// their colour and the same initials.
    fn own_face(&self) -> Option<(String, String, Color)> {
        let own = self.own_chip()?;
        let (r, g, b) = own.rgb;
        Some((own.name, own.identity, Color::from_rgb8(r, g, b)))
    }

    /// Where the person's own avatar stands, ending at `right_x`. It opens their own menu.
    /// Returns where the controls before it end.
    pub(super) fn place_own_avatar(&mut self, theme: &Theme, center_y: f64, right_x: f64) -> f64 {
        self.placed.own_face = None;
        let Some((name, identity, colour)) = self.own_face() else {
            self.own_avatar_rect = Rect::ZERO;
            return right_x;
        };
        let face = ui::presence::Face::new(&name, &identity, colour);
        let stack = ui::presence::FaceStack::BAR;
        let (rects, _) = stack.layout((right_x, center_y), (&[face], 0));
        self.own_avatar_rect = Self::faces_drawn(rects)
            .first()
            .copied()
            .unwrap_or(Rect::ZERO);
        self.placed.own_face = Some(right_x);
        right_x - stack.width_of(1, 0) - f64::from(theme.tokens.spacing.s2)
    }

    /// Paint the person's own avatar where [`Self::place_own_avatar`] stood it.
    pub(super) fn paint_own_avatar(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) {
        let (Some(right_x), Some((name, identity, colour))) =
            (self.placed.own_face, self.own_face())
        else {
            return;
        };
        let face = ui::presence::Face::new(&name, &identity, colour);
        let _ = ui::presence::FaceStack::BAR.paint(
            &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
            (right_x, self.placed.center_y),
            (&[face], 0),
            theme.tokens.colors.surfaces.base,
        );
    }

    /// The sharing disclosure's chip showing `label` in `available` room: the eye, the words, and
    /// the state's tone washed into the chip's body — warning where the person let someone
    /// control a window, whether or not it goes out to them yet, the accent while they only view.
    pub(super) fn sharing_chip<'a>(
        &self,
        theme: &Theme,
        label: &'a str,
        available: f64,
    ) -> ui::widget::BarChipFace<'a> {
        let tokens = &theme.tokens;
        let color = if self.sharing_summary().may_control > 0 {
            tokens.colors.semantic.warning
        } else {
            tokens.colors.intent.accent_ink
        };
        ui::widget::BarChipFace::new(label, &[])
            .icons(SHARE_GLYPH)
            .label(label)
            .label_width(available)
            .tone(color)
            .at_height(BAR_CONTROL_PX)
            .hovered(false)
            .focused(
                self.focus_ring_visible()
                    && self.focused_control.as_deref() == Some(Self::CONTROL_PEOPLE),
            )
    }

    /// Where the explicit, persistent disclosure beside the roster stands. An eye plus a count is
    /// retained even for view-only shares; control changes the colour and adds a pointer count
    /// instead of replacing visibility.
    pub(super) fn place_sharing_indicator(
        &mut self,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        right_x: f64,
    ) -> f64 {
        self.placed.sharing = None;
        // A sharing label exists exactly when someone asked to see a window.
        let Some(label) = self.sharing_label() else {
            self.sharing_indicator_rect = Rect::ZERO;
            return right_x;
        };
        let available = (right_x - self.center_lane.x1 - 48.0).clamp(28.0, 180.0);
        let width = self
            .sharing_chip(theme, &label, available)
            .width(text_renderer, theme);
        self.sharing_indicator_rect =
            ui::widget::BarChipFace::place_at(right_x - width, center_y, width, BAR_CONTROL_PX);
        self.placed.sharing = Some((label, available));
        self.sharing_indicator_rect.x0 - f64::from(theme.tokens.spacing.s2)
    }

    /// Paint the disclosure where [`Self::place_sharing_indicator`] stood it.
    pub(super) fn paint_sharing_indicator(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) {
        let Some((label, available)) = self.placed.sharing.as_ref() else {
            return;
        };
        let _ = self.sharing_chip(theme, label, *available).paint(
            &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
            self.sharing_indicator_rect,
            theme.tokens.colors.surfaces.base,
        );
    }

    /// Where a quiet offer to continue the tab another of this person's devices just moved to
    /// stands, left of the roster, with its own dismiss chip. It yields to the centre band: with
    /// no room for a readable label it does not stand at all (the palette still offers it).
    pub(super) fn place_continuity_chip(
        &mut self,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        label: Option<String>,
    ) {
        use ui::widget::{ChipFace, ChipLead};

        self.continuity_rect = Rect::ZERO;
        self.continuity_dismiss_rect = Rect::ZERO;
        self.placed.continuity = None;
        let Some(label) = label else {
            return;
        };
        let gap = f64::from(theme.tokens.spacing.s2);
        let height = 24.0;
        let right = self.people_left_x - f64::from(theme.tokens.spacing.s3);
        let left_limit = self.center_lane.x1 + f64::from(theme.tokens.spacing.s4);
        let dismiss_w = height;
        let lead = ChipLead::Glyph(ui::icons::Icon::Monitor);
        let natural = ChipFace::intrinsic_width(&label, lead, false, text_renderer, theme);
        let room = right - dismiss_w - gap - left_limit;
        // Below this the label elides to a word or two and stops saying what it offers.
        if room < 96.0 {
            return;
        }
        let width = natural.min(room);
        let top = center_y - height / 2.0;
        self.continuity_dismiss_rect = Rect::new(right - dismiss_w, top, right, top + height);
        let chip_right = self.continuity_dismiss_rect.x0 - gap;
        self.continuity_rect = Rect::new(chip_right - width, top, chip_right, top + height);
        self.placed.continuity = Some(label);
    }

    /// Paint the continuity offer where [`Self::place_continuity_chip`] stood it.
    pub(super) fn paint_continuity_chip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) {
        use ui::widget::{ChipFace, ChipLead};
        let Some(label) = self.placed.continuity.as_ref() else {
            return;
        };
        ChipFace::new(self.continuity_rect, label)
            .lead(ChipLead::Glyph(ui::icons::Icon::Monitor))
            .paint(scene, transform, text_renderer, theme);
        ChipFace::new(self.continuity_dismiss_rect, "")
            .lead(ChipLead::Glyph(ui::icons::Icon::X))
            .paint(scene, transform, text_renderer, theme);
    }

    /// The live continuity offer's label. Read from chrome rather than cached, so the semantic
    /// surface and the next paint agree even before a frame has drawn the chip.
    pub(super) fn continuity_label(&self) -> Option<String> {
        self.chrome.as_ref()?.continuity_offer()
    }
}

/// How the windows this device shares stand with the devices that asked to see them, counted by
/// viewer: what the bar says of them, and repaints for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct SharingSummary {
    /// Everyone who asked to see a window of this device.
    pub(super) viewers: usize,
    /// Those something of a window goes out to: they watch, and the others wait.
    pub(super) watching: usize,
    /// Those who control a window that goes out to them.
    pub(super) in_control: usize,
    /// Everyone the person let control a window, whether or not it goes out to them yet.
    pub(super) may_control: usize,
    /// The windows they asked to see.
    pub(super) surfaces: usize,
}

/// What the bar knows of who a viewer is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum ViewerKind {
    /// A row of the roster carries it, and says it is an agent.
    Agent,
    /// A row of the roster carries it, and says it is a person.
    Person,
    /// No row carries it, and its share says what the workspace calls it.
    Named,
    /// Nothing names it: the bar says so in words.
    #[default]
    Unnamed,
}

/// One viewer of this device's shared windows, as the bar names it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct SharingViewer {
    pub(super) peer: u64,
    /// The name the bar says: the roster's, what its share says the workspace calls it, or the
    /// bar's words for a viewer nothing names.
    pub(super) name: String,
    pub(super) kind: ViewerKind,
    /// Something of a window goes out to it: it watches. Until then it asked to see one and waits.
    pub(super) watching: bool,
    /// It controls a window that goes out to it.
    pub(super) in_control: bool,
    /// The person let it control a window, whether or not that window goes out to it yet.
    pub(super) may_control: bool,
}

impl SharingViewer {
    /// Take what `row`, one window this viewer asked to see, says of it.
    fn take(&mut self, row: &crate::chrome::ActiveExposure) {
        self.watching |= row.streaming;
        self.in_control |= row.streaming && row.can_control;
        self.may_control |= row.can_control;
        if self.name.is_empty() {
            self.name.clone_from(&row.name);
        }
    }
}

/// The windows one viewer asked to see, by what the bar calls them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct SharingTargets {
    /// Windows that go out to it, which it only views.
    pub(super) view_only: Vec<String>,
    /// Windows that go out to it, which it may control.
    pub(super) controlled: Vec<String>,
    /// Windows nothing of which goes out to it yet.
    pub(super) waiting: Vec<String>,
    /// Windows nothing of which goes out to it yet, which the person let it control.
    pub(super) waiting_control: Vec<String>,
}

/// How one face in the people stack looks now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct FaceLook {
    /// Idle or away.
    pub(super) dimmed: bool,
    /// In a call: the breathing ring's level.
    pub(super) call: Option<f32>,
    /// A wave's bounce (1 at rest).
    pub(super) scale: f64,
}
