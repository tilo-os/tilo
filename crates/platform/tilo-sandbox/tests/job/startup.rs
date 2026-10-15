//! The startup-time benchmark: the median spawn-to-exec of `/bin/true` must
//! stay under 100 ms. The design measures about 3 ms; this guards the order
//! of magnitude, not the exact number, so a slow CI machine still passes.

use std::time::{Duration, Instant};

use tilo_sandbox::job::{JobSpec, Launch, Root};

use crate::needs_sandbox;

#[test]
fn startup_stays_under_100ms() {
    needs_sandbox!();
    let runs = 60;
    let mut times = Vec::with_capacity(runs);
    for _ in 0..runs {
        let spec = JobSpec::new(Root::Host { extra: vec![] }, ["/bin/true"]);
        let t0 = Instant::now();
        let mut job = Launch::new(&spec).spawn().expect("spawn /bin/true");
        // spawn() already returned after execve; waiting adds only the exit.
        let _ = job.wait();
        times.push(t0.elapsed());
    }
    times.sort_unstable();
    let median = times[times.len() / 2];
    let p95 = times[times.len() * 95 / 100];
    eprintln!("startup: median {median:?}, p95 {p95:?} over {runs} spawns");
    assert!(
        median < Duration::from_millis(100),
        "median startup {median:?} is over 100 ms"
    );
}
