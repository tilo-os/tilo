//! Screenshot scenarios: the output a scenario's process printed, and the
//! view state shown over it. A capture runs no process, so every scene is the
//! same canned VT byte stream on every machine, parsed by the production
//! emulator exactly as PTY output would be.

use std::fmt::Write as _;

use crate::target::{EnvironmentSummary, RelaySummary, SessionTarget};

/// Every scenario `--shots --scenario` knows.
pub(crate) const SCENARIOS: &[&str] = &[
    "shell",
    "shell-select",
    "colors",
    "tui",
    "scrollback",
    "find",
    "empty",
    "empty-menu",
    "ended",
    "targets",
    "environments",
    "host",
];

/// Where the target scenarios' session runs, and the environments the
/// picker lists: shown before the output, so the header is laid out with it.
pub(crate) fn target(name: &str) -> Option<(SessionTarget, Vec<EnvironmentSummary>)> {
    match name {
        "host" => Some((SessionTarget::Host, environments())),
        "targets" | "environments" => Some((SessionTarget::environment("work"), environments())),
        _ => None,
    }
}

/// The person's relays the target scenarios' picker offers to run on.
pub(crate) fn relays(name: &str) -> Vec<RelaySummary> {
    match name {
        "targets" | "environments" => vec![RelaySummary {
            relay: "ab".repeat(32),
            label: "Home relay".into(),
        }],
        _ => Vec::new(),
    }
}

/// What the scenario's process printed.
pub(crate) fn output(name: &str) -> Vec<u8> {
    match name {
        "shell" | "shell-select" => shell(),
        "colors" => colors(),
        "tui" => tui(),
        "scrollback" => numbered_log(|i| {
            format!(
                "\x1b[38;5;250m{i:>4}\x1b[0m  log entry {i:03}: the quick brown fox jumps over the lazy dog"
            )
        }),
        // A terminal LF preserves the current column; the shell's CRLF output
        // models a readable transcript.
        "find" => numbered_log(|i| format!("{i:>4}  build step {i:03}: compiling terminal search")),
        "host" => b"you@laptop:~$ podman ps\r\nCONTAINER ID  IMAGE  COMMAND  CREATED  STATUS\r\nyou@laptop:~$ "
            .to_vec(),
        "targets" | "environments" => {
            b"[work ~]$ pandoc --version | head -1\r\npandoc 3.1.11\r\n[work ~]$ ".to_vec()
        }
        _ => Vec::new(),
    }
}

/// A short session at a login shell's prompt: a command, its output, and a
/// green `done`, so the shot shows prompt colours and SGR-coloured output.
fn shell() -> Vec<u8> {
    let prompt = "\x1b[1;32myou@tilo\x1b[0m:\x1b[1;34m~\x1b[0m$ ";
    let mut s = String::new();
    let _ = write!(
        s,
        "{prompt}printf 'Tilo terminal - live PTY session\\n'\r\n\
         Tilo terminal - live PTY session\r\n\
         {prompt}printf 'primes below 20: 2 3 5 7 11 13 17 19\\n'\r\n\
         primes below 20: 2 3 5 7 11 13 17 19\r\n\
         {prompt}printf '\\033[1;32mdone\\033[0m\\n'\r\n\
         \x1b[1;32mdone\x1b[0m\r\n\
         {prompt}"
    );
    s.into_bytes()
}

/// A palette of SGR effects: 16 ANSI colors, a 256-cube strip, a truecolor
/// gradient, and the style bits.
fn colors() -> Vec<u8> {
    let mut s = String::new();
    s.push_str("\x1b[1mTilo terminal — SGR palette\x1b[0m\r\n\r\n");
    s.push_str(" 16-color ANSI:  ");
    for n in 0..8 {
        let _ = write!(s, "\x1b[4{n}m  \x1b[0m");
    }
    s.push(' ');
    for n in 0..8 {
        let _ = write!(s, "\x1b[48;5;{}m  \x1b[0m", n + 8);
    }
    s.push_str("\r\n\r\n 256-color:      ");
    for n in (16..232).step_by(6) {
        let _ = write!(s, "\x1b[48;5;{n}m \x1b[0m");
    }
    s.push_str("\r\n\r\n truecolor:      ");
    for i in 0..48 {
        let r = (i * 255 / 47) as u8;
        let g = (255 - i * 255 / 47) as u8;
        let _ = write!(s, "\x1b[48;2;{r};{g};128m \x1b[0m");
    }
    s.push_str("\r\n\r\n styles:         ");
    s.push_str("\x1b[1mbold\x1b[0m  \x1b[2mdim\x1b[0m  \x1b[3mitalic\x1b[0m  \x1b[4munderline\x1b[0m  \x1b[9mstrike\x1b[0m  \x1b[7mreverse\x1b[0m\r\n");
    s.push_str("\r\n foreground:     ");
    s.push_str(
        "\x1b[38;2;255;90;90mred \x1b[38;2;90;220;120mgreen \x1b[38;2;110;160;255mblue\x1b[0m\r\n",
    );
    s.push_str("\r\n$ \x1b[38;5;120mecho\x1b[0m \x1b[38;5;223m\"hello, Tilo\"\x1b[0m\r\n");
    s.into_bytes()
}

/// A full-screen alternate-screen redraw resembling a text editor.
fn tui() -> Vec<u8> {
    let mut s = String::new();
    s.push_str("\x1b[?1049h\x1b[2J\x1b[H");
    s.push_str("\x1b[1;1H\x1b[7m Tilo EDIT  main.rs  [+]                                      unix  utf-8 \x1b[0m");
    #[allow(
        clippy::literal_string_with_formatting_args,
        reason = "demo source text shown verbatim in the tui shot; the braces are \
                  the sample code's own println! syntax, not a format template here"
    )]
    let lines = [
        "fn start() {",
        "    // alternate-screen redraw via CSI ?1049h",
        "    let greeting = \"hello, Tilo\";",
        "    for i in 0..3 {",
        "        println!(\"{i}: {greeting}\");",
        "    }",
        "}",
    ];
    for (i, line) in lines.iter().enumerate() {
        // Line-number gutter in a dim color, then source text.
        let _ = write!(
            s,
            "\x1b[{};1H\x1b[38;5;240m{:>3} \x1b[0m{}",
            i + 3,
            i + 1,
            line
        );
    }
    // A status line near the bottom in reverse video.
    s.push_str("\x1b[23;1H\x1b[7m NORMAL                                              3,17          Top \x1b[0m");
    s.push_str("\x1b[24;1H:wq");
    s.into_bytes()
}

/// Eighty numbered lines, enough to scroll back through.
fn numbered_log(line: impl Fn(usize) -> String) -> Vec<u8> {
    let mut s = String::new();
    for i in 0..80 {
        s.push_str(&line(i));
        s.push_str("\r\n");
    }
    s.into_bytes()
}

/// The environments the target shots show.
fn environments() -> Vec<EnvironmentSummary> {
    vec![
        EnvironmentSummary {
            key: "default".into(),
            name: "Default".into(),
            tools: vec!["git".into(), "ripgrep".into()],
            grants: vec![],
        },
        EnvironmentSummary {
            key: "work".into(),
            name: "Work".into(),
            tools: vec!["pandoc".into(), "python3".into(), "jq".into()],
            grants: vec![
                ("~/src".into(), true),
                ("~/Documents".into(), false),
                ("~/Projects/website".into(), true),
                ("~/Projects/thesis/figures".into(), true),
                ("~/Downloads/datasets".into(), false),
            ],
        },
    ]
}
