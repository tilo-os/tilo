//! Live probe for the audio graph client — drives the real
//! PipeWire/WirePlumber graph through the crate's production types
//! ([`tilo_media::audio::RealEnumerator`] / [`tilo_media::audio::RealGraphControl`]), so
//! the enumerate → route → verify path can be exercised end-to-end against a
//! running daemon (not just the unit fixtures).
//!
//! Usage:
//!   cargo run -p media --example `audio_probe`                     # enumerate
//!   cargo run -p media --example `audio_probe` route <stream> <sink-serial>
//!   cargo run -p media --example `audio_probe` unroute <stream>
//!   cargo run -p media --example `audio_probe` default <sink-id>
//!   cargo run -p media --example `audio_probe` volume <node-id> <0.0-1.0>
//!   cargo run -p media --example `audio_probe` mute <node-id> <0|1>

use tilo_media::audio::{AudioEnumerator, GraphControl, RealEnumerator, RealGraphControl};

fn dump() {
    let g = RealEnumerator.enumerate();
    println!("== sinks ==");
    for d in &g.sinks {
        println!(
            "  id={} serial={} {}{} vol={:?} desc={:?}",
            d.id,
            d.serial,
            d.name,
            if d.is_default { " [DEFAULT]" } else { "" },
            d.volume,
            d.description,
        );
    }
    println!("== sources ==");
    for d in &g.sources {
        println!(
            "  id={} serial={} {}{}",
            d.id,
            d.serial,
            d.name,
            if d.is_default { " [DEFAULT]" } else { "" }
        );
    }
    println!("== playback streams ==");
    for s in &g.playback_streams {
        println!(
            "  id={} serial={} app={:?} -> sink node {:?} vol={:?} label={:?}",
            s.id,
            s.serial,
            s.app,
            s.target_id,
            s.volume,
            s.label(),
        );
    }
    println!("== record streams ==");
    for s in &g.record_streams {
        println!(
            "  id={} app={:?} <- source node {:?}",
            s.id, s.app, s.target_id
        );
    }
}

/// Parse `args[i]` as `T`, or print a usage error and exit — this is a
/// probe's CLI arg handling, not a library API, so "tell the operator and
/// stop" is the right failure mode rather than propagating a `Result`.
#[allow(
    clippy::exit,
    reason = "a CLI probe's argument parsing: printing a usage error and stopping immediately is the intended failure mode, not a Result a caller would ever handle"
)]
fn parse_arg<T: std::str::FromStr>(args: &[String], i: usize, name: &str) -> T
where
    T::Err: std::fmt::Display,
{
    let Some(raw) = args.get(i) else {
        eprintln!("audio_probe: missing argument <{name}>");
        std::process::exit(1);
    };
    raw.parse().unwrap_or_else(|e| {
        eprintln!("audio_probe: invalid <{name}> {raw:?}: {e}");
        std::process::exit(1);
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let ctl = RealGraphControl;
    match args.first().map(String::as_str) {
        None => dump(),
        Some("route") => {
            let stream: u32 = parse_arg(&args, 1, "stream");
            let serial: u32 = parse_arg(&args, 2, "sink-serial");
            println!(
                "route stream {stream} -> serial {serial}: {:?}",
                ctl.route_stream(stream, serial)
            );
        }
        Some("unroute") => {
            let stream: u32 = parse_arg(&args, 1, "stream");
            println!("unroute stream {stream}: {:?}", ctl.unroute_stream(stream));
        }
        Some("default") => {
            let id: u32 = parse_arg(&args, 1, "sink-id");
            println!("set default sink {id}: {:?}", ctl.set_default_sink(id));
        }
        Some("volume") => {
            let id: u32 = parse_arg(&args, 1, "node-id");
            let v: f32 = parse_arg(&args, 2, "0.0-1.0");
            println!("set node {id} volume {v}: {:?}", ctl.set_node_volume(id, v));
        }
        Some("mute") => {
            let id: u32 = parse_arg(&args, 1, "node-id");
            let m: bool = args.get(2).is_some_and(|s| s != "0");
            println!("set node {id} mute {m}: {:?}", ctl.set_node_mute(id, m));
        }
        Some(other) => eprintln!("unknown command: {other}"),
    }
}
