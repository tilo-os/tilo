//! Only committed outgoing custody is enqueued, once per actual live session.
use std::collections::BTreeSet;
#[derive(Default)]
pub(super) struct Cursor {
    sent: BTreeSet<[u8; 32]>,
}
impl Cursor {
    pub(super) fn drive<'a>(
        &mut self,
        frames: impl Iterator<
            Item = (
                [u8; 32],
                &'a [u8],
                [u8; 32],
                tilo_peer_state::transport::Retention,
            ),
        >,
        mut send: impl FnMut(
            &[u8],
            [u8; 32],
            tilo_peer_state::transport::Retention,
        ) -> Result<(), String>,
    ) -> Result<(), String> {
        // The owner bounds entries to128 (64 originals plus64 recovery projections). Never retain ids no longer in its queue.
        let frames: Vec<_> = frames.take(129).collect();
        if frames.len() > 128 {
            return Err("native publication cursor capacity".into());
        }
        self.sent
            .retain(|id| frames.iter().any(|(current, ..)| current == id));
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
            tilo_peer_state::transport::Retention::Delta,
        );
        let mut cursor = Cursor::default();
        assert!(
            cursor
                .drive(std::iter::once(frame), |_, _, _| Err("pump full".into()))
                .is_err()
        );
        let mut sent = 0;
        cursor
            .drive(std::iter::once(frame), |bytes, _, _| {
                assert_eq!(bytes, frame.1);
                sent += 1;
                Ok(())
            })
            .unwrap();
        cursor
            .drive(std::iter::once(frame), |_, _, _| {
                sent += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(sent, 1);
        let mut replacement = Cursor::default();
        replacement
            .drive(std::iter::once(frame), |_, _, _| {
                sent += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(sent, 2);
        cursor
            .drive(std::iter::empty(), |_, _, _| panic!("retired frame"))
            .unwrap();
        assert!(cursor.sent.is_empty());
    }
    #[test]
    fn native_publication_cursor_covers_full_original_and_recovery_routes_then_new_edit() {
        let mut cursor = Cursor::default();
        let mut sent = 0;
        cursor
            .drive(
                (0..128).map(|id| {
                    (
                        [id; 32],
                        b"exact".as_slice(),
                        [id % 2; 32],
                        tilo_peer_state::transport::Retention::Delta,
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
                (1..129).map(|id| {
                    (
                        [id; 32],
                        b"exact".as_slice(),
                        [id % 2; 32],
                        tilo_peer_state::transport::Retention::Delta,
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
                    (0..129).map(|i| (
                        [i; 32],
                        b"x".as_slice(),
                        [0; 32],
                        tilo_peer_state::transport::Retention::Delta
                    )),
                    |_, _, _| panic!("over quota")
                )
                .is_err()
        );
    }
}
