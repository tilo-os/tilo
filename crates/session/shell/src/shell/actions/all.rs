//! Every action the shell offers, each with its declaration.

use super::*;

impl Action {
    pub(crate) const ALL: &'static [Action] = &[
        chrome(
            "dictation.toggle",
            "shortcut-name-dictation-toggle",
            "dictation-action-start-detail",
            "search-kw-dictation-toggle",
            |st, _| request_window_shortcut(st, ShortcutAction::DictationToggle),
        ),
        chrome(
            "dictation.finish",
            "shortcut-name-dictation-finish",
            "dictation-action-finish-detail",
            "search-kw-dictation-finish",
            |st, _| request_window_shortcut(st, ShortcutAction::DictationFinish),
        ),
        chrome(
            "dictation.cancel",
            "shortcut-name-dictation-cancel",
            "dictation-action-cancel-detail",
            "search-kw-dictation-cancel",
            |st, _| request_window_shortcut(st, ShortcutAction::DictationCancel),
        ),
        // "emoji" or a lone ":" in search: the same picker Super+. opens.
        chrome(
            "emoji.picker",
            "search-action-emoji-picker",
            "search-action-emoji-picker-subtitle",
            "search-kw-emoji-picker",
            |st, _| request_window_shortcut(st, ShortcutAction::EmojiPicker),
        ),
        // The same switch Super+Alt+Space makes, on the keyboard last typed on.
        chrome(
            "input.source.next",
            "shortcut-name-next-input-source",
            "search-action-next-input-source-subtitle",
            "search-kw-input-source-next",
            |st, _| {
                st.switch_to_next_input_source()
                    .map(drop)
                    .ok_or_else(|| ActionError::Explained(t("input-source-next-none")))
            },
        ),
        // The steps Super+Alt+8, = and - take, so the magnifier is reachable
        // from the palette, an agent and a screen reader, not only by chord.
        chrome(
            "magnifier.toggle",
            "search-action-magnifier-toggle",
            "search-action-magnifier-toggle-subtitle",
            "search-kw-magnifier-toggle",
            |st, _| request_window_shortcut(st, ShortcutAction::ToggleMagnifier),
        ),
        chrome(
            "magnifier.zoom-in",
            "search-action-magnifier-zoom-in",
            "search-action-magnifier-zoom-in-subtitle",
            "search-kw-magnifier-zoom-in",
            |st, _| request_window_shortcut(st, ShortcutAction::MagnifierZoomIn),
        ),
        chrome(
            "magnifier.zoom-out",
            "search-action-magnifier-zoom-out",
            "search-action-magnifier-zoom-out-subtitle",
            "search-kw-magnifier-zoom-out",
            |st, _| request_window_shortcut(st, ShortcutAction::MagnifierZoomOut),
        ),
        chrome(
            "navigation.axis.enable",
            "search-action-axis-enable",
            "search-action-axis-enable-subtitle",
            "search-kw-navigation-axis-enable",
            |st, _| {
                st.set_axis_navigation_enabled(true);
                Ok(())
            },
        )
        .view_step()
        .toggles(),
        chrome(
            "navigation.axis.disable",
            "search-action-axis-disable",
            "search-action-axis-disable-subtitle",
            "search-kw-navigation-axis-disable",
            |st, _| {
                st.set_axis_navigation_enabled(false);
                Ok(())
            },
        )
        .view_step()
        .toggles(),
        chrome(
            "navigation.axis.gather",
            "search-action-axis-gather",
            "search-action-axis-gather-subtitle",
            "search-kw-navigation-axis-gather",
            |st, _| {
                if !st.axis_navigation_enabled() {
                    return Err(ActionError::Explained(t("axis-navigation-disabled")));
                }
                active_workspace_id(st)?;
                st.request_axis_gather();
                Ok(())
            },
        )
        .view_step(),
        chrome(
            "feedback.open",
            "search-action-feedback-open",
            "search-action-feedback-open-subtitle",
            "search-kw-feedback-open",
            |st, _| crate::shell::feedback_entry::open(st),
        ),
        chrome(
            "feedback.point",
            "search-action-feedback-point",
            "search-action-feedback-point-subtitle",
            "search-kw-feedback-point",
            |st, _| crate::shell::feedback_entry::point(st),
        ),
        chrome(
            "overview.toggle",
            "search-action-overview-toggle",
            "search-action-overview-toggle-subtitle",
            "search-kw-overview-toggle",
            |st, _| {
                st.overlays.overview = !st.overlays.overview;
                Ok(())
            },
        )
        .view_step(),
        chrome(
            "settings.open",
            "search-action-settings-open",
            "search-action-settings-open-subtitle",
            "search-kw-settings-open",
            |st, _| {
                st.open_workspace_settings(tilo_desktop::workspace_settings::WsTab::General);
                Ok(())
            },
        )
        .view(),
        chrome(
            "settings.members.open",
            "search-action-settings-members-open",
            "search-action-settings-members-open-subtitle",
            "search-kw-settings-members-open",
            |st, _| {
                st.open_workspace_settings(tilo_desktop::workspace_settings::WsTab::Members);
                Ok(())
            },
        )
        .view(),
        chrome(
            "settings.privacy.open",
            "search-action-settings-privacy-open",
            "search-action-settings-privacy-open-subtitle",
            "search-kw-settings-privacy-open",
            |st, _| {
                st.open_workspace_settings(tilo_desktop::workspace_settings::WsTab::Permissions);
                Ok(())
            },
        )
        .view(),
        chrome(
            "settings.apps.open",
            "search-action-settings-apps-open",
            "search-action-settings-apps-open-subtitle",
            "search-kw-settings-apps-open",
            |st, _| {
                st.open_workspace_settings(tilo_desktop::workspace_settings::WsTab::Apps);
                Ok(())
            },
        )
        .view(),
        Action {
            id: "identity.connect",
            label: "search-action-identity-connect",
            subtitle: "search-action-identity-connect-subtitle",
            keywords: "search-kw-identity-connect",
            params: IDENTITY_CONNECT_PARAMS,
            apply: |st, args| {
                require_explicit_confirmation(args, "connect this identity")?;
                let path = args.text("path").unwrap_or_default().trim();
                if path.is_empty() {
                    return Err(ActionError::Refused("identity path is required".into()));
                }
                let (workspace_id, room) = active_admin_workspace(st)?;
                let store = crate::identity_registry::IdentityRegistryStore::from_env()
                    .map_err(|error| ActionError::Refused(error.to_string()))?;
                let profile = store
                    .import(std::path::Path::new(path))
                    .map_err(|error| ActionError::Refused(error.to_string()))?;
                let identity_paths = crate::firstboot::collaboration_identity_paths();
                crate::firstboot::ensure_device_certificate_for_record(
                    &profile.record_path,
                    &identity_paths,
                )
                .map_err(|error| ActionError::Refused(error.to_string()))?;
                crate::device_fleet::DeviceFleetStore::at(store.identity_dir(), identity_paths)
                    .register_current_device(&profile.id)
                    .map_err(|error| ActionError::Refused(error.to_string()))?;
                store
                    .bind_workspace(&room.to_string(), &profile.id)
                    .map_err(|error| ActionError::Refused(error.to_string()))?;
                fence_native_selection(st, workspace_id, room, &profile.user_public_key)?;
                st.tree().export_fs_capabilities();
                st.bump();
                Ok(())
            },
            ..Action::NEW
        }
        .privileged(),
        Action {
            id: "identity.select",
            label: "search-action-identity-select",
            subtitle: "search-action-identity-select-subtitle",
            keywords: "search-kw-identity-select",
            params: IDENTITY_SELECT_PARAMS,
            apply: |st, args| {
                let identity_id = args.text("id").unwrap_or_default().trim();
                if identity_id == IDENTITY_SETUP_CHOICE {
                    return request_identity_setup(st);
                }
                require_explicit_confirmation(args, "select this identity")?;
                let (workspace_id, room) = active_admin_workspace(st)?;
                let store = crate::identity_registry::IdentityRegistryStore::from_env()
                    .map_err(|error| ActionError::Refused(error.to_string()))?;
                let registry = store
                    .load_reconciled()
                    .map_err(|error| ActionError::Refused(error.to_string()))?;
                let profile = registry.identities.get(identity_id).ok_or_else(|| {
                    ActionError::Refused("that identity is not connected to this device".into())
                })?;
                store
                    .bind_workspace(&room.to_string(), identity_id)
                    .map_err(|error| ActionError::Refused(error.to_string()))?;
                fence_native_selection(st, workspace_id, room, &profile.user_public_key)?;
                st.tree().export_fs_capabilities();
                st.bump();
                Ok(())
            },
            ..Action::NEW
        }
        .privileged(),
        chrome(
            "identity.devices.copy",
            "search-action-identity-devices-copy",
            "search-action-identity-devices-copy-subtitle",
            "search-kw-identity-devices-copy",
            |st, _| {
                let (_, profile) = active_identity(st)?;
                let fleet = crate::device_fleet::DeviceFleetStore::from_env()
                    .and_then(|store| store.load(&profile.id))
                    .map_err(|error| ActionError::Refused(error.to_string()))?;
                let lines = fleet
                    .certificates
                    .iter()
                    .map(|cert| {
                        let fingerprint = hex::encode(cert.device_public_key);
                        format!(
                            "{}  {}  {}",
                            cert.device_label,
                            fingerprint,
                            if fleet.is_revoked(&cert.device_public_key) {
                                "revoked"
                            } else {
                                "active"
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                // The clipboard is out of sight and search closes on choose,
                // so confirm in passing rather than with a notification.
                ui::text_selection::clipboard_copy(&lines);
                st.acknowledge(t("search-action-device-list-copied"));
                Ok(())
            },
        )
        .outside(),
        Action {
            id: enrollment_tokens::DEVICE_ENROLL_ID,
            label: "search-action-identity-device-enroll",
            subtitle: "search-action-identity-device-enroll-subtitle",
            keywords: "search-kw-identity-device-enroll",
            params: enrollment_tokens::ENROLL_PARAMS,
            apply: |st, args| {
                enrollment_tokens::mint_for_person(
                    st,
                    args,
                    tilo_identity_host::enrollment::EnrollmentRole::PersonalDevice,
                )
            },
            ..Action::NEW
        }
        .declared(|_| enrollment_tokens::contract()),
        Action {
            id: enrollment_tokens::HOME_PEER_ENROLL_ID,
            label: "search-action-identity-home-peer-enroll",
            subtitle: "search-action-identity-home-peer-enroll-subtitle",
            keywords: "search-kw-identity-home-peer-enroll",
            params: enrollment_tokens::ENROLL_PARAMS,
            apply: |st, args| {
                enrollment_tokens::mint_for_person(
                    st,
                    args,
                    tilo_identity_host::enrollment::EnrollmentRole::HomePeer,
                )
            },
            ..Action::NEW
        }
        .declared(|_| enrollment_tokens::contract()),
        Action {
            id: device_pairing::DISCOVER_ID,
            label: "search-action-identity-device-pair-discover",
            subtitle: "search-action-identity-device-pair-discover-subtitle",
            keywords: "search-kw-identity-device-pair-discover",
            params: device_pairing::DISCOVER_PARAMS,
            apply: |_, _| Err(pairing_needs_lifecycle()),
            ..Action::NEW
        }
        .declared(device_pairing::contract),
        Action {
            id: device_pairing::APPROVE_ID,
            label: "search-action-identity-device-pair-approve",
            subtitle: "search-action-identity-device-pair-approve-subtitle",
            keywords: "search-kw-identity-device-pair-approve",
            params: device_pairing::APPROVE_PARAMS,
            apply: |_, _| Err(pairing_needs_lifecycle()),
            ..Action::NEW
        }
        .declared(device_pairing::contract),
        Action {
            id: device_pairing::REJECT_ID,
            label: "search-action-identity-device-pair-reject",
            subtitle: "search-action-identity-device-pair-reject-subtitle",
            keywords: "search-kw-identity-device-pair-reject",
            params: device_pairing::REJECT_PARAMS,
            apply: |_, _| Err(pairing_needs_lifecycle()),
            ..Action::NEW
        }
        .declared(device_pairing::contract),
        Action {
            id: device_pairing::ASKING_ID,
            label: "search-action-identity-device-pair-asking",
            subtitle: "search-action-identity-device-pair-asking-subtitle",
            keywords: "search-kw-identity-device-pair-asking",
            params: device_pairing::ASKING_PARAMS,
            apply: |_, _| Err(pairing_needs_lifecycle()),
            ..Action::NEW
        }
        .declared(device_pairing::contract),
        Action {
            id: device_pairing::ANSWER_ID,
            label: "search-action-identity-device-pair-answer",
            subtitle: "search-action-identity-device-pair-answer-subtitle",
            keywords: "search-kw-identity-device-pair-answer",
            params: device_pairing::ANSWER_PARAMS,
            apply: |_, _| Err(pairing_needs_lifecycle()),
            ..Action::NEW
        }
        .declared(device_pairing::contract),
        Action {
            id: identity_admin::LIST_ID,
            label: "search-action-identity-list",
            subtitle: "search-action-identity-list-subtitle",
            keywords: "search-kw-identity-list",
            params: crate::shell::identity_admin::LIST_PARAMS,
            apply: |_, _| Err(identity_needs_lifecycle()),
            ..Action::NEW
        }
        .declared(identity_admin::contract),
        Action {
            id: identity_admin::ACTIVATE_ID,
            label: "search-action-identity-activate",
            subtitle: "search-action-identity-activate-subtitle",
            keywords: "search-kw-identity-activate",
            params: crate::shell::identity_admin::ACTIVATE_PARAMS,
            apply: |_, _| Err(identity_needs_lifecycle()),
            ..Action::NEW
        }
        .declared(identity_admin::contract),
        Action {
            id: identity_admin::REMOVE_ID,
            label: "search-action-identity-remove",
            subtitle: "search-action-identity-remove-subtitle",
            keywords: "search-kw-identity-remove",
            params: crate::shell::identity_admin::REMOVE_PARAMS,
            apply: |_, _| Err(identity_needs_lifecycle()),
            ..Action::NEW
        }
        .declared(identity_admin::contract),
        Action {
            id: identity_admin::PROTECT_ID,
            label: "search-action-identity-protect",
            subtitle: "search-action-identity-protect-subtitle",
            keywords: "search-kw-identity-protect",
            params: crate::shell::identity_admin::PROTECT_PARAMS,
            apply: |_, _| Err(identity_needs_lifecycle()),
            ..Action::NEW
        }
        .declared(identity_admin::contract),
        Action {
            id: identity_admin::DEVICES_ID,
            label: "search-action-identity-devices-list",
            subtitle: "search-action-identity-devices-list-subtitle",
            keywords: "search-kw-identity-devices-list",
            params: crate::shell::identity_admin::DEVICES_PARAMS,
            apply: |_, _| Err(identity_needs_lifecycle()),
            ..Action::NEW
        }
        .declared(identity_admin::contract),
        Action {
            id: identity_admin::HOME_PEER_REMOVE_ID,
            label: "search-action-identity-home-peer-remove",
            subtitle: "search-action-identity-home-peer-remove-subtitle",
            keywords: "search-kw-identity-home-peer-remove",
            params: crate::shell::identity_admin::HOME_PEER_REMOVE_PARAMS,
            apply: |_, _| Err(identity_needs_lifecycle()),
            ..Action::NEW
        }
        .declared(identity_admin::contract),
        Action {
            id: identity_admin::HOME_PEER_RETRY_ID,
            label: "search-action-identity-home-peer-retry",
            subtitle: "search-action-identity-home-peer-retry-subtitle",
            keywords: "search-kw-identity-home-peer-retry-removals",
            params: crate::shell::identity_admin::HOME_PEER_RETRY_PARAMS,
            apply: |_, _| Err(identity_needs_lifecycle()),
            ..Action::NEW
        }
        .declared(identity_admin::contract),
        Action {
            id: "identity.device.revoke",
            label: "search-action-identity-device-revoke",
            subtitle: "search-action-identity-device-revoke-subtitle",
            keywords: "search-kw-identity-device-revoke",
            params: DEVICE_REVOKE_PARAMS,
            apply: |st, args| {
                if args.boolean("confirm") != Some(true) {
                    return Err(ActionError::Refused(
                        "explicit confirmation is required to revoke this device".into(),
                    ));
                }
                let (_, profile) = active_identity(st)?;
                let store = crate::shell::identity_admin::Store::from_env()
                    .map_err(ActionError::Refused)?;
                crate::shell::identity_admin::revoke(&store, &profile, args)
                    .map_err(ActionError::Refused)?;
                // Revoking this machine must immediately withdraw any
                // device-key filesystem grants exported for it.
                st.tree().export_fs_capabilities();
                st.push_notification(
                    t("search-action-device-revoked"),
                    t("search-action-device-revoked-body"),
                );
                Ok(())
            },
            ..Action::NEW
        }
        .privileged(),
        chrome(
            "quick-settings.open",
            "search-action-quick-settings-open",
            "search-action-quick-settings-open-subtitle",
            "search-kw-quick-settings-open",
            |st, _| {
                st.open_quick_settings();
                Ok(())
            },
        )
        .view(),
        chrome(
            "quick-settings.clipboard",
            "search-action-quick-settings-clipboard",
            "search-action-quick-settings-clipboard-subtitle",
            "search-kw-quick-settings-clipboard",
            |st, _| {
                st.open_quick_settings_section(tilo_quick_settings::QsSection::Clipboard);
                Ok(())
            },
        )
        .view(),
        chrome(
            "quick-settings.network",
            "qs-network-details",
            "search-action-quick-settings-network-subtitle",
            "search-kw-quick-settings-network",
            |st, _| {
                st.open_quick_settings_section(tilo_quick_settings::QsSection::Wifi);
                Ok(())
            },
        )
        .view(),
        chrome(
            "quick-settings.bluetooth",
            "qs-bluetooth-details",
            "search-action-quick-settings-bluetooth-subtitle",
            "search-kw-quick-settings-bluetooth",
            |st, _| {
                st.open_quick_settings_section(tilo_quick_settings::QsSection::Bluetooth);
                Ok(())
            },
        )
        .view(),
        chrome(
            "quick-settings.devices",
            "search-action-quick-settings-devices",
            "search-action-quick-settings-devices-subtitle",
            "search-kw-quick-settings-devices",
            |st, _| {
                st.open_quick_settings_section(tilo_quick_settings::QsSection::Devices);
                Ok(())
            },
        )
        .view(),
        chrome(
            "projection.open",
            "search-action-projection-open",
            "search-action-projection-open-subtitle",
            "search-kw-projection-open",
            |st, _| {
                st.open_projection();
                Ok(())
            },
        )
        .view(),
        chrome(
            "projection.close",
            "search-action-projection-close",
            "search-action-projection-close-subtitle",
            "search-kw-projection-close",
            |st, _| {
                st.close_projection();
                Ok(())
            },
        )
        .view(),
        chrome(
            shared_display_actions::INSPECT_ID,
            "search-action-shared-display-inspect",
            "search-action-shared-display-inspect-subtitle",
            "search-kw-display-shared-inspect",
            |_, _| Ok(()),
        )
        .view(),
        Action {
            id: shared_display_actions::INCLUDE_ID,
            label: "search-action-shared-display-include",
            subtitle: "search-action-shared-display-include-subtitle",
            keywords: "search-kw-display-shared-include",
            params: shared_display_actions::INCLUDE_PARAMS,
            apply: |state, args| {
                let revision = args
                    .integer("revision")
                    .and_then(|value| u64::try_from(value).ok())
                    .ok_or_else(|| ActionError::Explained(t("display-remote-stale")))?;
                shared_display_actions::include(
                    state,
                    args.text("device").unwrap_or_default(),
                    args.text("output").unwrap_or_default(),
                    revision,
                    args.boolean("included").unwrap_or(false),
                )
            },
            ..Action::NEW
        }
        .display_layout(),
        chrome(
            DISPLAY_EXTEND_ID,
            "search-action-display-layout-extend",
            "search-action-display-layout-extend-subtitle",
            "search-kw-display-layout-extend",
            |st, _| stage_display_layout(st, DisplayLayoutMode::Extend),
        )
        .display_layout(),
        chrome(
            DISPLAY_MIRROR_ID,
            "search-action-display-layout-mirror",
            "search-action-display-layout-mirror-subtitle",
            "search-kw-display-layout-mirror",
            |st, _| stage_display_layout(st, DisplayLayoutMode::Mirror),
        )
        .display_layout(),
        chrome(
            DISPLAY_BUILTIN_ONLY_ID,
            "search-action-display-layout-builtin-only",
            "search-action-display-layout-builtin-only-subtitle",
            "search-kw-display-layout-builtin-only",
            |st, _| stage_display_layout(st, DisplayLayoutMode::BuiltinOnly),
        )
        .display_layout(),
        chrome(
            DISPLAY_EXTERNAL_ONLY_ID,
            "search-action-display-layout-external-only",
            "search-action-display-layout-external-only-subtitle",
            "search-kw-display-layout-external-only",
            |st, _| stage_display_layout(st, DisplayLayoutMode::ExternalOnly),
        )
        .display_layout(),
        chrome(
            DISPLAY_OUTPUTS_INSPECT_ID,
            "search-action-display-outputs-inspect",
            "search-action-display-outputs-inspect-subtitle",
            "search-kw-display-outputs-inspect",
            |_, _| Ok(()),
        )
        .view(),
        chrome(
            DISPLAY_KEEP_ID,
            "search-action-display-change-keep",
            "search-action-display-change-keep-subtitle",
            "search-kw-display-change-keep",
            |_, _| settle_display_change(true),
        )
        .display_layout(),
        chrome(
            DISPLAY_REVERT_ID,
            "search-action-display-change-revert",
            "search-action-display-change-revert-subtitle",
            "search-kw-display-change-revert",
            |_, _| settle_display_change(false),
        )
        .display_layout(),
        chrome(
            BLUETOOTH_PAIRING_DECLINE_ID,
            "search-action-bluetooth-pairing-decline",
            "search-action-bluetooth-pairing-decline-subtitle",
            "search-kw-bluetooth-pairing-decline",
            |_, _| decline_bluetooth_pairing(),
        ),
        chrome(
            "capture.open",
            "search-action-capture-open",
            "search-action-capture-open-subtitle",
            "search-kw-capture-open",
            |st, _| {
                st.overlays.capture = true;
                Ok(())
            },
        )
        .view(),
        Action {
            id: "capture.screenshot.send",
            label: "search-action-capture-screenshot-send",
            subtitle: "search-action-capture-screenshot-send-subtitle",
            keywords: "search-kw-capture-screenshot-send",
            params: SCREENSHOT_SEND_PARAMS,
            apply: |st, args| {
                let target = args.text("target").unwrap_or_default().trim();
                if target.is_empty() {
                    return Err(ActionError::Refused(
                        "name `workspace` or one of your online devices".into(),
                    ));
                }
                st.push_screenshot_send(if target == "workspace" {
                    ui::widget::ScreenshotSendTarget::Workspace
                } else {
                    ui::widget::ScreenshotSendTarget::Device(target.to_string())
                });
                Ok(())
            },
            ..Action::NEW
        },
        chrome(
            "collaborators.open",
            "search-action-collaborators-open",
            "search-action-collaborators-open-subtitle",
            "search-kw-collaborators-open",
            |st, _| {
                st.overlays.collaborators = true;
                Ok(())
            },
        )
        .view(),
        Action {
            id: "presence.follow",
            label: "search-action-presence-follow",
            subtitle: "search-action-presence-follow-subtitle",
            keywords: "search-kw-presence-follow",
            params: PRESENCE_FOLLOW_PARAMS,
            apply: |st, args| {
                let id = args.text("collaborator").unwrap_or_default().trim();
                let followable = tilo_desktop::collaborators::collaborator_roster_rows(
                    &st.tree().desktop,
                    &crate::shell::state::local_user_id(),
                )
                .into_iter()
                .any(|row| row.id == id && row.followable);
                if !followable {
                    return Err(ActionError::Refused(format!(
                        "`{id}` is not a connected, followable collaborator in this workspace"
                    )));
                }
                st.push_presence_request(crate::shell::chrome_state::PresenceRequest::Follow(
                    id.to_string(),
                ));
                Ok(())
            },
            ..Action::NEW
        },
        chrome(
            "presence.follow.stop",
            "search-action-presence-follow-stop",
            "search-action-presence-follow-stop-subtitle",
            "search-kw-presence-follow-stop",
            |st, _| {
                st.push_presence_request(
                    crate::shell::chrome_state::PresenceRequest::StopFollowing,
                );
                Ok(())
            },
        ),
        Action {
            id: "presence.status.set",
            label: "search-action-presence-status-set",
            subtitle: "search-action-presence-status-set-subtitle",
            keywords: "search-kw-presence-status-set",
            params: PRESENCE_STATUS_PARAMS,
            apply: |st, args| {
                let text = args.text("text").unwrap_or_default().trim();
                if text.chars().count() > ui::live_collaboration::MAX_STATUS_CHARS {
                    return Err(ActionError::Refused(format!(
                        "status must be at most {} characters",
                        ui::live_collaboration::MAX_STATUS_CHARS
                    )));
                }
                st.push_presence_request(
                    crate::shell::chrome_state::PresenceRequest::SetManualStatus(
                        (!text.is_empty()).then(|| text.to_string()),
                    ),
                );
                Ok(())
            },
            ..Action::NEW
        },
        Action {
            id: "presence.status-sharing.set",
            label: "search-action-presence-status-sharing-set",
            subtitle: "search-action-presence-status-sharing-set-subtitle",
            keywords: "search-kw-presence-status-sharing-set",
            params: PRESENCE_SHARING_PARAMS,
            apply: |st, args| {
                let mode = ui::StatusSharing::from_word(
                    &args
                        .text("mode")
                        .unwrap_or_default()
                        .trim()
                        .to_ascii_lowercase(),
                )
                .ok_or_else(|| {
                    ActionError::Refused("status privacy must be off, manual, or automatic".into())
                })?;
                st.push_presence_request(
                    crate::shell::chrome_state::PresenceRequest::SetStatusSharing(mode),
                );
                Ok(())
            },
            ..Action::NEW
        },
        Action {
            id: "presence.wave",
            label: "search-action-presence-wave",
            subtitle: "search-action-presence-wave-subtitle",
            keywords: "search-kw-presence-wave",
            params: PRESENCE_FOLLOW_PARAMS,
            apply: |st, args| {
                let id = args.text("collaborator").unwrap_or_default().trim();
                let here = tilo_desktop::collaborators::collaborator_roster_rows(
                    &st.tree().desktop,
                    &crate::shell::state::local_user_id(),
                )
                .into_iter()
                .any(|row| row.id == id && !row.invited);
                if !here {
                    return Err(ActionError::Refused(format!(
                        "`{id}` is not here in this workspace"
                    )));
                }
                st.push_presence_request(crate::shell::chrome_state::PresenceRequest::Wave(
                    id.to_string(),
                ));
                Ok(())
            },
            ..Action::NEW
        },
        Action {
            id: "presence.availability.set",
            label: "search-action-presence-availability-set",
            subtitle: "search-action-presence-availability-set-subtitle",
            keywords: "search-kw-presence-availability-set",
            params: PRESENCE_AVAILABILITY_PARAMS,
            apply: |st, args| {
                let chosen = match args
                    .text("availability")
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "available" => ui::Availability::Available,
                    "focusing" => ui::Availability::Focusing,
                    "away" => ui::Availability::Away,
                    _ => {
                        return Err(ActionError::Refused(
                            "availability must be available, focusing, or away".into(),
                        ));
                    }
                };
                st.push_presence_request(
                    crate::shell::chrome_state::PresenceRequest::SetAvailability(chosen),
                );
                Ok(())
            },
            ..Action::NEW
        },
        Action {
            id: "presence.visibility.set",
            label: "search-action-presence-visibility-set",
            subtitle: "search-action-presence-visibility-set-subtitle",
            keywords: "search-kw-presence-visibility-set",
            params: PRESENCE_VISIBILITY_PARAMS,
            apply: |st, args| {
                let choice = match args
                    .text("visibility")
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "where" => ui::PresenceVisibility::WhereIAm,
                    "here" => ui::PresenceVisibility::ThatImHere,
                    "nothing" => ui::PresenceVisibility::Nothing,
                    _ => {
                        return Err(ActionError::Refused(
                            "visibility must be where, here, or nothing".into(),
                        ));
                    }
                };
                let Some(room) = st
                    .tree()
                    .desktop
                    .active_workspace()
                    .map(crate::shell::state::Workspace::room)
                else {
                    return Err(ActionError::Refused("no workspace is open".into()));
                };
                st.push_presence_request(
                    crate::shell::chrome_state::PresenceRequest::SetVisibility(room, choice),
                );
                Ok(())
            },
            ..Action::NEW
        },
        chrome(
            "room.activity.open",
            "search-action-room-activity-open",
            "search-action-room-activity-open-subtitle",
            "search-kw-room-activity-open",
            |st, _| {
                st.open_room_activity(None);
                Ok(())
            },
        )
        .view(),
        // Ranked like any other action, not appended after the hits — an
        // un-ranked row would lose "invite" to a file with the word in its name.
        chrome(
            "peer.invite",
            "search-action-peer-invite",
            "search-action-peer-invite-subtitle",
            "search-kw-peer-invite",
            |st, _| {
                st.push_peer_sync_request(crate::shell::overlay_host::PeerSyncAction::PromptInvite);
                Ok(())
            },
        )
        .outside_view(),
        // "Join a workspace…" opens the directory of public workspaces,
        // where a link or a code joins too.
        chrome(
            "peer.connect",
            "search-action-peer-connect",
            "search-action-peer-connect-subtitle",
            "search-kw-peer-connect",
            |st, _| {
                st.push_peer_sync_request(
                    crate::shell::overlay_host::PeerSyncAction::BrowseDirectory,
                );
                Ok(())
            },
        )
        .outside_view(),
        chrome(
            "peer.pair-device",
            "search-action-pair-device",
            "search-action-pair-device-subtitle",
            "search-kw-peer-pair-device",
            |st, _| {
                st.push_peer_sync_request(crate::shell::overlay_host::PeerSyncAction::PairDevice);
                Ok(())
            },
        )
        .outside_view(),
        // The name must come from the inviter, not the invitee: a peer
        // announcing its own name could claim any name and have it render
        // next to a caret this room's capabilities gate. The invitation the
        // room's host issues carries it; the peer id stays the identity,
        // the name is just a label.
        Action {
            id: "peer.invite.named",
            label: "search-action-peer-invite-named",
            subtitle: "search-action-peer-invite-named-subtitle",
            keywords: "search-kw-peer-invite-named",
            params: PEER_INVITE_NAMED_PARAMS,
            apply: |st, args| {
                let name = name_invited_peer(args)?;
                // Opens the invite composer prefilled with the name for the
                // person to review. Nothing is minted here:
                // `invitation.issue` is the action that mints a token.
                st.push_peer_sync_request(
                    crate::shell::overlay_host::PeerSyncAction::IssueInviteFor {
                        name,
                        copy: false,
                    },
                );
                Ok(())
            },
            ..Action::NEW
        }
        .outside()
        .document(),
        // Public workspaces: the active one listed by its host with a
        // description and a default role, or private again; one joined
        // from a directory reported to whoever runs it.
        Action {
            id: WORKSPACE_PUBLISH_ID,
            label: "search-action-workspace-publish",
            subtitle: "search-action-workspace-publish-subtitle",
            keywords: "search-kw-workspace-publish",
            params: WORKSPACE_PUBLISH_PARAMS,
            apply: |st, args| {
                let listing = publish_listing(st, args)?;
                st.publish_active_workspace(Some(listing));
                st.bump();
                Ok(())
            },
            ..Action::NEW
        }
        .privileged(),
        Action {
            id: WORKSPACE_UNPUBLISH_ID,
            label: "search-action-workspace-unpublish",
            subtitle: "search-action-workspace-unpublish-subtitle",
            keywords: "search-kw-workspace-unpublish",
            params: &[],
            apply: |st, _| {
                st.publish_active_workspace(None);
                st.bump();
                Ok(())
            },
            ..Action::NEW
        }
        .privileged(),
        // Bots of the active workspace: asked of its host, which runs
        // what the room's governance lets this person ask for.
        Action {
            id: BOT_ADD_ID,
            label: "search-action-bot-add",
            subtitle: "search-action-bot-add-subtitle",
            keywords: "search-kw-bot-add",
            params: BOT_ADD_PARAMS,
            apply: |st, args| {
                require_explicit_confirmation(args, "add this bot")?;
                let ask = bot_ask(args, |entry, version| crate::shell::network::BotAsk::Add {
                    entry,
                    version: version.unwrap_or_default(),
                })?;
                if !st.ask_active_workspace_for_bot(ask) {
                    return Err(ActionError::Refused("no active workspace".into()));
                }
                st.bump();
                Ok(())
            },
            ..Action::NEW
        }
        .privileged(),
        Action {
            id: APP_UNINSTALL_ID,
            label: "search-action-app-uninstall",
            subtitle: "search-action-app-uninstall-subtitle",
            keywords: "search-kw-app-uninstall",
            params: APP_UNINSTALL_PARAMS,
            apply: |_, args| uninstall_app(args, false),
            ..Action::NEW
        }
        .destructive(),
        Action {
            id: APP_REMOVE_FROM_APPS_ID,
            label: "search-action-app-remove-from-apps",
            subtitle: "search-action-app-remove-from-apps-subtitle",
            keywords: "search-kw-app-remove-from-apps",
            params: APP_REMOVE_FROM_APPS_PARAMS,
            apply: |_, args| uninstall_app(args, true),
            ..Action::NEW
        }
        .destructive(),
        Action {
            id: BOT_REMOVE_ID,
            label: "search-action-bot-remove",
            subtitle: "search-action-bot-remove-subtitle",
            keywords: "search-kw-bot-remove",
            params: BOT_REMOVE_PARAMS,
            apply: |st, args| {
                require_explicit_confirmation(args, "remove this bot")?;
                let ask = bot_ask(args, |entry, _| crate::shell::network::BotAsk::Remove {
                    entry,
                })?;
                if !st.ask_active_workspace_for_bot(ask) {
                    return Err(ActionError::Refused("no active workspace".into()));
                }
                st.bump();
                Ok(())
            },
            ..Action::NEW
        }
        .destructive(),
        Action {
            id: WORKSPACE_REPORT_ID,
            label: "search-action-workspace-report",
            subtitle: "search-action-workspace-report-subtitle",
            keywords: "search-kw-workspace-report",
            params: WORKSPACE_REPORT_PARAMS,
            apply: |st, args| {
                let reason = args.text("reason").unwrap_or_default().trim().to_owned();
                if reason.is_empty() {
                    return Err(ActionError::Refused("a report says what is wrong".into()));
                }
                let room = st
                    .tree()
                    .desktop
                    .active_workspace()
                    .map(crate::shell::state::Workspace::room)
                    .ok_or_else(|| ActionError::Refused("no active workspace".into()))?;
                st.push_peer_sync_request(
                    crate::shell::overlay_host::PeerSyncAction::ReportWorkspace { room, reason },
                );
                Ok(())
            },
            ..Action::NEW
        }
        .privileged(),
        // Workspace membership. Each passes the shell action gate and then
        // `crate::shell::membership`, the function the Members tab calls.
        Action {
            id: MEMBER_ROLE_SET_ID,
            label: "search-action-member-role-set",
            subtitle: "search-action-member-role-set-subtitle",
            keywords: "search-kw-member-role-set",
            params: membership_actions::MEMBER_ROLE_SET_PARAMS,
            apply: apply_member_role_set,
            ..Action::NEW
        }
        .declared(collaboration_contract),
        Action {
            id: MEMBER_REMOVE_ID,
            label: "search-action-member-remove",
            subtitle: "search-action-member-remove-subtitle",
            keywords: "search-kw-member-remove",
            params: membership_actions::MEMBER_REMOVE_PARAMS,
            apply: apply_member_remove,
            ..Action::NEW
        }
        .declared(collaboration_contract),
        // Invitations run through the peer-sync network on the overlay
        // host's frame pump, so they are dispatched as pending invocations
        // (see `build_catalogue`); `apply` is never their entry point.
        Action {
            id: INVITATION_ISSUE_ID,
            label: "search-action-invitation-issue",
            subtitle: "search-action-invitation-issue-subtitle",
            keywords: "search-kw-invitation-issue",
            params: membership_actions::INVITATION_ISSUE_PARAMS,
            apply: invitation_requires_lifecycle_dispatch,
            ..Action::NEW
        }
        .declared(collaboration_contract),
        Action {
            id: INVITATION_LIST_ID,
            label: "search-action-invitation-list",
            subtitle: "search-action-invitation-list-subtitle",
            keywords: "search-kw-invitation-list",
            params: membership_actions::INVITATION_LIST_PARAMS,
            apply: invitation_requires_lifecycle_dispatch,
            ..Action::NEW
        }
        .declared(collaboration_contract),
        Action {
            id: INVITATION_RESEND_ID,
            label: "search-action-invitation-resend",
            subtitle: "search-action-invitation-resend-subtitle",
            keywords: "search-kw-invitation-resend",
            params: membership_actions::INVITATION_ID_PARAMS,
            apply: invitation_requires_lifecycle_dispatch,
            ..Action::NEW
        }
        .declared(collaboration_contract),
        Action {
            id: INVITATION_CANCEL_ID,
            label: "search-action-invitation-cancel",
            subtitle: "search-action-invitation-cancel-subtitle",
            keywords: "search-kw-invitation-cancel",
            params: membership_actions::INVITATION_ID_PARAMS,
            apply: invitation_requires_lifecycle_dispatch,
            ..Action::NEW
        }
        .declared(collaboration_contract),
        // Alongside the peer rows because the OS treats an agent as an
        // ordinary participant, but not the same operation: a peer presents
        // a signed token; an agent is admitted by the room recording a
        // capability naming it — no token, no prompt, just the grant.
        Action {
            id: "agent.invite",
            label: "search-action-agent-invite",
            subtitle: "search-action-agent-invite-subtitle",
            keywords: "search-kw-agent-invite",
            params: AGENT_INVITE_PARAMS,
            apply: |st, args| {
                use tilo_action_policy::roles::AgentPreset;
                let name = args.text("name").unwrap_or(DEFAULT_AGENT_NAME).trim();
                let name = if name.is_empty() {
                    DEFAULT_AGENT_NAME
                } else {
                    name
                };
                // Case-folded: `Scribe` and `scribe` are otherwise two
                // different subjects, half-admitting the agent.
                let agent = name.to_lowercase();
                let preset = match args.text("preset") {
                    Some(value) => AgentPreset::parse(value).ok_or_else(|| {
                        ActionError::Refused(
                            "choose Observer, Contributor, Operator, or Custom".into(),
                        )
                    })?,
                    None => AgentPreset::Observer,
                };
                let user = st.active_local_user();
                let granted = {
                    let mut tree = st.tree_mut();
                    let Some((ws_id, room, held)) = tree
                        .desktop
                        .active_workspace()
                        .map(|w| (w.id, w.room().to_string(), w.policy.grants.clone()))
                    else {
                        return Err(ActionError::Refused(
                            "there is no workspace here to add an agent to".into(),
                        ));
                    };
                    // Ordinary grants on the room, through the tree so they
                    // reach the room document (and other members) and disk.
                    let changes = preset.admission(
                        &room,
                        &agent,
                        &tilo_action_policy::Subject::person(user.clone()),
                        &held,
                    );
                    tree.govern_in_workspace(ws_id, &user, changes).ok()
                };
                if granted.is_none() {
                    return Err(ActionError::Refused(format!(
                        "{user} does not administer this workspace, so {agent} was not added"
                    )));
                }
                // Land back on the dedicated, clearly non-human roster. The
                // next visible action is Review access, not a chat prompt.
                st.open_workspace_settings(crate::shell::chrome_state::WsTab::Agents);
                Ok(())
            },
            ..Action::NEW
        }
        .outside()
        .document(),
        // The Agents tab's Remove, for every caller: the same function, so
        // a person clicking and a script or screen reader end an agent's
        // access the same way.
        Action {
            id: crate::shell::membership::AGENT_REMOVE_ACTION,
            label: "search-action-agent-remove",
            subtitle: "search-action-agent-remove-subtitle",
            keywords: "search-kw-agent-remove",
            params: AGENT_REMOVE_PARAMS,
            apply: |st, args| {
                // Case-folded like `agent.invite`, which admitted it.
                let agent = args.text("name").unwrap_or_default().trim().to_lowercase();
                if agent.is_empty() {
                    return Err(ActionError::Refused(
                        "name the agent to remove; nothing was changed".into(),
                    ));
                }
                crate::shell::membership::remove_agent(st, &agent).map_err(Into::into)
            },
            ..Action::NEW
        }
        .destructive()
        .document(),
        chrome(
            "tab.new.terminal.here",
            "search-action-tab-new-terminal-here",
            "search-action-tab-new-terminal-here-subtitle",
            "search-kw-tab-new-terminal-here",
            |st, _| {
                crate::shell::window_actions::new_terminal(
                    st,
                    crate::shell::window_actions::Destination::Here,
                )
                .map_err(|error| ActionError::Refused(error.to_string()))
            },
        ),
        chrome(
            "window.new.terminal",
            "search-action-window-new-terminal",
            "search-action-window-new-terminal-subtitle",
            "search-kw-window-new-terminal",
            |st, _| {
                crate::shell::window_actions::new_terminal(
                    st,
                    crate::shell::window_actions::Destination::Beside,
                )
                .map_err(|error| ActionError::Refused(error.to_string()))
            },
        ),
        chrome(
            "tab.duplicate",
            "search-action-tab-duplicate",
            "search-action-tab-duplicate-subtitle",
            "search-kw-tab-duplicate",
            |st, _| {
                crate::shell::window_actions::duplicate_active_tab(st).map_or_else(
                    |err| {
                        let message = err.user_message();
                        st.push_warning_notification(t("host-tab-duplicate-failed"), &message);
                        Err(ActionError::Refused(message))
                    },
                    |_| Ok(()),
                )
            },
        ),
        // Another instance of an app as a tab right after the focused one:
        // what an app's "open in new tab" asks for, open to every caller.
        Action {
            id: "tab.open-beside",
            label: "search-action-tab-open-beside",
            subtitle: "search-action-tab-open-beside-subtitle",
            keywords: "search-kw-tab-open-beside",
            params: TAB_OPEN_BESIDE_PARAMS,
            apply: |st, args| {
                let app = args.text("app").unwrap_or_default().trim().to_owned();
                if app.is_empty() {
                    return Err(ActionError::Refused(
                        "an app is required; nothing was opened".into(),
                    ));
                }
                let target = args
                    .text("target")
                    .map(str::trim)
                    .filter(|target| !target.is_empty())
                    .map(str::to_owned);
                let background = args.boolean("background").unwrap_or(false);
                let anchor = crate::shell::window_actions::active_tab_id(st)
                    .and_then(|tab| crate::shell::app_tabs::anchor_for_tab(&st.tree(), tab));
                crate::shell::app_tabs::open_beside(st, &app, target.as_deref(), anchor, background)
                    .map_err(|error| ActionError::Refused(error.to_string()))
            },
            ..Action::NEW
        },
        // The tab menu's three rows as actions, on the focused tab: a
        // capability reachable only by right-click is not finished. The
        // rows and these closures call the same `window_actions` functions.
        chrome(
            "tab.menu",
            "search-action-tab-menu",
            "search-action-tab-menu-subtitle",
            "search-kw-tab-menu",
            |st, _| {
                if st.session_locked() || crate::shell::window_actions::active_tab_id(st).is_none()
                {
                    return Err(ActionError::Explained(t(
                        "search-action-tab-menu-unavailable",
                    )));
                }
                request_window_shortcut(st, ShortcutAction::TabMenu)
            },
        )
        .view(),
        chrome(
            "tab.drag.cancel",
            "search-action-tab-drag-cancel",
            "search-action-tab-drag-cancel-subtitle",
            "search-kw-tab-drag-cancel",
            |st, _| {
                if st.session_locked() {
                    return Err(ActionError::Explained(t(
                        "search-action-tab-drag-cancel-unavailable",
                    )));
                }
                st.request_tab_drag_cancel();
                Ok(())
            },
        )
        .view(),
        chrome(
            "tab.close",
            "search-action-tab-close",
            "search-action-tab-close-subtitle",
            "search-kw-tab-close",
            |st, _| {
                crate::shell::window_actions::close_active_tab(st).map_err(|err| {
                    let message = err.user_message();
                    st.push_warning_notification(t("search-action-tab-close-failed"), &message);
                    ActionError::Refused(message)
                })
            },
        ),
        Action {
            id: "tab.rename",
            label: "search-action-tab-rename",
            subtitle: "search-action-tab-rename-subtitle",
            keywords: "search-kw-tab-rename",
            params: TAB_RENAME_PARAMS,
            apply: rename_focused_tab,
            ..Action::NEW
        }
        .document(),
        chrome(
            "tab.reopen",
            "search-action-tab-reopen",
            "search-action-tab-reopen-subtitle",
            "search-kw-tab-reopen",
            |st, _| {
                st.reopen_closed_tab().map(|_| ()).ok_or_else(|| {
                    ActionError::Refused("no closed tab to reopen in this workspace".into())
                })
            },
        ),
        chrome(
            "tab.split-right",
            "search-action-tab-split-right",
            "search-action-tab-split-right-subtitle",
            "search-kw-tab-split-right",
            |st, _| {
                crate::shell::window_actions::split_active_tab(
                    st,
                    crate::shell::window_actions::SplitPlacement::Right,
                )
                .map(|_| ())
                .map_err(|err| {
                    let message = err.user_message();
                    st.push_warning_notification(t("search-action-tab-split-failed"), &message);
                    ActionError::Refused(message)
                })
            },
        ),
        chrome(
            "window.focus-previous",
            "search-action-window-focus-previous",
            "search-action-window-focus-previous-subtitle",
            "search-kw-window-focus-previous",
            |st, _| request_window_shortcut(st, ShortcutAction::FocusPreviousWindow),
        )
        .view_step(),
        chrome(
            "window.focus-next",
            "search-action-window-focus-next",
            "search-action-window-focus-next-subtitle",
            "search-kw-window-focus-next",
            |st, _| request_window_shortcut(st, ShortcutAction::FocusNextWindow),
        )
        .view_step(),
        chrome(
            "strip.focus-previous",
            "search-action-strip-focus-previous",
            "search-action-strip-focus-previous-subtitle",
            "search-kw-strip-focus-previous",
            |st, _| request_window_shortcut(st, ShortcutAction::PreviousStrip),
        )
        .view_step(),
        chrome(
            "strip.focus-next",
            "search-action-strip-focus-next",
            "search-action-strip-focus-next-subtitle",
            "search-kw-strip-focus-next",
            |st, _| request_window_shortcut(st, ShortcutAction::NextStrip),
        )
        .view_step(),
        Action {
            id: "strip.create",
            label: "search-action-strip-create",
            subtitle: "search-action-strip-create-subtitle",
            keywords: "search-kw-strip-create",
            params: STRIP_CREATE_PARAMS,
            apply: create_strip,
            ..Action::NEW
        }
        .document(),
        Action {
            id: "strip.rename",
            label: "search-action-strip-rename",
            subtitle: "search-action-strip-rename-subtitle",
            keywords: "search-kw-strip-rename",
            params: STRIP_RENAME_PARAMS,
            apply: rename_strip,
            ..Action::NEW
        }
        .document(),
        Action {
            id: "strip.move",
            label: "search-action-strip-move",
            subtitle: "search-action-strip-move-subtitle",
            keywords: "search-kw-strip-move",
            params: STRIP_MOVE_PARAMS,
            apply: move_strip,
            ..Action::NEW
        }
        .document(),
        Action {
            id: "strip.remove",
            label: "search-action-strip-remove",
            subtitle: "search-action-strip-remove-subtitle",
            keywords: "search-kw-strip-remove",
            params: STRIP_REMOVE_PARAMS,
            apply: remove_strip,
            ..Action::NEW
        }
        .document(),
        Action {
            id: "workspace.create",
            label: "search-action-workspace-create",
            subtitle: "search-action-workspace-create-subtitle",
            keywords: "search-kw-workspace-create",
            params: WORKSPACE_CREATE_PARAMS,
            apply: create_workspace,
            ..Action::NEW
        }
        .document(),
        Action {
            id: "workspace.rename",
            label: "search-action-workspace-rename",
            subtitle: "search-action-workspace-rename-subtitle",
            keywords: "search-kw-workspace-rename",
            params: WORKSPACE_RENAME_PARAMS,
            apply: rename_workspace,
            ..Action::NEW
        }
        .document(),
        Action {
            id: "workspace.delete",
            label: "search-action-workspace-delete",
            subtitle: "search-action-workspace-delete-subtitle",
            keywords: "search-kw-workspace-delete",
            params: WORKSPACE_DELETE_PARAMS,
            apply: delete_workspace,
            ..Action::NEW
        }
        .destructive(),
        chrome(
            "workspace.move-earlier",
            "search-action-workspace-move-earlier",
            "search-action-workspace-move-earlier-subtitle",
            "search-kw-workspace-move-earlier",
            |st, _| move_workspace(st, -1),
        ),
        chrome(
            "workspace.move-later",
            "search-action-workspace-move-later",
            "search-action-workspace-move-later-subtitle",
            "search-kw-workspace-move-later",
            |st, _| move_workspace(st, 1),
        ),
        chrome(
            "workspace.previous",
            "search-action-workspace-previous",
            "search-action-workspace-previous-subtitle",
            "search-kw-workspace-previous",
            |st, _| request_window_shortcut(st, ShortcutAction::PreviousWorkspace),
        )
        .view_step(),
        chrome(
            "workspace.next",
            "search-action-workspace-next",
            "search-action-workspace-next-subtitle",
            "search-kw-workspace-next",
            |st, _| request_window_shortcut(st, ShortcutAction::NextWorkspace),
        )
        .view_step(),
        chrome(
            "tab.previous",
            "search-action-tab-previous",
            "search-action-tab-previous-subtitle",
            "search-kw-tab-previous",
            |st, _| request_window_shortcut(st, ShortcutAction::PreviousTab),
        )
        .view_step(),
        chrome(
            "tab.next",
            "search-action-tab-next",
            "search-action-tab-next-subtitle",
            "search-kw-tab-next",
            |st, _| request_window_shortcut(st, ShortcutAction::NextTab),
        )
        .view_step(),
        chrome(
            "group.move-left",
            "search-action-group-move-left",
            "search-action-group-move-left-subtitle",
            "search-kw-group-move-left",
            |st, _| request_group_shortcut(st, ShortcutAction::MoveGroupLeft),
        )
        .rearranges(),
        chrome(
            "group.move-right",
            "search-action-group-move-right",
            "search-action-group-move-right-subtitle",
            "search-kw-group-move-right",
            |st, _| request_group_shortcut(st, ShortcutAction::MoveGroupRight),
        )
        .rearranges(),
        chrome(
            "group.move-to-strip-previous",
            "search-action-group-move-to-strip-previous",
            "search-action-group-move-to-strip-previous-subtitle",
            "search-kw-group-move-to-strip-previous",
            |st, _| request_group_shortcut(st, ShortcutAction::MoveGroupToPreviousStrip),
        )
        .rearranges(),
        chrome(
            "group.move-to-strip-next",
            "search-action-group-move-to-strip-next",
            "search-action-group-move-to-strip-next-subtitle",
            "search-kw-group-move-to-strip-next",
            |st, _| request_group_shortcut(st, ShortcutAction::MoveGroupToNextStrip),
        )
        .rearranges(),
        Action {
            id: GROUP_MOVE_TO_STRIP_ID,
            label: "search-action-group-move-to-strip",
            subtitle: "search-action-group-move-to-strip-subtitle",
            keywords: "search-kw-group-move-to-strip",
            params: GROUP_MOVE_TO_STRIP_PARAMS,
            apply: move_group_to_strip,
            ..Action::NEW
        }
        .rearranges(),
        chrome(
            "group.close",
            "search-action-group-close",
            "search-action-group-close-subtitle",
            "search-kw-group-close",
            |st, _| {
                use crate::shell::window_actions::{TabActionError, close_active_group};
                close_active_group(st).map_err(|err| match err {
                    TabActionError::NoActiveTab => no_window(),
                    other => ActionError::Explained(other.user_message()),
                })
            },
        ),
        chrome(
            "group.move-to-display-previous",
            "search-action-group-move-to-display-previous",
            "search-action-group-move-to-display-previous-subtitle",
            "search-kw-group-move-to-display-previous",
            |st, _| {
                other_display_needed(st)?;
                request_group_shortcut(st, ShortcutAction::MoveGroupToPreviousDisplay)
            },
        )
        .rearranges(),
        chrome(
            "group.move-to-display-next",
            "search-action-group-move-to-display-next",
            "search-action-group-move-to-display-next-subtitle",
            "search-kw-group-move-to-display-next",
            |st, _| {
                other_display_needed(st)?;
                request_group_shortcut(st, ShortcutAction::MoveGroupToNextDisplay)
            },
        )
        .rearranges(),
        chrome(
            "group.toggle-maximize",
            "search-action-group-toggle-maximize",
            "search-action-group-toggle-maximize-subtitle",
            "search-kw-group-toggle-maximize",
            |st, _| request_toggle_maximize(st),
        )
        .rearranges(),
        chrome(
            "group.maximize",
            "search-action-group-maximize",
            "search-action-group-maximize-subtitle",
            "search-kw-group-maximize",
            |st, _| request_group_shortcut(st, ShortcutAction::MaximizeWindow),
        )
        .rearranges(),
        chrome(
            "group.restore",
            "search-action-group-restore",
            "search-action-group-restore-subtitle",
            "search-kw-group-restore",
            |st, _| request_group_shortcut(st, ShortcutAction::RestoreWindow),
        )
        .rearranges(),
        chrome(
            "group.fullscreen",
            "search-action-group-fullscreen",
            "search-action-group-fullscreen-subtitle",
            "search-kw-group-fullscreen",
            |st, _| request_group_shortcut(st, ShortcutAction::ToggleFullscreen),
        )
        .rearranges(),
        chrome(
            "group.narrower",
            "search-action-group-narrower",
            "search-action-group-narrower-subtitle",
            "search-kw-group-narrower",
            |st, _| request_group_shortcut(st, ShortcutAction::ResizeNarrower),
        )
        .rearranges(),
        chrome(
            "group.wider",
            "search-action-group-wider",
            "search-action-group-wider-subtitle",
            "search-kw-group-wider",
            |st, _| request_group_shortcut(st, ShortcutAction::ResizeWider),
        )
        .rearranges(),
        chrome(
            "tab.move-to-group-previous",
            "search-action-tab-move-to-group-previous",
            "search-action-tab-move-to-group-previous-subtitle",
            "search-kw-tab-move-to-group-previous",
            |st, _| request_group_shortcut(st, ShortcutAction::AttachTabLeft),
        )
        .rearranges(),
        chrome(
            "tab.move-to-group-next",
            "search-action-tab-move-to-group-next",
            "search-action-tab-move-to-group-next-subtitle",
            "search-kw-tab-move-to-group-next",
            |st, _| request_group_shortcut(st, ShortcutAction::AttachTabRight),
        )
        .rearranges(),
        Action {
            id: "window.focus-nth",
            label: "search-action-window-focus-nth",
            subtitle: "search-action-window-focus-nth-subtitle",
            keywords: "search-kw-window-focus-nth",
            params: WINDOW_FOCUS_NTH_PARAMS,
            apply: |st, args| {
                let action = args
                    .integer("index")
                    .and_then(|index| usize::try_from(index).ok()?.checked_sub(1))
                    .and_then(|index| {
                        ShortcutAction::ALL
                            .into_iter()
                            .find(|action| action.focus_group_index() == Some(index))
                    })
                    .ok_or_else(|| ActionError::Refused("index must be 1 to 9".into()))?;
                request_window_shortcut(st, action)
            },
            ..Action::NEW
        }
        .view_step(),
        Action {
            id: "strip.focus-nth",
            label: "search-action-strip-focus-nth",
            subtitle: "search-action-strip-focus-nth-subtitle",
            keywords: "search-kw-strip-focus-nth",
            params: STRIP_FOCUS_NTH_PARAMS,
            apply: |st, args| {
                let action = args
                    .integer("index")
                    .and_then(|index| usize::try_from(index).ok()?.checked_sub(1))
                    .and_then(|index| {
                        ShortcutAction::ALL
                            .into_iter()
                            .find(|action| action.focus_strip_index() == Some(index))
                    })
                    .ok_or_else(|| ActionError::Refused("index must be 1 to 9".into()))?;
                request_window_shortcut(st, action)
            },
            ..Action::NEW
        }
        .view_step(),
        chrome(
            "tab.move-to-new-group",
            "search-action-tab-move-to-new-group",
            "search-action-tab-move-to-new-group-subtitle",
            "search-kw-tab-move-to-new-group",
            |st, _| {
                crate::shell::window_actions::split_active_tab(
                    st,
                    crate::shell::window_actions::SplitPlacement::StripEnd,
                )
                .map(|_| ())
                .map_err(|err| {
                    let message = err.user_message();
                    st.push_warning_notification(t("host-tab-move-failed"), &message);
                    ActionError::Refused(message)
                })
            },
        ),
        Action {
            id: "tab.move-to-workspace",
            label: "search-action-tab-move-to-workspace",
            subtitle: "search-action-tab-move-to-workspace-subtitle",
            keywords: "search-kw-tab-move-to-workspace",
            params: TAB_MOVE_TO_WORKSPACE_PARAMS,
            apply: move_focused_tab_to_workspace,
            ..Action::NEW
        },
        chrome(
            "notifications.dnd.toggle",
            "search-action-notifications-dnd-toggle",
            "search-action-notifications-dnd-toggle-subtitle",
            "search-kw-notifications-dnd-toggle",
            |st, _| {
                st.toggle_dnd();
                Ok(())
            },
        )
        .toggles(),
        chrome(
            "notifications.open",
            "search-action-notifications-open",
            "search-action-notifications-open-subtitle",
            "search-kw-notifications-open",
            |st, _| {
                st.open_quick_settings_level(tilo_quick_settings::QsLevel::Notifications);
                Ok(())
            },
        )
        .view(),
        chrome(
            "notifications.clear",
            "search-action-notifications-clear",
            "search-action-notifications-clear-subtitle",
            "search-kw-notifications-clear",
            |st, _| {
                if st.notification_log().history().is_empty() {
                    return Err(ActionError::Refused(
                        "there are no notifications to clear".into(),
                    ));
                }
                st.clear_notification_history();
                Ok(())
            },
        ),
        chrome(
            "notifications.clear.undo",
            "search-action-notifications-clear-undo",
            "search-action-notifications-clear-undo-subtitle",
            "search-kw-notifications-clear-undo",
            |st, _| {
                if st.undo_notification_history_clear() {
                    Ok(())
                } else {
                    Err(ActionError::Refused(
                        "no cleared notifications can be brought back".into(),
                    ))
                }
            },
        ),
        chrome(
            "appearance.theme.dark",
            "search-action-appearance-theme-dark",
            "search-action-appearance-theme-dark-subtitle",
            "search-kw-appearance-theme-dark",
            |st, _| {
                let invoked_by = settings_store::InvokingAction::action("appearance.theme.dark");
                set_mode(st, "dark", &invoked_by)
            },
        )
        .toggles(),
        chrome(
            "appearance.theme.light",
            "search-action-appearance-theme-light",
            "search-action-appearance-theme-light-subtitle",
            "search-kw-appearance-theme-light",
            |st, _| {
                let invoked_by = settings_store::InvokingAction::action("appearance.theme.light");
                set_mode(st, "light", &invoked_by)
            },
        )
        .toggles(),
        Action {
            id: "appearance.language",
            label: "search-action-appearance-language",
            subtitle: "search-action-appearance-language-subtitle",
            keywords: "search-kw-appearance-language",
            params: LANGUAGE_PARAMS,
            apply: |st, args| {
                let language = args.text("language").unwrap_or_default();
                if language.trim().is_empty() {
                    return Err(ActionError::Refused(
                        "a language is required; nothing was changed".into(),
                    ));
                }
                set_language(st, language)
            },
            ..Action::NEW
        },
        // The palette does this on Enter over a record row; this is the
        // address for a caller that names a record itself.
        Action {
            id: "record.bring",
            label: "search-action-record-bring",
            subtitle: "search-action-record-bring-subtitle",
            keywords: "search-kw-record-bring",
            params: RECORD_BRING_PARAMS,
            apply: |st, args| {
                let id = args.text("id").unwrap_or_default();
                if id.trim().is_empty() {
                    return Err(ActionError::Refused(
                        "a record id is required; nothing was brought here".into(),
                    ));
                }
                if ui::record::RecordId::parse(id).is_none() {
                    return Err(ActionError::Refused(format!(
                        "{id:?} is not a record id this build can read"
                    )));
                }
                let kind = args.text("kind").unwrap_or("document");
                let kind = if kind.trim().is_empty() {
                    "document"
                } else {
                    kind
                };
                let tree = st.tree();
                let workspace = tree.desktop.active_workspace().ok_or_else(|| {
                    ActionError::Refused("no active workspace accepts the record".into())
                })?;
                let user = st.local_user_for_room(&workspace.room());
                let actor = workspace.actor_for_person(&user);
                let target = (workspace.id, workspace.room(), actor);
                drop(tree);
                st.push_tracked_record_request(id, kind, target.0, target.1, target.2);
                Ok(())
            },
            ..Action::NEW
        }
        .document(),
        // Last: an English "open" on a German desktop matches every *.open
        // id at the same score, so declaration order is the only tiebreak.
        // The tour and the help overlay must not win that race.
        chrome(
            "shortcut-help.toggle",
            "search-action-shortcut-help-toggle",
            "search-action-shortcut-help-toggle-subtitle",
            "search-kw-shortcut-help-toggle",
            |st, _| {
                st.overlays.shortcut_help = !st.overlays.shortcut_help;
                Ok(())
            },
        )
        .view_step(),
        chrome(
            "desktop-tour.open",
            "search-action-desktop-tour-open",
            "search-action-desktop-tour-open-subtitle",
            "search-kw-desktop-tour-open",
            |st, _| {
                st.overlays.desktop_tour = true;
                Ok(())
            },
        )
        .view(),
    ];
}
