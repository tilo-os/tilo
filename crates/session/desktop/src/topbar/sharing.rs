//! The people on the bar: the avatars of everyone here, the colours their
//! cursor and avatar settle on, who is watching or sharing, and the continuity
//! chip beside them.

use super::*;

impl TopBarChrome {
    /// Where the person's own avatar was last drawn, for anchoring their
    /// profile card; zero when it is not shown.
    pub fn own_avatar_rect(&self) -> Rect {
        self.control_rect(Self::CONTROL_PROFILE)
    }

    /// Where the others' faces (or the sharing indicator) were last drawn,
    /// for anchoring who is here; zero when neither is shown.
    pub fn collaborators_rect(&self) -> Rect {
        self.control_rect(Self::CONTROL_COLLABORATORS)
    }

    /// What the own avatar is called to assistive technology.
    pub(super) fn profile_control_name(&self) -> String {
        let name = self.own_chip().map(|own| own.name).unwrap_or_default();
        t_args("topbar-profile", &[("name", name.into())])
    }

    /// The faces in the people control, one named image each, then the
    /// "+N" chip for those beyond them. Keyed by the roster id, so a
    /// reader's place survives someone arriving or leaving.
    pub(super) fn face_accessibility(&self) -> Vec<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, Role};
        let mut faces: Vec<AccessNode> = self
            .collaborator_names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let name = if self
                    .collaborator_agents
                    .get(index)
                    .copied()
                    .unwrap_or(false)
                {
                    t_args("topbar-face-agent", &[("name", name.as_str().into())])
                } else {
                    name.clone()
                };
                let mut face = AccessNode::new(Role::Image)
                    .with_name(name)
                    .with_hashed_key((
                        "topbar-face",
                        self.collaborator_ids.get(index).map_or("", String::as_str),
                    ));
                if let Some(rect) = self.face_rects.get(index) {
                    face = face.with_bounds(*rect);
                }
                face
            })
            .collect();
        if self.collaborator_overflow > 0 {
            faces.push(
                AccessNode::new(Role::StaticText)
                    .with_name(t_args(
                        "topbar-faces-more",
                        &[("count", self.collaborator_overflow.into())],
                    ))
                    .with_hashed_key("topbar-faces-more"),
            );
        }
        faces
    }

    /// The person's own avatar, when the bar shows it.
    pub(super) fn own_chip(&self) -> Option<crate::chrome::OwnChip> {
        if self.is_compact() {
            return None;
        }
        self.chrome.as_ref()?.snapshot().own
    }

    /// Read how each face looks from `presence`: who is idle or away, in a
    /// call (their ring breathing), or bouncing from a wave.
    pub(super) fn read_face_looks(
        &mut self,
        presence: &PresenceCtx,
        motion: ui::tokens::EffectiveMotion,
    ) {
        let now = ui::time::now();
        let mut anyone_in_call = false;
        let since = *self.call_since.get_or_insert(now);
        self.face_looks = self
            .collaborator_ids
            .iter()
            .map(|id| {
                let user = ui::permissions::UserId(id.clone());
                let in_call = presence.call_of(&user).is_some();
                anyone_in_call |= in_call;
                FaceLook {
                    dimmed: presence.availability_of(&user).dims(),
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

    pub fn collaborator_initials(&self) -> &[char] {
        &self.collaborator_initials
    }

    /// Whether the workspace pill names the strip shown.
    pub(super) fn shows_strip_name(&self) -> bool {
        self.strip_name().is_some()
    }

    pub fn collaborator_colors(&self) -> &[Color] {
        &self.collaborator_colors
    }

    pub fn collaborator_overflow(&self) -> usize {
        self.collaborator_overflow
    }

    /// Pinned so a future revert to the old four-rect quad fails loudly
    /// instead of silently regressing.
    pub fn overview_glyph_icon() -> ui::icons::Icon {
        ui::icons::Icon::LayoutGrid
    }

    pub(super) fn collaborators_visible(&self) -> bool {
        !self.collaborator_initials.is_empty()
            || self.collaborator_overflow > 0
            || self.watching() > 0
            || self.sharing_summary().0 > 0
    }

    /// People following the room without taking part, from the host.
    pub(super) fn watching(&self) -> u32 {
        self.chrome
            .as_ref()
            .map_or(0, |chrome| chrome.snapshot().watching)
    }

    /// `(viewers, controllers, surfaces)` from the compositor authority.
    pub(super) fn sharing_summary(&self) -> (usize, usize, usize) {
        let Some(chrome) = self.chrome.as_ref() else {
            return (0, 0, 0);
        };
        let snapshot = chrome.snapshot();
        let mut peers = snapshot
            .active_exposures
            .iter()
            .map(|row| row.peer)
            .collect::<Vec<_>>();
        peers.compact_sort();
        peers.dedup();
        let mut controllers = snapshot
            .active_exposures
            .iter()
            .filter(|row| row.can_control)
            .map(|row| row.peer)
            .collect::<Vec<_>>();
        controllers.compact_sort();
        controllers.dedup();
        let mut surfaces = snapshot
            .active_exposures
            .iter()
            .map(|row| row.surface)
            .collect::<Vec<_>>();
        surfaces.compact_sort();
        surfaces.dedup();
        (peers.len(), controllers.len(), surfaces.len())
    }

    /// Named viewers for disclosure prose. Transport ids are joined to the
    /// active room's live roster through `Collaborator::session`; an unknown
    /// session remains visible under its deterministic peer handle.
    pub(super) fn sharing_viewers(&self) -> Vec<(u64, String, Option<bool>, bool)> {
        let Some(chrome) = self.chrome.as_ref() else {
            return Vec::new();
        };
        let snapshot = chrome.snapshot();
        let mut authority = Vec::<(u64, bool)>::new();
        for row in &snapshot.active_exposures {
            if let Some((_, can_control)) = authority.iter_mut().find(|(peer, _)| *peer == row.peer)
            {
                *can_control |= row.can_control;
            } else {
                authority.push((row.peer, row.can_control));
            }
        }
        authority.compact_sort_by_key(|(peer, _)| *peer);
        let tree = chrome.tree();
        let mut viewers = Vec::new();
        for (peer, can_control) in authority {
            if let Some((workspace, participant)) = tree.workspaces.iter().find_map(|workspace| {
                workspace
                    .collaborators
                    .iter()
                    .find(|person| person.session == Some(peer))
                    .map(|participant| (workspace, participant))
            }) {
                let is_agent = workspace.policy.grants.iter().any(|grant| {
                    matches!(
                        &grant.subject,
                        tilo_action_policy::Subject::Agent(id)
                            if id.eq_ignore_ascii_case(&participant.id)
                                || id.eq_ignore_ascii_case(&participant.name)
                    )
                });
                viewers.push((peer, participant.name.clone(), Some(is_agent), can_control));
            } else {
                viewers.push((peer, format!("p{peer:016x}"), None, can_control));
            }
        }
        viewers
    }

    pub(super) fn sharing_targets_for_peer(&self, peer: u64) -> (Vec<String>, Vec<String>) {
        let Some(chrome) = self.chrome.as_ref() else {
            return (Vec::new(), Vec::new());
        };
        let snapshot = chrome.snapshot();
        let mut authority = Vec::<(u64, bool)>::new();
        for row in snapshot
            .active_exposures
            .iter()
            .filter(|row| row.peer == peer)
        {
            if let Some((_, can_control)) = authority
                .iter_mut()
                .find(|(surface, _)| *surface == row.surface)
            {
                *can_control |= row.can_control;
            } else {
                authority.push((row.surface, row.can_control));
            }
        }
        authority.compact_sort_by_key(|(surface, _)| *surface);
        let tree = chrome.tree();
        let controlled = authority
            .iter()
            .filter(|(_, can_control)| *can_control)
            .map(|(surface, _)| *surface)
            .collect();
        let view_only = authority
            .iter()
            .filter(|(_, can_control)| !*can_control)
            .map(|(surface, _)| *surface)
            .collect();
        (
            Self::sharing_target_labels(&tree, view_only),
            Self::sharing_target_labels(&tree, controlled),
        )
    }

    /// Glance text puts the privacy state before identity so elision can
    /// shorten a long display name without ever hiding "controls"/"viewing"
    /// or whether the participant is an agent.
    pub(super) fn sharing_label(&self) -> Option<String> {
        let (viewers, controllers, _) = self.sharing_summary();
        if viewers == 0 {
            return None;
        }
        let named = self.sharing_viewers();
        Some(match named.as_slice() {
            [(_, name, is_agent, controls)] => {
                let key = match (is_agent, controls) {
                    (Some(true), true) => "topbar-sharing-agent-controls",
                    (Some(true), false) => "topbar-sharing-agent-viewing",
                    (Some(false), true) => "topbar-sharing-person-controls",
                    (Some(false), false) => "topbar-sharing-person-viewing",
                    (None, true) => "topbar-sharing-session-controls",
                    (None, false) => "topbar-sharing-session-viewing",
                };
                t_args(key, &[("name", name.as_str().into())])
            }
            _ if controllers > 0 => t_args(
                "topbar-sharing-count-controlling",
                &[
                    ("controllers", controllers.into()),
                    ("viewers", viewers.into()),
                ],
            ),
            _ => t_args("topbar-sharing-count", &[("viewers", viewers.into())]),
        })
    }

    pub(super) fn sharing_target_labels(tree: &DesktopTree, surfaces: Vec<u64>) -> Vec<String> {
        surfaces
            .into_iter()
            .map(|surface| {
                tree.workspaces
                    .iter()
                    .find_map(|workspace| {
                        workspace
                            .strips
                            .iter()
                            .flat_map(|strip| &strip.window_groups)
                            .flat_map(|group| &group.tabs)
                            .find(|tab| {
                                matches!(
                                    &tab.tile,
                                    tilo_desktop_model::TileContent::Foreign { surface_id, .. }
                                        if *surface_id == surface
                                )
                            })
                            .map(|tab| (workspace, tab))
                    })
                    // A window the tree does not list has no name a person
                    // knows; its surface number would mean nothing to them.
                    .map_or_else(
                        || t("topbar-sharing-unknown-window"),
                        |(workspace, tab)| {
                            if workspace.id == tree.active {
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

    /// The person's own avatar, ending at `right_x`, in their colour with
    /// their initials; returns where the controls before it end. It opens
    /// their profile card, so it is set apart from the others' stack.
    pub(super) fn draw_own_avatar(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        right_x: f64,
    ) -> f64 {
        let Some(own) = self.own_chip() else {
            self.own_avatar_rect = Rect::ZERO;
            return right_x;
        };
        let side = BAR_CONTROL_PX;
        let chip = Rect::new(
            right_x - side,
            center_y - side / 2.0,
            right_x,
            center_y + side / 2.0,
        );
        let (r, g, b) = own.rgb;
        ui::widget::AvatarFace::new(&own.name, &own.identity)
            .presence(Color::from_rgb8(r, g, b))
            .ringed_on(theme.tokens.colors.surfaces.base)
            .paint(
                &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                chip,
            );
        self.own_avatar_rect = chip;
        self.draw_avatar_ask_dot(scene, transform, theme);
        chip.x0 - f64::from(theme.tokens.spacing.s3)
    }

    /// Paint an explicit, persistent disclosure beside the roster. An eye
    /// plus a count is retained even for view-only shares; control changes
    /// the colour and adds a pointer count instead of replacing visibility.
    pub(super) fn draw_sharing_indicator(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        right_x: f64,
    ) -> f64 {
        let (_, controllers, _) = self.sharing_summary();
        // A sharing label exists exactly when someone is viewing.
        let Some(label) = self.sharing_label() else {
            self.sharing_indicator_rect = Rect::ZERO;
            return right_x;
        };
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let color = if controllers > 0 {
            colors.semantic.warning
        } else {
            colors.intent.accent_ink
        };
        let style = tokens.typography.style(TextRole::Caption);
        let available = (right_x - self.center_lane.x1 - 48.0).clamp(28.0, 180.0);
        let label = text_renderer.elide_to_width_style(
            &label,
            ui::num::f32_of_f64(available),
            &style.clone().with_weight(TextWeight::Semibold),
        );
        let (label_w, label_h) = text_renderer.measure_style(
            &label,
            None,
            &style.clone().with_weight(TextWeight::Semibold),
        );
        let width = f64::from(label_w) + 31.0;
        self.sharing_indicator_rect = Rect::new(
            right_x - width,
            center_y - BAR_CONTROL_PX / 2.0,
            right_x,
            center_y + BAR_CONTROL_PX / 2.0,
        );
        let pill = RoundedRect::from_rect(
            self.sharing_indicator_rect,
            ui::chrome::ChromeRole::Selector.radius(theme, BAR_CONTROL_PX),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            color.with_alpha_factor(0.18),
            None,
            &pill,
        );
        ui::icons::draw_icon(
            scene,
            transform,
            Rect::new(
                self.sharing_indicator_rect.x0 + 7.0,
                center_y - 6.0,
                self.sharing_indicator_rect.x0 + 19.0,
                center_y + 6.0,
            ),
            ui::icons::Icon::Eye,
            color,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        text_renderer.draw_style(
            scene,
            &label,
            colors.text_primary,
            at(
                transform,
                (
                    self.sharing_indicator_rect.x0 + 23.0,
                    center_y - f64::from(label_h) / 2.0,
                ),
            ),
            None,
            &style.with_weight(TextWeight::Semibold),
        );
        self.sharing_indicator_rect.x0 - f64::from(tokens.spacing.s3)
    }

    /// A quiet offer to continue the tab another of this person's devices
    /// just moved to, left of the roster, with its own dismiss chip. It
    /// yields to the centre band: with no room for a readable label it is
    /// not drawn at all (the palette still offers it).
    pub(super) fn draw_continuity_chip(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        label: Option<&str>,
    ) {
        use ui::widget::{ChipFace, ChipLead};

        self.continuity_rect = Rect::ZERO;
        self.continuity_dismiss_rect = Rect::ZERO;
        let Some(label) = label else {
            return;
        };
        let gap = f64::from(theme.tokens.spacing.s2);
        let height = 24.0;
        let right = self.people_left_x - f64::from(theme.tokens.spacing.s3);
        let left_limit = self.center_lane.x1 + f64::from(theme.tokens.spacing.s4);
        let dismiss_w = height;
        let lead = ChipLead::Glyph(ui::icons::Icon::Monitor);
        let natural = ChipFace::intrinsic_width(label, lead, false, text_renderer, theme);
        let room = right - dismiss_w - gap - left_limit;
        // Below this the label elides to a word or two and stops saying
        // what it offers.
        if room < 96.0 {
            return;
        }
        let width = natural.min(room);
        let top = center_y - height / 2.0;
        self.continuity_dismiss_rect = Rect::new(right - dismiss_w, top, right, top + height);
        let chip_right = self.continuity_dismiss_rect.x0 - gap;
        self.continuity_rect = Rect::new(chip_right - width, top, chip_right, top + height);
        ChipFace::new(self.continuity_rect, label).lead(lead).paint(
            scene,
            transform,
            text_renderer,
            theme,
        );
        ChipFace::new(self.continuity_dismiss_rect, "")
            .lead(ChipLead::Glyph(ui::icons::Icon::X))
            .paint(scene, transform, text_renderer, theme);
    }

    /// The live continuity offer's label. Read from chrome rather than
    /// cached, so the semantic surface and the next paint agree even before
    /// a frame has drawn the chip.
    pub(super) fn continuity_label(&self) -> Option<String> {
        self.chrome.as_ref()?.continuity_offer()
    }
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

/// Cursor and avatar colours for a room, settled over the whole roster so
/// a peer's avatar matches the colour their cursor is drawn in elsewhere. An
/// explicit `cursor_color` outranks anything computed; the local user takes
/// part in the settlement (never the avatar stack) so every host agrees.
pub(super) fn roster_colors(
    roster: &[tilo_desktop_model::Collaborator],
    local: &str,
) -> HashMap<String, (u8, u8, u8)> {
    let explicit: HashMap<_, _> = roster
        .iter()
        .filter_map(|c| {
            c.cursor_color
                .as_deref()
                .and_then(ui::presence::parse_hex_color)
                .map(|rgb| (c.id.clone(), rgb))
        })
        .collect();
    let mut ids: Vec<_> = roster
        .iter()
        .filter(|c| !explicit.contains_key(&c.id))
        .map(|c| c.id.as_str())
        .collect();
    if !local.is_empty() && !explicit.contains_key(local) {
        ids.push(local);
    }
    let mut assigned =
        ui::presence::assign_peer_colors(&ids, &explicit.values().copied().collect::<Vec<_>>());
    assigned.extend(explicit);
    assigned
}
