//! The bundled offline sample library.
//!
//! Screenshot scenarios and tests can browse this catalogue without a
//! network. The live app starts empty. Every track carries a synthetic
//! `yt_id`; the streaming path replaces these with real search results.

use base64::Engine as _;
use ui::tilo_resource::{Id, Invoke};

use crate::model::{Album, Artist, CatalogueTrack, Library, Playlist, Track};

fn artist(id: &str, name: &str, genre: &str, listeners: u64) -> Artist {
    Artist {
        id: id.into(),
        name: name.into(),
        genre: genre.into(),
        monthly_listeners: listeners,
    }
}

fn album(id: &str, title: &str, artist_id: &str, year: u32) -> Album {
    Album {
        id: id.into(),
        title: title.into(),
        artist_id: artist_id.into(),
        year,
    }
}

fn track(
    id: &str,
    title: &str,
    album_id: &str,
    artist_id: &str,
    no: u32,
    dur: u32,
    plays: u64,
) -> Track {
    Track {
        id: id.into(),
        title: title.into(),
        album_id: album_id.into(),
        artist_id: artist_id.into(),
        track_no: no,
        duration_s: dur,
        yt_id: format!("yt_{id}"),
        plays,
    }
}

/// A sample album's bundled cover, by its id, as the portable `data:` picture
/// the window and the shell's now-playing card read: the sample library is
/// catalog data with no artwork of its own, and a capture of it should show a
/// song the way a real album does. Each cover is drawn from the album's id, so
/// it is the same picture every run and no two albums share one.
#[must_use]
pub fn sample_cover(album_id: &str) -> String {
    /// A tile the size of a cover thumbnail.
    const EDGE: u32 = 256;
    let seed = fnv1a(album_id);
    let hue = f32::from(u16::try_from(seed % 360).unwrap_or(0));
    let drift = 24.0 + f32::from(u16::try_from((seed >> 9) % 48).unwrap_or(0));
    let (from, to) = (hsv(hue, 0.55, 0.86), hsv((hue + drift) % 360.0, 0.78, 0.38));
    let mut pixels = Vec::with_capacity((EDGE * EDGE * 4) as usize);
    let centre = (EDGE as f32) * 0.18;
    let reach = (EDGE as f32) * 0.62;
    for y in 0..EDGE {
        for x in 0..EDGE {
            let (fx, fy) = (x as f32 / EDGE as f32, y as f32 / EDGE as f32);
            // A diagonal wash between the two colours, lit from the top left.
            let t = ((fx + fy) * 0.5).clamp(0.0, 1.0);
            // One soft disc where the light falls, so the tile reads as art
            // rather than a swatch.
            let glow = (((x as f32 - centre).powi(2) + (y as f32 - centre).powi(2)).sqrt() / reach)
                .clamp(0.0, 1.0);
            let light = (1.0 - glow).powi(3) * 0.35;
            let mix = |a: f32, b: f32| {
                let lerp = a + (b - a) * t;
                lerp + (1.0 - lerp) * light
            };
            pixels.extend_from_slice(&[
                to_srgb(mix(from[0], to[0])),
                to_srgb(mix(from[1], to[1])),
                to_srgb(mix(from[2], to[2])),
                255,
            ]);
        }
    }
    let Ok(png) = ui::png::encode_rgba8(EDGE, EDGE, &pixels) else {
        return String::new();
    };
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    )
}

/// A number from `text`, stable across runs and processes.
fn fnv1a(text: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// `hue` degrees, `saturation` and `value` from 0 to 1, as linear RGB.
fn hsv(hue: f32, saturation: f32, value: f32) -> [f32; 3] {
    let channel = |n: f32| {
        let k = (n + hue / 60.0) % 6.0;
        value - value * saturation * k.min(4.0 - k).clamp(0.0, 1.0)
    };
    [channel(5.0), channel(3.0), channel(1.0)]
}

/// A linear channel as the 8-bit sRGB value a picture carries.
fn to_srgb(channel: f32) -> u8 {
    let channel = channel.clamp(0.0, 1.0);
    let gamma = if channel <= 0.003_130_8 {
        12.92 * channel
    } else {
        1.055 * channel.powf(1.0 / 2.4) - 0.055
    };
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a clamped channel in 0..=1 times 255 is a byte"
    )]
    let byte = (gamma * 255.0).round() as u8;
    byte
}

/// Build the full seeded library: catalog + playlists + user-authored state.
pub fn sample_library() -> Library {
    let mut lib = Library::default();
    seed_artists(&mut lib);
    seed_albums(&mut lib);
    seed_tracks(&mut lib);
    seed_playlists(&mut lib);
    seed_liked_and_saved(&mut lib);
    lib
}

fn seed_artists(lib: &mut Library) {
    for a in [
        artist("ar_aurora", "Aurora Skies", "Dream Pop", 2_140_553),
        artist("ar_midnight", "Midnight Transit", "Synthwave", 884_201),
        artist("ar_kavi", "Kavi & The Tide", "Indie Folk", 1_502_998),
        artist("ar_lumen", "Lumen Theory", "Electronic", 3_771_440),
        artist("ar_sable", "Sable Quartet", "Modern Classical", 612_770),
        artist("ar_neon", "Neon Harbor", "City Pop", 1_009_330),
    ] {
        lib.artists.insert(a.id.clone(), a);
    }
}

fn seed_albums(lib: &mut Library) {
    for al in [
        album("al_glasslight", "Glasslight", "ar_aurora", 2023),
        album("al_paperboats", "Paper Boats", "ar_aurora", 2021),
        album("al_afterhours", "After Hours Drive", "ar_midnight", 2022),
        album("al_vhs", "VHS Sunsets", "ar_midnight", 2019),
        album("al_riverstone", "Riverstone", "ar_kavi", 2024),
        album("al_pulse", "Pulsewidth", "ar_lumen", 2023),
        album("al_strata", "Strata", "ar_lumen", 2020),
        album("al_nocturnes", "Nocturnes for Strings", "ar_sable", 2022),
        album("al_harborlights", "Harbor Lights", "ar_neon", 2023),
    ] {
        lib.albums.insert(al.id.clone(), al);
    }
}

fn seed_tracks(lib: &mut Library) {
    for t in aurora_skies_tracks()
        .into_iter()
        .chain(midnight_transit_tracks())
        .chain(kavi_tracks())
        .chain(lumen_theory_tracks())
        .chain(sable_quartet_tracks())
        .chain(neon_harbor_tracks())
    {
        lib.tracks.insert(t.id.clone(), t);
    }
}

/// Aurora Skies — Glasslight + Paper Boats.
fn aurora_skies_tracks() -> Vec<Track> {
    vec![
        track(
            "tk_g1",
            "Glasslight",
            "al_glasslight",
            "ar_aurora",
            1,
            215,
            5_412_001,
        ),
        track(
            "tk_g2",
            "Paper Lanterns",
            "al_glasslight",
            "ar_aurora",
            2,
            198,
            3_220_551,
        ),
        track(
            "tk_g3",
            "Slow Tide",
            "al_glasslight",
            "ar_aurora",
            3,
            242,
            2_880_120,
        ),
        track(
            "tk_g4",
            "Ghost in the Frame",
            "al_glasslight",
            "ar_aurora",
            4,
            187,
            1_990_004,
        ),
        track(
            "tk_g5",
            "Halcyon",
            "al_glasslight",
            "ar_aurora",
            5,
            263,
            4_110_900,
        ),
        track(
            "tk_p1",
            "Paper Boats",
            "al_paperboats",
            "ar_aurora",
            1,
            221,
            6_002_330,
        ),
        track(
            "tk_p2",
            "Undertow",
            "al_paperboats",
            "ar_aurora",
            2,
            205,
            1_540_220,
        ),
        track(
            "tk_p3",
            "Lighthouse",
            "al_paperboats",
            "ar_aurora",
            3,
            234,
            2_220_005,
        ),
    ]
}

/// Midnight Transit — After Hours Drive + VHS Sunsets.
fn midnight_transit_tracks() -> Vec<Track> {
    vec![
        track(
            "tk_a1",
            "Neon Mileage",
            "al_afterhours",
            "ar_midnight",
            1,
            252,
            3_990_120,
        ),
        track(
            "tk_a2",
            "Tail Lights",
            "al_afterhours",
            "ar_midnight",
            2,
            228,
            2_330_551,
        ),
        track(
            "tk_a3",
            "Overpass",
            "al_afterhours",
            "ar_midnight",
            3,
            274,
            1_880_004,
        ),
        track(
            "tk_a4",
            "Last Exit",
            "al_afterhours",
            "ar_midnight",
            4,
            301,
            4_550_900,
        ),
        track(
            "tk_v1",
            "Tracking Error",
            "al_vhs",
            "ar_midnight",
            1,
            198,
            1_120_330,
        ),
        track("tk_v2", "Rewind", "al_vhs", "ar_midnight", 2, 213, 990_220),
        track(
            "tk_v3",
            "Static Bloom",
            "al_vhs",
            "ar_midnight",
            3,
            245,
            1_330_005,
        ),
    ]
}

/// Kavi & The Tide — Riverstone.
fn kavi_tracks() -> Vec<Track> {
    vec![
        track(
            "tk_r1",
            "Riverstone",
            "al_riverstone",
            "ar_kavi",
            1,
            236,
            2_770_120,
        ),
        track(
            "tk_r2",
            "Foxglove",
            "al_riverstone",
            "ar_kavi",
            2,
            199,
            1_440_551,
        ),
        track(
            "tk_r3",
            "Cedar & Smoke",
            "al_riverstone",
            "ar_kavi",
            3,
            268,
            3_010_004,
        ),
        track(
            "tk_r4",
            "Long Way Home",
            "al_riverstone",
            "ar_kavi",
            4,
            287,
            2_220_900,
        ),
    ]
}

/// Lumen Theory — Pulsewidth + Strata.
fn lumen_theory_tracks() -> Vec<Track> {
    vec![
        track(
            "tk_u1",
            "Pulsewidth",
            "al_pulse",
            "ar_lumen",
            1,
            312,
            7_220_330,
        ),
        track(
            "tk_u2", "Modular", "al_pulse", "ar_lumen", 2, 289, 3_120_220,
        ),
        track(
            "tk_u3",
            "Sidechain",
            "al_pulse",
            "ar_lumen",
            3,
            274,
            4_330_005,
        ),
        track(
            "tk_u4", "Resonant", "al_pulse", "ar_lumen", 4, 256, 2_990_120,
        ),
        track(
            "tk_s1",
            "Bedrock",
            "al_strata",
            "ar_lumen",
            1,
            298,
            1_770_120,
        ),
        track(
            "tk_s2",
            "Sediment",
            "al_strata",
            "ar_lumen",
            2,
            264,
            1_140_551,
        ),
        track(
            "tk_s3",
            "Uplift",
            "al_strata",
            "ar_lumen",
            3,
            241,
            2_010_004,
        ),
    ]
}

/// Sable Quartet — Nocturnes for Strings.
fn sable_quartet_tracks() -> Vec<Track> {
    vec![
        track(
            "tk_n1",
            "Nocturne I",
            "al_nocturnes",
            "ar_sable",
            1,
            327,
            880_330,
        ),
        track(
            "tk_n2",
            "Nocturne II",
            "al_nocturnes",
            "ar_sable",
            2,
            354,
            770_220,
        ),
        track(
            "tk_n3",
            "Nocturne III",
            "al_nocturnes",
            "ar_sable",
            3,
            298,
            990_005,
        ),
    ]
}

/// Neon Harbor — Harbor Lights.
fn neon_harbor_tracks() -> Vec<Track> {
    vec![
        track(
            "tk_h1",
            "Harbor Lights",
            "al_harborlights",
            "ar_neon",
            1,
            233,
            3_330_120,
        ),
        track(
            "tk_h2",
            "Midnight Ferry",
            "al_harborlights",
            "ar_neon",
            2,
            217,
            2_140_551,
        ),
        track(
            "tk_h3",
            "Cassette Sun",
            "al_harborlights",
            "ar_neon",
            3,
            249,
            1_910_004,
        ),
        track(
            "tk_h4",
            "Boulevard",
            "al_harborlights",
            "ar_neon",
            4,
            271,
            2_550_900,
        ),
    ]
}

fn seed_playlists(lib: &mut Library) {
    for (id, name, description, tracks) in [
        (
            "pl_focus",
            "Deep Focus",
            "Long-form instrumentals for heads-down work.",
            &["tk_u1", "tk_n1", "tk_s1", "tk_u4", "tk_n2", "tk_g3"][..],
        ),
        (
            "pl_drive",
            "Night Drive",
            "Synthwave and city pop for the open road.",
            &["tk_a1", "tk_h1", "tk_a4", "tk_v1", "tk_h4", "tk_a2"][..],
        ),
        (
            "pl_morning",
            "Slow Morning",
            "Easing into the day.",
            &["tk_p1", "tk_r1", "tk_g2", "tk_r3"][..],
        ),
    ] {
        let mut playlist = Playlist::named(id, name);
        description.clone_into(&mut playlist.description);
        for track in tracks {
            playlist.push(track);
        }
        lib.playlists.push(Id::from_raw(id), playlist);
    }
}

/// A populated "Liked Songs" + a couple saved albums so the screenshots show
/// the like/heart affordances in their active state.
fn seed_liked_and_saved(lib: &mut Library) {
    for id in ["tk_g1", "tk_p1", "tk_u1", "tk_h1", "tk_r3", "tk_a4"] {
        lib.liked.insert(id.into());
    }
    for id in ["al_glasslight", "al_pulse"] {
        lib.saved_albums.insert(id.into());
    }
}

/// The sample catalogue as the actions that build it in a library: every
/// track imported with its album and artist, the playlists put in place
/// with their tracks, and the likes, saved albums and followed artists set.
/// Screenshot scenarios stage it through the same door as every edit.
pub fn sample_actions() -> Vec<Invoke<Library>> {
    let lib = sample_library();
    let paths = Library::paths();
    let mut actions = Vec::new();
    for track in lib.tracks.values() {
        let (Some(album), Some(artist)) = (
            lib.albums.get(&track.album_id),
            lib.artists.get(&track.artist_id),
        ) else {
            continue;
        };
        actions.push(Library::IMPORT_TRACK.with(CatalogueTrack {
            track: track.clone(),
            album: album.clone(),
            artist: artist.clone(),
        }));
    }
    let mut edits = Vec::new();
    for (at, (id, playlist)) in lib.playlists.iter().enumerate() {
        edits.push(paths.playlists.insert_as(id, at, playlist));
    }
    edits.extend(lib.liked.iter().map(|track| paths.liked.insert(track)));
    edits.extend(
        lib.saved_albums
            .iter()
            .map(|album| paths.saved_albums.insert(album)),
    );
    edits.extend(
        lib.followed_artists
            .iter()
            .map(|artist| paths.followed_artists.insert(artist)),
    );
    actions.extend(edits.iter().filter_map(|edit| Invoke::edit(edit).ok()));
    actions
}

#[cfg(test)]
pub(crate) fn sample_document() -> automerge::AutoCommit {
    let mut doc = automerge::AutoCommit::new();
    <Library as ui::tilo_resource::Resource>::initialize(&mut doc)
        .expect("the current library schema initializes");
    ui::tilo_resource::Fields::write_fields(&sample_library(), &mut doc, &automerge::ROOT)
        .expect("the sample library writes");
    doc
}

#[cfg(test)]
pub(crate) fn library_of(doc: &automerge::AutoCommit) -> Library {
    <Library as ui::tilo_resource::Resource>::read(doc).expect("a music library")
}

#[cfg(test)]
pub(crate) fn empty_document() -> automerge::AutoCommit {
    let mut doc = automerge::AutoCommit::new();
    <Library as ui::tilo_resource::Resource>::initialize(&mut doc)
        .expect("the current library schema initializes");
    ui::tilo_resource::Fields::write_fields(&Library::default(), &mut doc, &automerge::ROOT)
        .expect("an empty library writes");
    doc
}
