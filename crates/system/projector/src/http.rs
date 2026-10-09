//! The token-guarded HLS media server a cast receiver fetches from.

use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use getrandom::{SysRng, rand_core::UnwrapErr};
use rand::Rng;

use crate::error::ProjectorError;

pub(crate) struct MediaTelemetry {
    started: Instant,
    pub(crate) requests: AtomicU64,
    pub(crate) bytes: AtomicU64,
    pub(crate) service_us: AtomicU64,
    pub(crate) last_request_ms: AtomicU64,
}

impl MediaTelemetry {
    pub(crate) fn new() -> Self {
        Self {
            started: Instant::now(),
            requests: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            service_us: AtomicU64::new(0),
            last_request_ms: AtomicU64::new(0),
        }
    }

    pub(crate) fn elapsed_millis(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

impl Default for MediaTelemetry {
    fn default() -> Self {
        Self::new()
    }
}

fn serve_file(mut stream: TcpStream, root: &Path, token: &str, telemetry: &MediaTelemetry) {
    let request_started = Instant::now();
    let mut request = [0u8; 2048];
    let mut read = 0;
    loop {
        if read == request.len() {
            let _ = stream.write_all(
                b"HTTP/1.1 431 Request Header Fields Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            return;
        }
        let Some(remaining) = request.get_mut(read..) else {
            return;
        };
        let Ok(received) = stream.read(remaining) else {
            return;
        };
        if received == 0 {
            return;
        }
        let Some(total) = read.checked_add(received) else {
            return;
        };
        read = total;
        let Some(received_request) = request.get(..read) else {
            return;
        };
        if received_request
            .windows(4)
            .any(|bytes| bytes == b"\r\n\r\n")
        {
            break;
        }
    }
    let Some(bytes) = request.get(..read) else {
        return;
    };
    let first_line = String::from_utf8_lossy(bytes);
    let resource = first_line
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|path| path.strip_prefix('/'))
        .and_then(|path| path.strip_prefix(token))
        .and_then(|path| path.strip_prefix('/'))
        .filter(|path| {
            *path == "live.m3u8"
                || path.strip_prefix("segment-").is_some_and(|rest| {
                    rest.strip_suffix(".ts")
                        .is_some_and(|digits| digits.bytes().all(|byte| byte.is_ascii_digit()))
                })
        });
    let Some(resource) = resource else {
        let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
        return;
    };
    let path = root.join(resource);
    match fs::read(&path) {
        Ok(body) => {
            let content_type = if Path::new(resource).extension() == Some(OsStr::new("m3u8")) {
                "application/vnd.apple.mpegurl"
            } else {
                "video/mp2t"
            };
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&body);
            telemetry.requests.fetch_add(1, Ordering::Relaxed);
            telemetry
                .bytes
                .fetch_add(body.len() as u64, Ordering::Relaxed);
            telemetry.service_us.fetch_add(
                u64::try_from(request_started.elapsed().as_micros()).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
            telemetry
                .last_request_ms
                .store(telemetry.elapsed_millis(), Ordering::Release);
        }
        Err(_) => {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
        }
    }
}

pub(crate) fn start_http_server(
    root: PathBuf,
    stop: Arc<AtomicBool>,
    receiver: std::net::IpAddr,
    token: String,
    telemetry: Arc<MediaTelemetry>,
) -> Result<(u16, std::thread::JoinHandle<()>), ProjectorError> {
    let bind_ip = if receiver.is_ipv6() {
        std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED)
    } else {
        std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
    };
    let listener = TcpListener::bind(std::net::SocketAddr::new(bind_ip, 0))
        .map_err(ProjectorError::io("cannot bind projection media server"))?;
    let port = listener
        .local_addr()
        .map_err(ProjectorError::io("cannot read projection media address"))?
        .port();
    let thread = std::thread::Builder::new()
        .name("tilo-proj-http".to_owned())
        .spawn(move || {
            while !stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok(_) if stop.load(Ordering::Acquire) => break,
                    Ok((stream, peer)) if peer.ip() == receiver => {
                        serve_file(stream, &root, &token, &telemetry);
                    }
                    Ok((mut stream, _)) => {
                        let _ = stream
                            .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n");
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        })
        .map_err(ProjectorError::io("cannot start projection media server"))?;
    Ok((port, thread))
}

pub(crate) fn wake_http_listener(receiver: std::net::IpAddr, port: u16) {
    let loopback = if receiver.is_ipv6() {
        std::net::SocketAddr::new(std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST), port)
    } else {
        std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), port)
    };
    let _ = TcpStream::connect_timeout(&loopback, Duration::from_millis(200));
}

pub(crate) fn random_media_token() -> String {
    let mut bytes = [0u8; 24];
    UnwrapErr(SysRng).fill_bytes(&mut bytes);
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread_ids() -> std::collections::BTreeSet<String> {
        std::fs::read_dir("/proc/self/task")
            .expect("/proc/self/task")
            .map(|entry| entry.expect("task").file_name().into_string().expect("tid"))
            .collect()
    }

    fn context_switches(tid: &str) -> u64 {
        std::fs::read_to_string(format!("/proc/self/task/{tid}/status"))
            .expect("thread status")
            .lines()
            .filter(|line| line.contains("ctxt_switches"))
            .filter_map(|line| line.split_whitespace().nth(1)?.parse::<u64>().ok())
            .sum()
    }

    fn spawned_thread<T>(comm: &str, start: impl FnOnce() -> T) -> (T, String) {
        let before = thread_ids();
        let started = start();
        let waiting = Instant::now();
        loop {
            let named = thread_ids()
                .difference(&before)
                .find(|tid| {
                    std::fs::read_to_string(format!("/proc/self/task/{tid}/comm"))
                        .is_ok_and(|name| name.trim() == comm)
                })
                .cloned();
            if let Some(tid) = named {
                return (started, tid);
            }
            assert!(
                waiting.elapsed() < Duration::from_secs(5),
                "no new thread named {comm}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn assert_stays_asleep(tid: &str, what: &str) {
        std::thread::sleep(Duration::from_millis(50));
        let settled = context_switches(tid);
        std::thread::sleep(Duration::from_millis(400));
        let woke = context_switches(tid).saturating_sub(settled);
        assert_eq!(woke, 0, "idle {what} woke {woke} times in 400 ms");
    }

    #[test]
    fn idle_http_accept_thread_sleeps_without_clients() {
        let stop = Arc::new(AtomicBool::new(false));
        let receiver = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
        let ((port, thread), tid) = spawned_thread("tilo-proj-http", || {
            start_http_server(
                std::env::temp_dir(),
                stop.clone(),
                receiver,
                "token".to_owned(),
                Arc::new(MediaTelemetry::default()),
            )
            .expect("HTTP server")
        });
        assert_stays_asleep(&tid, "projection HTTP accept");
        stop.store(true, Ordering::Release);
        wake_http_listener(receiver, port);
        thread.join().expect("join HTTP server");
    }
}
