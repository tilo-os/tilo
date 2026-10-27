//! Owned bounded socket IO leaves the `LocalSet` free to drive real capabilities.
use super::{AtomicBool, Duration, Instant, Ordering, UnixStream, bounded, io};
pub(super) async fn exchange<T: Send + 'static>(
    mut stream: UnixStream,
    cancel: &AtomicBool,
    deadline: Instant,
    operation: impl FnOnce(&mut UnixStream) -> io::Result<T> + Send + 'static,
) -> io::Result<(UnixStream, io::Result<T>)> {
    let interrupt = stream.try_clone()?;
    let task = tokio::task::spawn_blocking(move || {
        let result = (|| {
            let held = stream.try_clone()?;
            bounded(&held, &AtomicBool::new(false), deadline, || {
                operation(&mut stream)
            })
        })();
        (stream, result)
    });
    while !task.is_finished() {
        if cancel.load(Ordering::Acquire) || Instant::now() >= deadline {
            let _ = interrupt.shutdown(std::net::Shutdown::Both);
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // Never discard a timeout JoinHandle holding the actual proof or transport.
    task.await.map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        sync::Arc,
    };
    #[test]
    fn stalled_partial_frame_leaves_actual_rpc_runtime_progressing_and_returns_owned_socket() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (client, mut peer) = UnixStream::pair().unwrap();
        peer.write_all(&8u32.to_le_bytes()).unwrap();
        peer.write_all(&[1]).unwrap();
        runtime.block_on(async {
            let progressed = Arc::new(AtomicBool::new(false));
            let mark = Arc::clone(&progressed);
            let timer = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                mark.store(true, Ordering::Release);
            });
            let (owned, result) = exchange(
                client,
                &AtomicBool::new(false),
                Instant::now() + Duration::from_millis(80),
                |stream| {
                    wire_frame::Framing::little_endian(64)
                        .reject_empty()
                        .read(stream)
                        .map_err(io::Error::other)
                },
            )
            .await
            .unwrap();
            timer.await.unwrap();
            assert!(progressed.load(Ordering::Acquire));
            assert!(result.is_err());
            assert!(owned.peer_addr().is_ok());
        });
    }
    #[test]
    fn cancellation_shutdown_waits_for_actual_blocking_reader_before_returning_custody() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (client, _peer) = UnixStream::pair().unwrap();
        runtime.block_on(async {
            let cancel = Arc::new(AtomicBool::new(false));
            let signal = Arc::clone(&cancel);
            let timer = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                signal.store(true, Ordering::Release);
            });
            // The deadline is an hour away: returning is the cancellation's
            // doing.
            let (owned, result) = exchange(
                client,
                &cancel,
                Instant::now() + Duration::from_secs(3600),
                |stream| {
                    let mut byte = [0];
                    stream.read_exact(&mut byte)
                },
            )
            .await
            .unwrap();
            timer.await.unwrap();
            assert!(result.is_err());
            assert!(owned.peer_addr().is_ok());
        });
    }
}
