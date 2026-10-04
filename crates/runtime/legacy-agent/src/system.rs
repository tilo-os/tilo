//! What the guest reports about itself.

/// `(MemTotal, MemAvailable)` in KiB from `/proc/meminfo`.
#[must_use]
pub fn meminfo(contents: &str) -> (u64, u64) {
    let field = |name: &str| {
        contents
            .lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
            .and_then(|rest| rest.split_whitespace().next()?.parse().ok())
            .unwrap_or(0)
    };
    (field("MemTotal"), field("MemAvailable"))
}

/// Whole seconds from `/proc/uptime`.
#[must_use]
pub fn uptime_secs(contents: &str) -> u64 {
    contents
        .split_whitespace()
        .next()
        .and_then(|secs| secs.split('.').next()?.parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_files_are_read_defensively() {
        let meminfo_text =
            "MemTotal:        2014340 kB\nMemFree: 1 kB\nMemAvailable:    1500000 kB\n";
        assert_eq!(meminfo(meminfo_text), (2_014_340, 1_500_000));
        assert_eq!(meminfo("garbage"), (0, 0));
        assert_eq!(uptime_secs("1234.56 99.00\n"), 1234);
        assert_eq!(uptime_secs(""), 0);
    }
}
