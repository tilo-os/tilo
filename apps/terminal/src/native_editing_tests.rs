#[cfg(all(test, target_os = "linux"))]
mod native_editing_tests {
    use super::*;
    use runtime_portal::editor::{self, Lease, Request};
    use std::time::Duration;
    use tilo_test_env::{hang_bound, wait_until};

    fn fixture() -> std::io::Result<(TerminalWidget, editor::Socket)> {
        let (mut widget, _, _) = crate::tests::build_test_widget_io();
        widget.set_focus(true);
        let (socket, producer) = editor::Socket::pair()?;
        producer.read_timeout(Some(hang_bound()))?;
        widget.native_editor = Some(crate::native_editor::Client::new(
            socket,
            Arc::new(Mutex::new(ui::WakeHandle::default())),
        )?);
        let mut frame = editor::encode(
            1,
            &Request::Replace {
                lease: Lease {
                    incarnation: [1; 16],
                    generation: 1,
                    revision: 1,
                    expires_ns: u64::MAX,
                    anchor: 0,
                    focus: 0,
                },
                transaction: 1,
                text: String::new(),
            },
        )?;
        frame
            .get_mut(5..8)
            .ok_or_else(|| std::io::Error::other("missing editor status header"))?
            .copy_from_slice(&[0, 2, 4]);
        frame
            .get_mut(8..16)
            .ok_or_else(|| std::io::Error::other("missing editor request header"))?
            .fill(0);
        producer.send(&frame)?;
        wait_until("the native announcement", || {
            widget.ime_content() == Some(ui::ime::InputContent::TEXT)
        });
        Ok((widget, producer))
    }

    #[test]
    fn native_editor_redisplay_preserves_lease_but_keyboard_input_revokes_it() {
        let (mut widget, producer) = fixture().expect("qualified native editor fixture");
        let original = widget.ime_edit_fingerprint(&[9; 32]);
        assert!(original.is_some());
        widget.feed(b"\rPROOF> ");
        assert_eq!(
            widget.ime_edit_fingerprint(&[9; 32]),
            original,
            "native redisplay erased its own checkpoint"
        );
        assert!(widget.start_deferred_text().is_some());
        widget.try_write_input(b"typed").unwrap();
        assert_eq!(widget.ime_content(), Some(ui::ime::InputContent::TERMINAL));
        let mut packet = [0; editor::FRAME_BYTES];
        let size = producer.receive(&mut packet).unwrap();
        assert_eq!(size, editor::HEADER_BYTES);
        assert_eq!(packet[5], 6, "physical input did not revoke original lease");
    }

    #[test]
    fn native_editor_ordinary_ime_waits_for_exact_ack_without_pty_fallback() {
        let (mut widget, producer) = fixture().expect("qualified native editor fixture");
        let writes = widget.attach_input_probe();
        widget.commit_ordinary_ime("界");
        assert!(
            writes.try_recv().is_err(),
            "native IME leaked into process stdin"
        );
        let mut packet = [0; editor::FRAME_BYTES];
        let size = producer.receive(&mut packet).unwrap();
        assert_eq!(packet[5], 2);
        assert_eq!(&packet[editor::HEADER_BYTES..size], "界".as_bytes());
        assert!(
            widget.native_ime.as_ref().unwrap().poll().is_none(),
            "queued edit claimed success"
        );
        widget.commit_ordinary_ime("second");
        assert!(writes.try_recv().is_err(), "pending ACK fell back to PTY");
        assert_eq!(widget.native_ime_error, Some(ui::text_input::Failure::Busy));
        let mut receipt = packet[..editor::HEADER_BYTES].to_vec();
        receipt[5] = 0;
        receipt[6] = 3;
        receipt[7] = 0;
        receipt[16..64].fill(0);
        receipt[72..80].fill(0);
        // A matching request id with a different transaction is still uncertain.
        receipt[64..72].fill(0);
        producer.send(&receipt).unwrap();
        wait_until("the wrong transaction to retire the original edit", || {
            widget.poll_native_ime()
        });
        assert_eq!(
            widget.native_ime_error,
            Some(ui::text_input::Failure::Unknown)
        );
        assert!(
            writes.try_recv().is_err(),
            "uncertain edit was blindly retried"
        );
        assert!(widget.status_banner_message().is_some());
        assert!(
            widget
                .accessibility()
                .unwrap()
                .children
                .iter()
                .any(|child| child.live.is_some())
        );
    }

    #[test]
    fn native_editor_menu_and_read_only_state_refuse_capture_and_commit() {
        let (mut widget, producer) = fixture().expect("qualified native editor fixture");
        assert!(widget.start_deferred_text().is_some());
        widget
            .context_menu
            .set_items(vec![MenuItem::action("Copy", || {})]);
        widget.context_menu.open_at([0.0, 0.0]);
        assert_eq!(widget.ime_content(), None);
        assert!(widget.start_deferred_text().is_none());
        assert!(matches!(
            widget.commit_deferred_text("must not move").unwrap().poll(),
            Some(Err(ui::text_input::Failure::Rejected))
        ));
        widget.context_menu.close();
        widget.native_editor_read_only_changed(true);
        assert!(widget.start_deferred_text().is_none());
        assert_eq!(
            widget.inspect_editor(),
            serde_json::json!({ "available": false })
        );
        let mut packet = [0; editor::FRAME_BYTES];
        producer.receive(&mut packet).unwrap();
        assert_eq!(
            packet[5], 6,
            "permission change did not revoke original lease"
        );
    }
    #[test]
    fn canceled_original_capture_cannot_send_after_platform_handoff() {
        let (mut widget, producer) = fixture().expect("qualified native editor fixture");
        let original = widget.start_deferred_text().unwrap();
        original.cancel();
        let receipt = widget.commit_deferred_text("private words").unwrap();
        wait_until("the canceled original capture to complete", || {
            receipt.poll().is_some()
        });
        assert!(matches!(
            receipt.poll(),
            Some(Err(ui::text_input::Failure::Rejected))
        ));
        // The receipt resolves only after the client decided not to send, so
        // a packet it had sent would already be queued: this read waits for
        // nothing that could still arrive.
        producer
            .read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let mut packet = [0; editor::FRAME_BYTES];
        let error = producer.receive(&mut packet).unwrap_err();
        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ));
    }
}
