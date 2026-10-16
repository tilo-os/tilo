//! CPU and memory pressure as a process of its own, for the desktop smoke.
//!
//! `pressure_fixture BUSY BYTES` runs `BUSY` spinning threads and rewrites a
//! `BYTES`-sized working set continuously, at the lowest priority, the way a
//! build runs beside a desktop session. It dies with its parent.

#[cfg(target_os = "linux")]
fn main() {
    let mut args = std::env::args().skip(1);
    let busy: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(1);
    let bytes: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(0);
    // SAFETY: plain syscalls with constant arguments; the child must not
    // outlive the test that started it, and must never compete with the
    // desktop at the desktop's own priority.
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
        libc::setpriority(libc::PRIO_PROCESS, 0, 19);
    }
    for n in 0..busy {
        std::thread::spawn(move || {
            let mut x = 0x9E37_79B9_7F4A_7C15_u64 ^ u64::try_from(n).unwrap_or(0);
            loop {
                for _ in 0..50_000 {
                    x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                }
                std::hint::black_box(x);
            }
        });
    }
    if bytes > 0 {
        std::thread::spawn(move || {
            const CHUNK: usize = 32 << 20;
            let mut chunks: Vec<Vec<u8>> = (0..bytes.div_ceil(CHUNK))
                .map(|_| vec![1_u8; CHUNK])
                .collect();
            let mut turn = 0_usize;
            loop {
                let index = turn.checked_rem(chunks.len()).unwrap_or(0);
                // Rewrite one chunk through every page, then give it back and
                // fault a fresh one in.
                if let Some(chunk) = chunks.get_mut(index) {
                    for page in chunk.chunks_mut(4096) {
                        if let Some(first) = page.first_mut() {
                            *first = first.wrapping_add(1);
                        }
                    }
                    *chunk = vec![u8::try_from(turn % 251).unwrap_or(0); CHUNK];
                }
                turn = turn.wrapping_add(1);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        });
    }
    loop {
        std::thread::park();
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {}
