//! Quick settings device rows: geometry, details and faces.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// Copy `index` of the Clipboard section, where the page shows it.
    pub(super) fn qs_clipboard_row_rect(&self, index: usize) -> Rect {
        self.chrome
            .as_ref()
            .and_then(|chrome| {
                self.qs_root.hosted_row_rect(
                    &chrome.borrow().quick_settings,
                    crate::shell::quick_settings::QsSection::Clipboard,
                    index,
                )
            })
            .unwrap_or(Rect::ZERO)
    }

    pub(super) fn qs_device_row_actions(row: Rect) -> ([Rect; 4], Rect) {
        crate::shell::quick_settings::hosted::placement_rects(row)
    }

    /// The Devices row text for a local input: whether it is the one in
    /// use, and its level.
    pub(super) fn local_input_detail(input: &LocalMediaInputView) -> String {
        if !input.is_default {
            return t("qs-devices-mic-other");
        }
        let mut detail = t("qs-devices-mic-current");
        if let Some(level) = input.level_percent {
            detail.push_str(&t_args("qs-devices-mic-level", &[("level", level.into())]));
        }
        if input.muted {
            detail.push_str(&t("qs-devices-mic-muted"));
        }
        detail
    }

    pub(super) fn qs_realtime_consent_actions(row: Rect) -> (Rect, Rect) {
        crate::shell::quick_settings::hosted::pair_rects(row)
    }

    pub(super) fn qs_selected_media(&self) -> Option<&String> {
        match &self.qs_selected_source {
            Some(QsDeviceSource::Media(session_id)) => Some(session_id),
            _ => None,
        }
    }

    /// Which of a device row's two trailing buttons `point` hits: `true` for
    /// the first (allow), `false` for the second (decline), `None` for neither.
    pub(super) fn qs_device_row_choice(&self, row_index: usize, point: Point) -> Option<bool> {
        let (first, second) =
            Self::qs_realtime_consent_actions(self.qs_devices_row_rect(row_index));
        if first.contains(point) {
            Some(true)
        } else if second.contains(point) {
            Some(false)
        } else {
            None
        }
    }

    /// Defaults the source to the playing (or first) local media session
    /// while no other source is chosen, and drops a media session that ended.
    pub(super) fn normalise_qs_selected_media(&mut self, view: &DeviceFabricView) {
        match &self.qs_selected_source {
            Some(QsDeviceSource::Media(selected))
                if !view
                    .local_media
                    .iter()
                    .any(|media| &media.session_id == selected) => {}
            None => {}
            Some(_) => return,
        }
        self.qs_selected_source = view
            .local_media
            .iter()
            .find(|media| media.playing)
            .or_else(|| view.local_media.first())
            .map(|media| QsDeviceSource::Media(media.session_id.clone()));
    }

    pub(super) fn fabric_device_detail(
        device: &FabricDeviceView,
        selected: Option<&FabricMediaView>,
        transfer: Option<&FabricTransferView>,
    ) -> String {
        if device.is_local {
            return t("qs-devices-this-device");
        }
        if !device.online {
            return t("qs-devices-offline");
        }
        if let Some(transfer) = transfer {
            return match transfer.status {
                FabricTransferStatus::Sending => {
                    let percent = transfer
                        .sent_bytes
                        .saturating_mul(100)
                        .checked_div(transfer.total_bytes)
                        .unwrap_or(0);
                    t_args("qs-devices-sending", &[("percent", percent.into())])
                }
                FabricTransferStatus::Sent => t("qs-devices-sent"),
                FabricTransferStatus::Failed => t("qs-devices-transfer-failed"),
            };
        }
        if device.controls_local && device.local_can_control {
            return t("qs-devices-mutual-control");
        }
        if device.controls_local {
            return t("qs-devices-may-control");
        }
        if device.local_can_control {
            return t("qs-devices-control-granted");
        }
        match selected {
            Some(media) if media.targets.contains(&device.id) => t_args(
                "qs-devices-play-here",
                &[("media", media.title.as_str().into())],
            ),
            Some(media) if media.has_video && !device.video_output => t("qs-devices-audio-only"),
            Some(_) if !device.audio_output => t("qs-devices-no-output"),
            Some(_) => t("qs-devices-unavailable-media"),
            None => t("qs-devices-online"),
        }
    }

    /// The health clause a device row adds after its status: only what a
    /// person acts on (its power). Reachability is already the status, and
    /// latency or route counts are diagnostics.
    pub(super) fn fabric_health_detail(
        health: &crate::shell::spatial_calibration::DeviceHealthView,
    ) -> Option<String> {
        use crate::shell::spatial_calibration::DevicePower;
        let power = match health.power {
            DevicePower::Unknown => return None,
            DevicePower::External => t("qs-devices-external-power"),
            DevicePower::Charging(percent) => {
                t_args("qs-devices-charging", &[("percent", percent.into())])
            }
            DevicePower::Battery(percent) => {
                t_args("qs-devices-battery", &[("percent", percent.into())])
            }
        };
        Some(t_args("qs-devices-health", &[("power", power.into())]))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the bounded device rows share paint geometry with their capability and transport status"
    )]
    pub(super) fn draw_qs_bluetooth_details(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        qs: &crate::shell::chrome_state::QuickSettingsState,
        address: &str,
    ) {
        let accessory = qs
            .bt_devices
            .iter()
            .find(|device| device.address.eq_ignore_ascii_case(address));
        let fallback_name = t("a11y-bluetooth-accessory");
        let name = accessory.map_or(fallback_name.as_str(), |device| device.name.as_str());
        self.draw_qs_drill_in_header(
            scene,
            transform,
            text_renderer,
            theme,
            &t_args("a11y-bluetooth-move", &[("accessory", name.into())]),
        );
        let panel = self.quick_settings_rect();
        text_renderer.draw_style(
            scene,
            &t("qs-bluetooth-move-note"),
            theme.tokens.colors.text_dim,
            transform.pre_translate((panel.x0 + 12.0, panel.y0 + 56.0).into()),
            Some(f32_of_f64(panel.width() - 24.0)),
            &theme.text(TextRole::Caption),
        );
        let Ok(view) = self.device_fabric_call(DeviceFabricUiAction::Snapshot) else {
            self.draw_qs_drill_in_empty(
                scene,
                transform,
                text_renderer,
                theme,
                &t("qs-devices-fabric-unavailable"),
            );
            return;
        };
        let rows = std::iter::once(None)
            .chain(view.devices.into_iter().map(Some))
            .collect::<Vec<_>>();
        let _ = self.clamp_qs_device_fabric_start(rows.len());
        let visible_rows = self.qs_device_fabric_visible_rows();
        for (index, item) in rows
            .iter()
            .skip(self.qs_device_fabric_start)
            .take(visible_rows)
            .enumerate()
        {
            let row = self.qs_device_row_rect(index);
            let (title, detail, icon, enabled) = match item {
                None => (
                    t("qs-bluetooth-this-device"),
                    t_args("qs-bluetooth-disconnect", &[("accessory", name.into())]),
                    ui::icons::Icon::Bluetooth,
                    accessory.is_some_and(|device| device.connected),
                ),
                Some(device) => (
                    device.label.clone(),
                    if device.is_local {
                        t("qs-bluetooth-this-certified")
                    } else if device.online {
                        t("qs-bluetooth-move-here")
                    } else {
                        t("qs-devices-offline")
                    },
                    ui::icons::Icon::Monitor,
                    !device.is_local && device.online,
                ),
            };
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(row, "")
                    .hovered(enabled && self.last_cursor.is_some_and(|point| row.contains(point)))
                    .disabled(!enabled)
                    .accessible_name(&format!("{title}. {detail}")),
            );
            ui::icons::draw_icon(
                scene,
                transform,
                Rect::new(row.x0 + 12.0, row.y0 + 15.0, row.x0 + 32.0, row.y0 + 35.0),
                icon,
                theme.tokens.colors.text_primary,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
            text_renderer.draw_style(
                scene,
                &title,
                theme.tokens.colors.text_primary,
                transform.pre_translate((row.x0 + 42.0, row.y0 + 10.0).into()),
                Some(f32_of_f64(row.width() - 54.0)),
                &theme.text(TextRole::Label),
            );
            text_renderer.draw_style(
                scene,
                &detail,
                theme.tokens.colors.text_dim,
                transform.pre_translate((row.x0 + 42.0, row.y0 + 32.0).into()),
                Some(f32_of_f64(row.width() - 54.0)),
                &theme.text(TextRole::Caption),
            );
        }
        if rows.len() > visible_rows {
            let shown = visible_rows.min(rows.len().saturating_sub(self.qs_device_fabric_start));
            text_renderer.draw_style(
                scene,
                &t_args(
                    "qs-devices-showing",
                    &[
                        (
                            "first",
                            self.qs_device_fabric_start.saturating_add(1).into(),
                        ),
                        (
                            "last",
                            self.qs_device_fabric_start.saturating_add(shown).into(),
                        ),
                        ("total", rows.len().into()),
                    ],
                ),
                theme.tokens.colors.text_dim,
                transform.pre_translate((panel.x0 + 12.0, panel.y1 - 14.0).into()),
                Some(f32_of_f64(panel.width() - 24.0)),
                &theme.text(TextRole::Caption),
            );
        }
    }

    /// The title, detail, icon, selected state and enabled state of one
    /// device-fabric row: what the pane draws and what it announces.
    #[expect(
        clippy::too_many_lines,
        reason = "one presentation arm per device-fabric row kind"
    )]
    pub(super) fn qs_device_row_face(
        &self,
        view: &DeviceFabricView,
        selected: Option<&FabricMediaView>,
        item: &DeviceFabricPaneRow,
    ) -> (String, String, ui::icons::Icon, bool, bool) {
        match item {
            DeviceFabricPaneRow::CalibrationProposal(proposal) => {
                let label = view
                    .devices
                    .iter()
                    .find(|device| device.id == proposal.peer)
                    .map_or_else(|| t("a11y-certified-display"), |device| device.label.clone());
                let side = match proposal.edge {
                    crate::shell::device_fabric::DisplayEdge::Left => "left",
                    crate::shell::device_fabric::DisplayEdge::Right => "right",
                    crate::shell::device_fabric::DisplayEdge::Top => "above",
                    crate::shell::device_fabric::DisplayEdge::Bottom => "below",
                };
                (
                    t_args("qs-devices-place", &[("display", label.into()), ("side", side.into())]),
                    t("qs-devices-place-note"),
                    ui::icons::Icon::Monitor,
                    false,
                    true,
                )
            }
            DeviceFabricPaneRow::CalibrationRevert(device) => (
                t_args(
                    "a11y-placement-undo",
                    &[(
                        "display",
                        view.devices
                            .iter()
                            .find(|item| item.id == *device)
                            .map_or_else(|| t("a11y-display-fallback"), |item| item.label.clone())
                            .into(),
                    )],
                ),
                t("qs-devices-placement-undo-note"),
                ui::icons::Icon::Monitor,
                false,
                true,
            ),
            DeviceFabricPaneRow::SpeakerConsent(consent) => (
                consent.prompt(),
                consent.note(),
                match consent.scope {
                    crate::shell::speaker_group_runtime::SpeakerConsentScope::Play(_) => {
                        ui::icons::Icon::Volume2
                    }
                    crate::shell::speaker_group_runtime::SpeakerConsentScope::Listen => {
                        ui::icons::Icon::Mic
                    }
                },
                false,
                true,
            ),
            DeviceFabricPaneRow::SpeakerSession(session) => (
                t_args(
                    "a11y-speaker-playing-on",
                    &[("group", session.name.as_str().into())],
                ),
                match session.status() {
                    crate::shell::speaker_group_runtime::SpeakerSessionStatus::ClockFailed(
                        reason,
                    ) => t_args(
                        "qs-devices-speaker-clock-failed",
                        &[
                            ("count", session.failed_members.into()),
                            ("reason", t(reason.message_key()).into()),
                        ],
                    ),
                    crate::shell::speaker_group_runtime::SpeakerSessionStatus::SyncingClocks => {
                        t_args(
                            "qs-devices-speaker-syncing",
                            &[
                                ("playing", session.playing_members.into()),
                                ("syncing", session.syncing_members.into()),
                            ],
                        )
                    }
                    _ if session.inbound => t_args(
                        "qs-devices-speaker-for",
                        &[(
                            "peer",
                            session
                                .peer_label
                                .clone()
                                .unwrap_or_else(|| t("qs-devices-another-device"))
                                .into(),
                        )],
                    ),
                    crate::shell::speaker_group_runtime::SpeakerSessionStatus::Connecting => {
                        t_args(
                            "qs-devices-speaker-connecting",
                            &[
                                ("playing", session.playing_members.into()),
                                ("waiting", session.waiting_members.into()),
                            ],
                        )
                    }
                    crate::shell::speaker_group_runtime::SpeakerSessionStatus::Playing => {
                        t_args(
                            "qs-devices-speakers-playing",
                            &[("count", session.playing_members.into())],
                        )
                    }
                },
                ui::icons::Icon::Volume2,
                true,
                true,
            ),
            DeviceFabricPaneRow::SpeakerGroup(group) => (
                group.name.clone(),
                speaker_group_summary(group),
                ui::icons::Icon::Volume2,
                false,
                group.ready_members() > 0
                    && matches!(
                        self.qs_selected_source,
                        Some(QsDeviceSource::RemoteAudio(_) | QsDeviceSource::Media(_))
                    ),
            ),
            DeviceFabricPaneRow::SpeakerSetup => (
                t("a11y-speaker-setup"),
                t("qs-devices-speaker-setup-note"),
                ui::icons::Icon::Volume2,
                false,
                true,
            ),
            DeviceFabricPaneRow::SpeakerMembership(membership) => (
                membership.title(sentence_case),
                membership.note(),
                ui::icons::Icon::Volume2,
                false,
                true,
            ),
            DeviceFabricPaneRow::RemoteAudioConsent(consent) => {
                let source_label = view
                    .devices
                    .iter()
                    .find(|device| device.id == consent.source)
                    .map_or_else(|| t("qs-devices-another-certified"), |device| device.label.clone());
                (
                    t_args(
                        "a11y-remote-audio-play-here",
                        &[("media", consent.label.as_str().into())],
                    ),
                    t_args(
                        "qs-devices-requested-allow-once",
                        &[("source", source_label.into())],
                    ),
                    ui::icons::Icon::Volume2,
                    false,
                    true,
                )
            }
            DeviceFabricPaneRow::RemoteAudioSource(source) => (
                source.label.clone(),
                if source.application {
                    t("qs-devices-audio-app")
                } else {
                    t("qs-devices-audio-system")
                },
                ui::icons::Icon::Volume2,
                self.qs_selected_source == Some(QsDeviceSource::RemoteAudio(source.token)),
                true,
            ),
            DeviceFabricPaneRow::RemoteAudioRoute(route) => {
                let peer = view
                    .devices
                    .iter()
                    .find(|device| device.id == route.peer)
                    .map_or_else(|| t("qs-devices-certified-device"), |device| device.label.clone());
                let detail = match route.phase {
                    crate::shell::remote_audio::RemoteAudioPhase::AwaitingConsent => {
                        t_args("qs-devices-audio-awaiting", &[("peer", peer.into())])
                    }
                    crate::shell::remote_audio::RemoteAudioPhase::SyncingClock => {
                        t_args("qs-devices-audio-syncing", &[("peer", peer.into())])
                    }
                    crate::shell::remote_audio::RemoteAudioPhase::ClockFailed(reason) => t_args(
                        "qs-devices-audio-clock-failed",
                        &[
                            ("peer", peer.into()),
                            ("reason", t(reason.message_key()).into()),
                        ],
                    ),
                    crate::shell::remote_audio::RemoteAudioPhase::Streaming => {
                        t_args("qs-devices-audio-streaming", &[("peer", peer.into())])
                    }
                    crate::shell::remote_audio::RemoteAudioPhase::Interrupted => {
                        t_args("qs-devices-audio-interrupted", &[("peer", peer.into())])
                    }
                };
                (
                    route.label.clone(),
                    detail,
                    ui::icons::Icon::Volume2,
                    true,
                    true,
                )
            }
            DeviceFabricPaneRow::RemoteCaptureConsent(consent) => {
                let source_label = view
                    .devices
                    .iter()
                    .find(|device| device.id == consent.peer)
                    .map_or_else(|| t("qs-devices-another-certified"), |device| device.label.clone());
                (
                    t_args(
                        "qs-devices-capture-share",
                        &[("input", t(remote_capture_kind_key(consent.profile.kind())).into())],
                    ),
                    t_args(
                        "qs-devices-capture-requested",
                        &[
                            ("source", source_label.into()),
                            ("app", consent.app_label.as_str().into()),
                        ],
                    ),
                    ui::icons::Icon::Camera,
                    false,
                    true,
                )
            }
            DeviceFabricPaneRow::RemoteCaptureSource(kind) => (
                t_args(
                    "qs-devices-capture-use-remote",
                    &[("input", t(remote_capture_kind_key(*kind)).into())],
                ),
                t("qs-devices-capture-source-note"),
                ui::icons::Icon::Camera,
                self.qs_selected_source == Some(QsDeviceSource::RemoteCapture(*kind)),
                true,
            ),
            DeviceFabricPaneRow::RemoteCaptureSession(session) => {
                let peer = view
                    .devices
                    .iter()
                    .find(|device| device.id == session.peer)
                    .map_or_else(|| t("qs-devices-certified-device"), |device| device.label.clone());
                let detail = match session.phase {
                    crate::shell::remote_capture::runtime::RemoteCaptureSessionPhase::Requesting => {
                        t_args("qs-devices-capture-waiting-on", &[("peer", peer.into())])
                    }
                    crate::shell::remote_capture::runtime::RemoteCaptureSessionPhase::Provisioning => {
                        t("a11y-remote-capture-provisioning")
                    }
                    crate::shell::remote_capture::runtime::RemoteCaptureSessionPhase::AppApprovalRequired => {
                        t("qs-devices-capture-allow-app")
                    }
                    crate::shell::remote_capture::runtime::RemoteCaptureSessionPhase::WaitingForReceiver => {
                        t_args("qs-devices-capture-waiting-ready", &[("peer", peer.into())])
                    }
                    crate::shell::remote_capture::runtime::RemoteCaptureSessionPhase::Streaming => {
                        t_args("qs-devices-capture-private", &[("peer", peer.into())])
                    }
                };
                (
                    t_args(
                        "a11y-remote-capture-session",
                        &[("input", t(remote_capture_kind_key(session.kind)).into())],
                    ),
                    detail,
                    ui::icons::Icon::Camera,
                    session.phase
                        == crate::shell::remote_capture::runtime::RemoteCaptureSessionPhase::Streaming,
                    session.phase
                        == crate::shell::remote_capture::runtime::RemoteCaptureSessionPhase::AppApprovalRequired
                        || session.session_id.is_some(),
                )
            }
            DeviceFabricPaneRow::RemoteCaptureDegraded(degraded) => (
                t_args(
                    "a11y-remote-capture-unavailable",
                    &[("input", t(remote_capture_kind_key(degraded.kind)).into())],
                ),
                match degraded.reason {
                    crate::shell::remote_capture::runtime::RemoteCaptureDegradedReason::SourceUnavailable => {
                        t("qs-devices-capture-source-unavailable")
                    }
                    crate::shell::remote_capture::runtime::RemoteCaptureDegradedReason::ReceiverUnavailable => {
                        t("qs-devices-capture-receiver-unavailable")
                    }
                    crate::shell::remote_capture::runtime::RemoteCaptureDegradedReason::TransferInterrupted => {
                        t("qs-devices-capture-interrupted")
                    }
                },
                ui::icons::Icon::Camera,
                false,
                false,
            ),
            DeviceFabricPaneRow::RealtimeConsent(consent) => {
                let source_label = view
                    .devices
                    .iter()
                    .find(|device| device.id == consent.source)
                    .map_or_else(|| t("qs-devices-another-certified"), |device| device.label.clone());
                (
                    t_args(
                        "a11y-call-continue",
                        &[("room", consent.room_label.as_str().into())],
                    ),
                    t_args(
                        "qs-devices-call-requested",
                        &[("source", source_label.into())],
                    ),
                    ui::icons::Icon::Camera,
                    false,
                    true,
                )
            }
            DeviceFabricPaneRow::RealtimeSource(source) => (
                t_args(
                    "a11y-call-in",
                    &[("room", source.room_label.as_str().into())],
                ),
                t("qs-devices-call-source-note"),
                ui::icons::Icon::Camera,
                matches!(&self.qs_selected_source, Some(QsDeviceSource::Realtime(session)) if *session == source.session),
                true,
            ),
            DeviceFabricPaneRow::BluetoothAuthorization(authorization) => (
                t_args(
                    "a11y-bluetooth-pair",
                    &[("accessory", authorization.accessory_name.as_str().into())],
                ),
                t_args(
                    "qs-devices-bluetooth-requested",
                    &[("source", authorization.source_label.as_str().into())],
                ),
                ui::icons::Icon::Bluetooth,
                false,
                true,
            ),
            DeviceFabricPaneRow::BluetoothHandoff(status) => (
                status.accessory_name.clone(),
                match status.phase {
                    crate::shell::device_fabric_runtime::BluetoothHandoffPhaseView::PreparingTarget =>
                        t_args("qs-devices-handoff-checking", &[("peer", status.peer_label.as_str().into())]),
                    crate::shell::device_fabric_runtime::BluetoothHandoffPhaseView::AwaitingApproval =>
                        t_args("qs-devices-handoff-awaiting", &[("peer", status.peer_label.as_str().into())]),
                    crate::shell::device_fabric_runtime::BluetoothHandoffPhaseView::ReleasingSource =>
                        t("qs-devices-handoff-releasing"),
                    crate::shell::device_fabric_runtime::BluetoothHandoffPhaseView::PairingTarget =>
                        t_args("qs-devices-handoff-pairing", &[("peer", status.peer_label.as_str().into())]),
                    crate::shell::device_fabric_runtime::BluetoothHandoffPhaseView::ConnectingTarget =>
                        t_args("qs-devices-handoff-connecting", &[("peer", status.peer_label.as_str().into())]),
                    crate::shell::device_fabric_runtime::BluetoothHandoffPhaseView::AwaitingConfirmation =>
                        t("qs-devices-handoff-confirming"),
                    crate::shell::device_fabric_runtime::BluetoothHandoffPhaseView::Completed =>
                        t_args("qs-devices-handoff-connected", &[("peer", status.peer_label.as_str().into())]),
                    crate::shell::device_fabric_runtime::BluetoothHandoffPhaseView::Failed(failure) if status.outbound =>
                        t_args(
                            "qs-devices-handoff-failed-retry",
                            &[("reason", sentence_case(&t(bluetooth_handoff_failure_key(failure))).into())],
                        ),
                    crate::shell::device_fabric_runtime::BluetoothHandoffPhaseView::Failed(failure) =>
                        sentence_case(&t(bluetooth_handoff_failure_key(failure))),
                    crate::shell::device_fabric_runtime::BluetoothHandoffPhaseView::Cancelled if status.outbound =>
                        t("qs-devices-cancelled-retry"),
                    crate::shell::device_fabric_runtime::BluetoothHandoffPhaseView::Cancelled =>
                        t("qs-devices-cancelled"),
                },
                ui::icons::Icon::Bluetooth,
                false,
                status.phase
                    != crate::shell::device_fabric_runtime::BluetoothHandoffPhaseView::Completed
                    && (status.outbound
                        || !matches!(
                            status.phase,
                            crate::shell::device_fabric_runtime::BluetoothHandoffPhaseView::Failed(_)
                                | crate::shell::device_fabric_runtime::BluetoothHandoffPhaseView::Cancelled
                        )),
            ),
            DeviceFabricPaneRow::GamepadAuthorization(authorization) => (
                t_args(
                    "a11y-controller-use",
                    &[("controller", authorization.controller_label.as_str().into())],
                ),
                t_args(
                    "qs-devices-controller-requested",
                    &[
                        ("source", authorization.source_label.as_str().into()),
                        ("app", authorization.app_id.as_str().into()),
                    ],
                ),
                ui::icons::Icon::Mouse,
                false,
                true,
            ),
            DeviceFabricPaneRow::LocalGamepad(gamepad) => (
                gamepad.name.clone(),
                gamepad.forwarding.as_ref().map_or_else(
                    || t("qs-devices-controller-local"),
                    |forwarding| {
                        let target = view
                            .devices
                            .iter()
                            .find(|device| device.id == forwarding.target)
                            .map_or_else(|| t("qs-devices-another-device"), |device| device.label.clone());
                        if forwarding.ready {
                            t_args("qs-devices-controller-forwarding", &[("target", target.into())])
                        } else {
                            t_args("qs-devices-controller-awaiting", &[("target", target.into())])
                        }
                    },
                ),
                ui::icons::Icon::Mouse,
                self.qs_selected_source == Some(QsDeviceSource::Gamepad(gamepad.token)) || gamepad.forwarding.is_some(),
                true,
            ),
            DeviceFabricPaneRow::MediaHandoff(status) => (
                t_args(
                    "a11y-media-handoff",
                    &[
                        ("media", status.title.as_str().into()),
                        ("target", status.target_label.as_str().into()),
                    ],
                ),
                match status.phase {
                    crate::shell::device_fabric_runtime::MediaHandoffPhase::Pending =>
                        t("qs-devices-media-waiting"),
                    crate::shell::device_fabric_runtime::MediaHandoffPhase::Completed =>
                        t("qs-devices-media-opened"),
                    crate::shell::device_fabric_runtime::MediaHandoffPhase::Failed(failure) =>
                        t_args(
                            "qs-devices-media-failed",
                            &[("reason", t(media_handoff_failure_key(failure)).into())],
                        ),
                    crate::shell::device_fabric_runtime::MediaHandoffPhase::Cancelled =>
                        t("qs-devices-cancelled-retry"),
                },
                if view
                    .local_media
                    .iter()
                    .find(|media| media.session_id == status.session_id)
                    .is_some_and(|media| media.has_video)
                {
                    ui::icons::Icon::Film
                } else {
                    ui::icons::Icon::Music
                },
                false,
                status.phase
                    != crate::shell::device_fabric_runtime::MediaHandoffPhase::Completed,
            ),
            DeviceFabricPaneRow::Media(media) => (
                media.title.clone(),
                if media.playing {
                    t("qs-devices-media-playing")
                } else {
                    t("qs-devices-media-paused")
                },
                if media.has_video {
                    ui::icons::Icon::Film
                } else {
                    ui::icons::Icon::Music
                },
                self.qs_selected_media() == Some(&media.session_id),
                true,
            ),
            DeviceFabricPaneRow::LocalInput(input) => (
                input.label.clone(),
                Self::local_input_detail(input),
                ui::icons::Icon::Mic,
                input.is_default,
                true,
            ),
            DeviceFabricPaneRow::LocalOutput(output) => (
                output.label.clone(),
                output.bluetooth_codec.as_ref().map_or_else(
                    || {
                        if output.is_default {
                            t("qs-devices-output-default")
                        } else {
                            t("qs-devices-output-route")
                        }
                    },
                    |codec| t_args("qs-devices-output-bluetooth", &[("codec", codec.as_str().into())]),
                ),
                ui::icons::Icon::Volume2,
                output.is_default,
                !output.is_default,
            ),
            DeviceFabricPaneRow::Tethering(status) => {
                use crate::shell::personal_tethering_runtime::{
                    TetheringDirection, TetheringPhase,
                };
                let detail = match status.phase {
                    TetheringPhase::Requesting => t("qs-devices-tether-requesting"),
                    TetheringPhase::ConsentRequired => t("qs-devices-tether-consent"),
                    TetheringPhase::Preparing => t("qs-devices-tether-preparing"),
                    TetheringPhase::Connecting => t("qs-devices-tether-connecting"),
                    TetheringPhase::Active => match status.direction {
                        TetheringDirection::Requester => t("qs-devices-tether-requester"),
                        TetheringDirection::Provider => t("qs-devices-tether-provider"),
                    },
                    TetheringPhase::Denied => t("qs-devices-tether-denied"),
                    TetheringPhase::Expired => t("qs-devices-tether-expired"),
                    TetheringPhase::Offline => t("qs-devices-tether-offline"),
                    TetheringPhase::PolicyDisabled => t("qs-devices-tether-disabled"),
                    TetheringPhase::Failed => t("qs-devices-tether-failed"),
                    TetheringPhase::Disconnected => t("qs-devices-tether-disconnected"),
                };
                (
                    t_args(
                        "qs-devices-tether-with",
                        &[("peer", status.peer_label.as_str().into())],
                    ),
                    detail,
                    ui::icons::Icon::Wifi,
                    status.phase == TetheringPhase::Active,
                    status.can_disconnect(),
                )
            }
            DeviceFabricPaneRow::Device(device) => {
                let enabled = !device.is_local
                    && device.online
                    && (device.kind == crate::shell::device_fabric::DeviceKind::Phone
                        || matches!(
                            self.qs_selected_source,
                            Some(
                                QsDeviceSource::Gamepad(_)
                                    | QsDeviceSource::RemoteAudio(_)
                                    | QsDeviceSource::RemoteCapture(_)
                                    | QsDeviceSource::Realtime(_)
                            )
                        )
                        || selected.is_some_and(|media| media.targets.contains(&device.id)));
                let mut detail = if device.kind
                    == crate::shell::device_fabric::DeviceKind::Phone
                    && !device.is_local
                    && device.online
                    && self.qs_selected_source.is_none()
                {
                    t("qs-devices-ask-phone")
                } else if matches!(self.qs_selected_source, Some(QsDeviceSource::Gamepad(_)))
                    && !device.is_local
                    && device.online
                {
                    t("qs-devices-use-controller")
                } else if let Some(QsDeviceSource::RemoteCapture(kind)) = self.qs_selected_source
                    && !device.is_local
                    && device.online
                {
                    t_args(
                        "qs-devices-use-input",
                        &[("input", t(remote_capture_kind_key(kind)).into())],
                    )
                } else if matches!(self.qs_selected_source, Some(QsDeviceSource::RemoteAudio(_)))
                    && !device.is_local
                    && device.online
                {
                    if device.audio_output {
                        t("qs-devices-play-audio-here")
                    } else {
                        t("qs-devices-no-audio")
                    }
                } else {
                    Self::fabric_device_detail(
                        device,
                        selected,
                        view.transfers
                            .iter()
                            .rev()
                            .find(|transfer| transfer.target == device.id),
                    )
                };
                if let Some(health) = view
                    .device_health
                    .iter()
                    .find(|health| health.peer == device.id)
                    .and_then(Self::fabric_health_detail)
                {
                    detail.push_str(" · ");
                    detail.push_str(&health);
                }
                (
                    device.label.clone(),
                    detail,
                    ui::icons::Icon::Monitor,
                    false,
                    enabled,
                )
            }
        }
    }

    /// The names of a device-fabric row's trailing buttons, left to right.
    pub(super) fn qs_device_row_button_names(item: &DeviceFabricPaneRow) -> Vec<String> {
        use crate::shell::personal_tethering_runtime::TetheringPhase;
        let pair = |allow: &str, decline: &str| vec![t(allow), t(decline)];
        match item {
            DeviceFabricPaneRow::CalibrationProposal(_) => {
                pair("a11y-placement-apply", "a11y-placement-dismiss")
            }
            DeviceFabricPaneRow::RemoteAudioConsent(_) => {
                pair("a11y-remote-audio-allow", "a11y-remote-audio-decline")
            }
            DeviceFabricPaneRow::SpeakerConsent(consent) => {
                vec![consent.allow_label(), consent.decline_label()]
            }
            DeviceFabricPaneRow::RemoteCaptureConsent(_) => {
                pair("a11y-remote-capture-allow", "a11y-remote-capture-decline")
            }
            DeviceFabricPaneRow::RealtimeConsent(_) => pair("a11y-call-allow", "a11y-call-decline"),
            DeviceFabricPaneRow::GamepadAuthorization(_) => {
                pair("a11y-controller-allow", "a11y-controller-decline")
            }
            DeviceFabricPaneRow::BluetoothAuthorization(_) => {
                pair("a11y-bluetooth-pair-allow", "a11y-bluetooth-pair-decline")
            }
            DeviceFabricPaneRow::Tethering(status)
                if status.phase == TetheringPhase::ConsentRequired =>
            {
                pair("a11y-tethering-allow", "a11y-tethering-decline")
            }
            DeviceFabricPaneRow::LocalInput(input) if input.is_default => {
                pair("a11y-input-lower", "a11y-input-raise")
            }
            DeviceFabricPaneRow::Device(device) => vec![
                t("qs-devices-place-this-left"),
                t("qs-devices-place-this-right"),
                t("qs-devices-place-this-above"),
                t("qs-devices-place-this-below"),
                if device.controls_local {
                    t("a11y-control-revoke")
                } else {
                    t("a11y-control-allow")
                },
            ],
            _ => Vec::new(),
        }
    }
}
