//! Only committed outgoing custody is enqueued, once per actual live session.
use std::collections::BTreeSet;
#[derive(Default)]
pub(super) struct Cursor {
    sent: BTreeSet<[u8; 32]>,
}
impl Cursor {
    /// Send each of `frames`, what custody allows now, unless this session
    /// already sent it. `queued` is every id still in custody: an entry held
    /// back for a while (a recovery projection waiting for a fresh governor
    /// checkpoint) stays sent, so it is not sent again once allowed.
    pub(super) fn drive<'a>(
        &mut self,
        queued: impl Iterator<Item = [u8; 32]>,
        frames: impl Iterator<Item = ([u8; 32], &'a [u8], [u8; 32], neighbour_wire::Retention)>,
        mut send: impl FnMut(&[u8], [u8; 32], neighbour_wire::Retention) -> Result<(), String>,
    ) -> Result<(), String> {
        // The owner bounds entries to128 (64 originals plus64 recovery projections). Never retain ids no longer in its queue.
        let frames: Vec<_> = frames.take(129).collect();
        if frames.len() > 128 {
            return Err("native publication cursor capacity".into());
        }
        let queued: BTreeSet<_> = queued.collect();
        self.sent.retain(|id| queued.contains(id));
        for (id, bytes, channel, retention) in frames {
            if self.sent.contains(&id) {
                continue;
            }
            send(bytes, channel, retention)?;
            self.sent.insert(id);
            tracing::debug!(id = %blake3::Hash::from_bytes(id), "native publication enqueued on this cursor");
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_publication_cursor_advances_only_on_enqueue_and_resets_per_session() {
        let frame = (
            [1; 32],
            b"exact signed bytes".as_slice(),
            [2; 32],
            neighbour_wire::Retention::Delta,
        );
        let mut cursor = Cursor::default();
        assert!(
            cursor
                .drive(
                    std::iter::once(frame.0),
                    std::iter::once(frame),
                    |_, _, _| Err("pump full".into())
                )
                .is_err()
        );
        let mut sent = 0;
        cursor
            .drive(
                std::iter::once(frame.0),
                std::iter::once(frame),
                |bytes, _, _| {
                    assert_eq!(bytes, frame.1);
                    sent += 1;
                    Ok(())
                },
            )
            .unwrap();
        cursor
            .drive(
                std::iter::once(frame.0),
                std::iter::once(frame),
                |_, _, _| {
                    sent += 1;
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(sent, 1);
        let mut replacement = Cursor::default();
        replacement
            .drive(
                std::iter::once(frame.0),
                std::iter::once(frame),
                |_, _, _| {
                    sent += 1;
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(sent, 2);
        cursor
            .drive(std::iter::empty(), std::iter::empty(), |_, _, _| {
                panic!("retired frame")
            })
            .unwrap();
        assert!(cursor.sent.is_empty());
    }
    #[test]
    fn native_publication_cursor_covers_full_original_and_recovery_routes_then_new_edit() {
        let mut cursor = Cursor::default();
        let mut sent = 0;
        cursor
            .drive(
                (0..128).map(|id| [id; 32]),
                (0..128).map(|id| {
                    (
                        [id; 32],
                        b"exact".as_slice(),
                        [id % 2; 32],
                        neighbour_wire::Retention::Delta,
                    )
                }),
                |_, _, _| {
                    sent += 1;
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(sent, 128);
        cursor
            .drive(
                (1..129).map(|id| [id; 32]),
                (1..129).map(|id| {
                    (
                        [id; 32],
                        b"exact".as_slice(),
                        [id % 2; 32],
                        neighbour_wire::Retention::Delta,
                    )
                }),
                |_, _, _| {
                    sent += 1;
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(sent, 129);
        assert_eq!(cursor.sent.len(), 128);
    }
    #[test]
    fn native_publication_cursor_refuses_excess_without_partial_send() {
        let mut cursor = Cursor::default();
        assert!(
            cursor
                .drive(
                    (0..129).map(|i| [i; 32]),
                    (0..129).map(|i| (
                        [i; 32],
                        b"x".as_slice(),
                        [0; 32],
                        neighbour_wire::Retention::Delta
                    )),
                    |_, _, _| panic!("over quota")
                )
                .is_err()
        );
    }
    #[test]
    fn native_publication_cursor_never_resends_an_entry_held_back_while_queued() {
        let frame = (
            [3; 32],
            b"recovery projection".as_slice(),
            [4; 32],
            neighbour_wire::Retention::Delta,
        );
        let mut cursor = Cursor::default();
        let mut sent = 0;
        let mut count = |_: &[u8], _, _| {
            sent += 1;
            Ok(())
        };
        cursor
            .drive(std::iter::once(frame.0), std::iter::once(frame), &mut count)
            .unwrap();
        // Still in custody but not sendable now, then sendable again.
        cursor
            .drive(std::iter::once(frame.0), std::iter::empty(), &mut count)
            .unwrap();
        cursor
            .drive(std::iter::once(frame.0), std::iter::once(frame), &mut count)
            .unwrap();
        assert_eq!(sent, 1);
    }
}
