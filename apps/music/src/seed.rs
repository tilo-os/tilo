//! The bundled offline sample library.
//!
//! Shipping a populated catalog means the `--shots` screenshot (and a
//! first-run with no synced devices) shows a real, browsable library and a
//! now-playing track instead of an empty shell. Every track carries a
//! synthetic `yt_id`; the real streaming path ([`crate::source`]) would
//! replace these with live YouTube-Music ids resolved from a search.

use crate::model::{Album, Artist, CacheState, Library, Playback, Playlist, RepeatMode, Track};

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

/// Build the full seeded library: catalog + playlists + cache state.
pub fn sample_library() -> Library {
    let mut lib = Library::default();

    // ---- Artists -------------------------------------------------------
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

    // ---- Albums --------------------------------------------------------
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

    // ---- Tracks --------------------------------------------------------
    let tracks = [
        // Aurora Skies — Glasslight
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
        // Aurora Skies — Paper Boats
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
        // Midnight Transit — After Hours Drive
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
        // Midnight Transit — VHS Sunsets
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
        // Kavi & The Tide — Riverstone
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
        // Lumen Theory — Pulsewidth
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
        // Lumen Theory — Strata
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
        // Sable Quartet — Nocturnes for Strings
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
        // Neon Harbor — Harbor Lights
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
    ];
    for t in tracks {
        lib.tracks.insert(t.id.clone(), t);
    }

    // ---- Playlists -----------------------------------------------------
    lib.playlists = vec![
        Playlist {
            id: "pl_focus".into(),
            name: "Deep Focus".into(),
            description: "Long-form instrumentals for heads-down work.".into(),
            track_ids: vec![
                "tk_u1".into(),
                "tk_n1".into(),
                "tk_s1".into(),
                "tk_u4".into(),
                "tk_n2".into(),
                "tk_g3".into(),
            ],
            cover: String::new(),
        },
        Playlist {
            id: "pl_drive".into(),
            name: "Night Drive".into(),
            description: "Synthwave and city pop for the open road.".into(),
            track_ids: vec![
                "tk_a1".into(),
                "tk_h1".into(),
                "tk_a4".into(),
                "tk_v1".into(),
                "tk_h4".into(),
                "tk_a2".into(),
            ],
            cover: String::new(),
        },
        Playlist {
            id: "pl_morning".into(),
            name: "Slow Morning".into(),
            description: "Easing into the day.".into(),
            track_ids: vec![
                "tk_p1".into(),
                "tk_r1".into(),
                "tk_g2".into(),
                "tk_r3".into(),
            ],
            cover: String::new(),
        },
    ];

    // ---- Cache / download state ---------------------------------------
    // A handful of tracks are marked to keep offline so the Downloads view has
    // content. (No fabricated in-progress download — a `Downloading` state is
    // only ever set by a real, advancing fetch.)
    for id in [
        "tk_g1", "tk_p1", "tk_u1", "tk_a1", "tk_h1", "tk_r1", "tk_n1",
    ] {
        lib.cache.insert(id.into(), CacheState::Cached);
    }

    // ---- Liked songs + saved albums -----------------------------------
    // A populated "Liked Songs" + a couple saved albums so the screenshots
    // show the like/heart affordances in their active state.
    for id in ["tk_g1", "tk_p1", "tk_u1", "tk_h1", "tk_r3", "tk_a4"] {
        lib.liked.insert(id.into());
    }
    for id in ["al_glasslight", "al_pulse"] {
        lib.saved_albums.insert(id.into());
    }

    lib
}

/// Default playback used when seeding a fresh document: **idle**.
///
/// Nothing is loaded and nothing is playing until the user starts something.
/// The app must never claim to be playing a track the user didn't choose — a
/// fresh launch shows "Nothing playing" in the transport bar, not a fabricated
/// now-playing state.
pub fn sample_playback(_lib: &Library) -> Playback {
    Playback {
        current: None,
        queue: Vec::new(),
        queue_index: 0,
        playing: false,
        position_ms: 0,
        shuffle: false,
        repeat: RepeatMode::Off,
        volume: 76,
    }
}
