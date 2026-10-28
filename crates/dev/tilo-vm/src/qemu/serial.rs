//! Reading a guest's serial log: whether it says the boot is ready or has
//! failed, and when, by the kernel's clock.

use regex::Regex;
use serde::Serialize;

/// The serial log as text: NULs, carriage returns and ANSI colour sequences
/// removed, so `[  OK  ] Reached target ...` matches like a bare line.
#[must_use]
pub fn clean(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\0' | '\r' => {}
            '\u{1b}' if chars.peek() == Some(&'[') => {
                chars.next();
                // CSI: parameters and intermediates up to one final byte.
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            c => out.push(c),
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    /// A `--ready` pattern matched.
    Ready,
    /// A `--fail` pattern matched.
    Failed,
    /// Ready, but no kernel timestamp precedes the ready line, so there is
    /// no boot time to report and none is made up.
    MissingTiming,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Hit {
    pub verdict: Verdict,
    /// The pattern that matched.
    pub pattern: String,
    /// The line it matched.
    pub line: String,
    /// The last kernel timestamp (`[    1.250000]`) at or before that line,
    /// in milliseconds since the kernel started.
    pub kernel_ms: Option<u64>,
}

/// The last kernel timestamp on `line` (`[    1.250000]`), in milliseconds.
fn kernel_stamp(line: &str) -> Option<u64> {
    line.match_indices('[')
        .filter_map(|(start, _)| {
            let rest = line.get(start.checked_add(1)?..)?;
            let inner = rest.get(..rest.find(']')?)?.trim_start();
            let (seconds, fraction) = inner.split_once('.')?;
            if seconds.is_empty()
                || fraction.is_empty()
                || !seconds.bytes().all(|b| b.is_ascii_digit())
                || !fraction.bytes().all(|b| b.is_ascii_digit())
            {
                return None;
            }
            let seconds: u64 = seconds.parse().ok()?;
            let millis: u64 = fraction
                .bytes()
                .chain(std::iter::repeat(b'0'))
                .take(3)
                .fold(0, |acc: u64, b| {
                    acc.saturating_mul(10)
                        .saturating_add(u64::from(b.saturating_sub(b'0')))
                });
            Some(seconds.saturating_mul(1000).saturating_add(millis))
        })
        .next_back()
}

/// What `text` says so far. A failure anywhere wins over readiness, so a
/// panic after the ready line still fails the boot.
#[must_use]
pub fn scan(text: &str, ready: &[Regex], fail: &[Regex], kernel_time: bool) -> Option<Hit> {
    for pattern in fail {
        if let Some(line) = text.lines().find(|line| pattern.is_match(line)) {
            return Some(Hit {
                verdict: Verdict::Failed,
                pattern: pattern.as_str().to_owned(),
                line: line.to_owned(),
                kernel_ms: None,
            });
        }
    }
    let mut last = None;
    for line in text.lines() {
        if let Some(millis) = kernel_stamp(line) {
            last = Some(millis);
        }
        if let Some(pattern) = ready.iter().find(|pattern| pattern.is_match(line)) {
            let verdict = if kernel_time && last.is_none() {
                Verdict::MissingTiming
            } else {
                Verdict::Ready
            };
            return Some(Hit {
                verdict,
                pattern: pattern.as_str().to_owned(),
                line: line.to_owned(),
                kernel_ms: last,
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const TARGET: &str = r"Reached target [^-]*(multi-user\.target|graphical\.target)";
    const PANIC: &str = "Kernel panic|Failed to load kernel";

    fn check(console: &str) -> Option<Hit> {
        scan(
            &clean(console.as_bytes()),
            &[Regex::new(TARGET).unwrap()],
            &[Regex::new(PANIC).unwrap()],
            true,
        )
    }

    #[test]
    fn boot_measurement_requires_the_target_and_an_observed_kernel_time() {
        let target = "systemd[1]: Reached target multi-user.target - Multi-User System.";
        let hit = check(&format!("[    1.250] {target}")).unwrap();
        assert_eq!((hit.verdict, hit.kernel_ms), (Verdict::Ready, Some(1250)));

        let graphical = format!(
            "[    2.500] previous line\n\x1b[32m[ OK ]\x1b[0m {}",
            target.replace("multi-user", "graphical")
        );
        let hit = check(&graphical).unwrap();
        assert_eq!((hit.verdict, hit.kernel_ms), (Verdict::Ready, Some(2500)));

        assert_eq!(check("Kernel panic").unwrap().verdict, Verdict::Failed);
        assert_eq!(
            check(&format!("[ 1.250] {target}\nKernel panic"))
                .unwrap()
                .verdict,
            Verdict::Failed
        );
        assert_eq!(check(""), None);
        assert_eq!(check(target).unwrap().verdict, Verdict::MissingTiming);
        assert_eq!(
            check(&format!("{target}\n[ 9.000] too late"))
                .unwrap()
                .verdict,
            Verdict::MissingTiming
        );
    }

    #[test]
    fn serial_noise_is_removed_before_matching() {
        assert_eq!(clean(b"a\0b\r\n\x1b[1;32mok\x1b[0m"), "ab\nok");
        let hit = scan(
            "[ 0.5] x\nlogin: ",
            &[Regex::new("login:").unwrap()],
            &[],
            false,
        )
        .unwrap();
        assert_eq!((hit.verdict, hit.kernel_ms), (Verdict::Ready, Some(500)));
    }
}
