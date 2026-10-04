//! A bundled sample project, for `--shots` and the demo: a few small, idiomatic
//! programs in several languages.

use crate::model::Project;

const MAIN_RS: &str = r#"//! Entry point for the `ferris-cli` demo.
//!
//! Greets the world, then prints every prime below a limit that you can
//! set with `--count`.

use std::env;

mod prime;
mod greeter;

/// Program entry point. Parses an optional `--count N` flag.
fn main() {
    let args: Vec<String> = env::args().collect();
    let limit = parse_limit(&args).unwrap_or(20);

    greeter::greet("world");

    let primes = prime::primes_below(limit);
    println!("Found {} primes below {}:", primes.len(), limit);
    for (i, p) in primes.iter().enumerate() {
        // Six per row keeps the output tidy in an 80-column terminal.
        let sep = if (i + 1) % 6 == 0 { '\n' } else { ' ' };
        print!("{:>4}{}", p, sep);
    }
    println!();
}

/// Pull an integer out of `--count N`, returning `None` if absent or invalid.
fn parse_limit(args: &[String]) -> Option<u64> {
    let pos = args.iter().position(|a| a == "--count")?;
    args.get(pos + 1)?.parse().ok()
}
"#;

const PRIME_RS: &str = r"//! Prime-number utilities.

/// Return every prime strictly below `limit` using a sieve of Eratosthenes.
pub fn primes_below(limit: u64) -> Vec<u64> {
    if limit < 2 {
        return Vec::new();
    }
    let n = limit as usize;
    let mut sieve = vec![true; n];
    sieve[0] = false;
    sieve[1] = false;

    let mut i = 2usize;
    while i * i < n {
        if sieve[i] {
            let mut j = i * i;
            while j < n {
                sieve[j] = false;
                j += i;
            }
        }
        i += 1;
    }

    sieve
        .iter()
        .enumerate()
        .filter_map(|(idx, &is_prime)| if is_prime { Some(idx as u64) } else { None })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primes_below_twenty() {
        assert_eq!(primes_below(20), vec![2, 3, 5, 7, 11, 13, 17, 19]);
    }
}
";

const GREETER_RS: &str = r#"//! Greeting helpers.

use std::fmt::Display;

/// Print a friendly greeting to stdout.
pub fn greet<T: Display>(who: T) {
    println!("Hello, {}! \u{1F980}", who);
}

/// Build (but don't print) a greeting string — handy for tests.
pub fn greeting<T: Display>(who: T) -> String {
    format!("Hello, {}!", who)
}
"#;

const CARGO_TOML: &str = r#"[package]
name = "ferris-cli"
version = "0.1.0"
edition = "2021"
description = "A tiny prime-counting greeter."
license = "MIT"

[dependencies]
# No external dependencies — std only.

[profile.release]
opt-level = 3
lto = true
"#;

const README_MD: &str = r"# ferris-cli

A tiny command-line demo that greets the world and counts primes.

## Usage

```sh
cargo run -- --count 50
```

## Layout

- `src/main.rs` — argument parsing and the program entry point
- `src/prime.rs` — a sieve of Eratosthenes
- `src/greeter.rs` — greeting helpers
";

const CONFIG_JSON: &str = r#"{
  "name": "ferris-cli",
  "version": "0.1.0",
  "settings": {
    "tabWidth": 4,
    "insertFinalNewline": true,
    "trimTrailingWhitespace": true,
    "rulers": [80, 100]
  },
  "features": ["greeter", "prime-sieve"],
  "experimental": false
}
"#;

const BUILD_SH: &str = r#"#!/usr/bin/env bash
# Build and run the demo in release mode.
set -euo pipefail

COUNT="${1:-20}"

echo "Building ferris-cli..."
cargo build --release

echo "Running with --count ${COUNT}"
exec ./target/release/ferris-cli --count "${COUNT}"
"#;

const APP_PY: &str = r#"#!/usr/bin/env python3
"""A small Python version of the Rust demo."""

from typing import List


def primes_below(limit: int) -> List[int]:
    """Return every prime strictly below `limit`."""
    if limit < 2:
        return []
    sieve = [True] * limit
    sieve[0] = sieve[1] = False
    for i in range(2, int(limit**0.5) + 1):
        if sieve[i]:
            for j in range(i * i, limit, i):
                sieve[j] = False
    return [i for i, is_prime in enumerate(sieve) if is_prime]


if __name__ == "__main__":
    print("Hello, world! \U0001F40D")
    print(primes_below(20))
"#;

/// Construct the bundled sample project tree. Returns a [`Project`] with the
/// `src/main.rs` folder expanded so the first screenshot lands on real code.
pub fn build() -> Project {
    let mut p = Project::new("ferris-cli");
    let root = p.root;

    let src = p.add_folder(root, "src");
    p.add_file(src, "main.rs", MAIN_RS);
    p.add_file(src, "prime.rs", PRIME_RS);
    p.add_file(src, "greeter.rs", GREETER_RS);

    let scripts = p.add_folder(root, "scripts");
    p.add_file(scripts, "build.sh", BUILD_SH);
    p.add_file(scripts, "app.py", APP_PY);

    p.add_file(root, "Cargo.toml", CARGO_TOML);
    p.add_file(root, "README.md", README_MD);
    p.add_file(root, "config.json", CONFIG_JSON);

    // Expand `src` so the file tree opens on something interesting.
    p.toggle_folder(src);
    p
}

/// The node id of `src/main.rs` in a freshly-built sample project — the file
/// we open by default. Resolved by walking the tree so it stays correct if the
/// construction order changes.
pub fn default_open(project: &Project) -> Option<crate::model::NodeId> {
    use crate::model::NodeKind;
    project
        .nodes
        .values()
        .find(|n| n.name == "main.rs" && matches!(n.kind, NodeKind::File { .. }))
        .map(|n| n.id)
}
