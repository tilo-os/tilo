//! Bundled offline catalogue. The `--shots` renderer has no event loop and
//! no network, so the app must look fully alive from a cold start — this
//! module hands back a populated [`TubeModel`] the moment the app boots.
//!
//! Live fetch (Invidious / `SponsorBlock` / `DeArrow`) folds fetched data on top
//! of this once the user interacts; see `api.rs` + `app::TubeApp::pump`.

use crate::model::{Category, Channel, Comment, Route, Segment, Settings, TubeModel, Video};

fn seg(category: Category, start: f64, end: f64, votes: i64) -> Segment {
    Segment {
        category,
        start,
        end,
        votes,
    }
}

fn comment(author: &str, initials: &str, text: &str, likes: u64, ago: &str) -> Comment {
    Comment {
        author: author.into(),
        initials: initials.into(),
        text: text.into(),
        likes,
        time_ago: ago.into(),
        pinned: false,
        hearted: false,
        replies: 0,
    }
}

/// A video by `channel` with every figure zeroed; fixtures fill in the rest
/// with struct-update syntax.
fn video(id: &str, title: &str, dearrow: Option<&str>, channel: &Channel) -> Video {
    Video {
        id: id.into(),
        title: title.into(),
        dearrow_title: dearrow.map(std::string::ToString::to_string),
        channel_id: channel.id.clone(),
        channel_name: channel.name.clone(),
        channel_seed: channel.seed.clone(),
        // Offline sample carries no real uploader avatar URL.
        channel_avatar_url: String::new(),
        views: 0,
        published: String::new(),
        duration: 0,
        description: String::new(),
        likes: 0,
        seed: id.into(),
        // Offline sample: no real thumbnail URL, so the seed gradient shows.
        thumbnail_url: String::new(),
        category: String::new(),
        dearrow_thumb: dearrow.is_some(),
        sponsor: Vec::new(),
        comments: Vec::new(),
        live: false,
    }
}

fn channel(id: &str, name: &str, handle: &str, subs: u64, verified: bool, desc: &str) -> Channel {
    Channel {
        id: id.into(),
        name: name.into(),
        handle: handle.into(),
        subscribers: subs,
        verified,
        description: desc.into(),
        seed: id.into(),
        avatar_url: String::new(),
        banner_url: String::new(),
    }
}

/// Build the full offline catalogue.
#[allow(
    clippy::too_many_lines,
    reason = "one block of fixture literals per bundled channel/video/comment; splitting it \
              would only relocate the sample data, not shrink what a reader must trace for \
              one entry"
)]
pub fn model() -> TubeModel {
    let chans = vec![
        channel(
            "ch_blender",
            "Blender Foundation",
            "@blender",
            1_780_000,
            true,
            "Open-source 3D creation. Free to use for any purpose, forever. Tutorials, Open Movies and release breakdowns.",
        ),
        channel(
            "ch_lowlevel",
            "Low Level Learning",
            "@lowlevellearning",
            742_000,
            true,
            "Systems programming, security and how computers actually work, from the metal up.",
        ),
        channel(
            "ch_kurz",
            "Kurzgesagt",
            "@kurzgesagt",
            22_300_000,
            true,
            "Videos explaining things with optimistic nihilism. Science, space and the very big questions, animated.",
        ),
        channel(
            "ch_techlinked",
            "Privacy Guides",
            "@privacyguides",
            318_000,
            true,
            "Non-profit, socially-motivated website that provides information for protecting your data security and privacy.",
        ),
        channel(
            "ch_synth",
            "Polyphonic",
            "@polyphonic",
            980_000,
            false,
            "Long-form video essays about the music you love and why it sounds the way it does.",
        ),
        channel(
            "ch_lofi",
            "Lo-Fi Garden",
            "@lofigarden",
            2_100_000,
            false,
            "24/7 beats to relax / study / debug FUSE drivers to.",
        ),
    ];
    #[allow(
        clippy::indexing_slicing,
        reason = "indexes a fixed, hand-authored literal list a few lines above by a \
                  literal position at each call site; an out-of-range index here is a \
                  sample-data authoring bug that should fail loudly during development, \
                  not a runtime risk from untrusted input"
    )]
    let c = |i: usize| &chans[i];

    let videos = vec![
        Video {
            views: 842_000,
            published: "3 days ago".into(),
            duration: 1620,
            category: "Programming".into(),
            description: "A deep dive into porting a C compositor to Rust with smithay. We cover the wins (memory safety, fearless concurrency), the pain (lifetimes in the event loop) and real frame-time numbers.\n\nChapters:\n0:00 Intro\n2:30 Why Rust\n9:10 The render loop\n18:40 Benchmarks\n24:00 Sponsor\n26:10 Outro".into(),
            likes: 48_000,
            sponsor: vec![
                seg(Category::Intro, 0.0, 18.0, 412),
                seg(Category::Sponsor, 1440.0, 1570.0, 1893),
                seg(Category::Interaction, 540.0, 552.0, 233),
                seg(Category::Outro, 1565.0, 1620.0, 188),
            ],
            comments: vec![
                Comment {
                    pinned: true,
                    hearted: true,
                    replies: 41,
                    ..comment(
                        "Low Level Learning",
                        "LL",
                        "Pinned: benchmark methodology + the full flamegraphs are linked in the description. Ask away!",
                        2_300,
                        "3 days ago",
                    )
                },
                comment(
                    "ferris_fan",
                    "FF",
                    "The borrow checker is the real compositor.",
                    5_400,
                    "3 days ago",
                ),
                Comment {
                    replies: 7,
                    ..comment(
                        "anna.dev",
                        "AD",
                        "Finally a channel that shows the FAILED attempts too. Subscribed.",
                        1_120,
                        "2 days ago",
                    )
                },
                comment(
                    "oxidize_me",
                    "OM",
                    "24:00 sponsor segment is already on SponsorBlock, this client skipped it automatically. Beautiful.",
                    880,
                    "1 day ago",
                ),
            ],
            ..video(
                "vid_rust_async",
                "I rewrote my window compositor in Rust and it changed EVERYTHING!!!",
                Some("Rewriting a Wayland compositor in Rust: results and benchmarks"),
                c(1),
            )
        },
        Video {
            views: 6_400_000,
            published: "1 week ago".into(),
            duration: 700,
            category: "Science".into(),
            description: "Dark matter outweighs everything we can see, five to one. What if the answer was hiding in tiny black holes formed in the first instant of the universe? An animated tour of the primordial black hole hypothesis.".into(),
            likes: 312_000,
            sponsor: vec![
                seg(Category::Sponsor, 410.0, 498.0, 4021),
                seg(Category::SelfPromo, 560.0, 600.0, 1204),
                seg(Category::Outro, 585.0, 612.0, 902),
            ],
            comments: vec![
                Comment {
                    pinned: true,
                    hearted: true,
                    replies: 120,
                    ..comment(
                        "Kurzgesagt",
                        "KG",
                        "Sources & further reading are in the description. This one took 1,400 hours to animate.",
                        41_000,
                        "1 week ago",
                    )
                },
                comment(
                    "astro_amy",
                    "AA",
                    "optimistic nihilism is exactly the energy I need on a Monday",
                    12_300,
                    "6 days ago",
                ),
                comment(
                    "void_walker",
                    "VW",
                    "the duck at 4:32 again. always the duck.",
                    8_900,
                    "5 days ago",
                ),
            ],
            ..video(
                "vid_black_holes",
                "What If Black Holes ARE The Dark Matter? (You Won't BELIEVE #3)",
                Some("Could primordial black holes account for dark matter?"),
                c(2),
            )
        },
        Video {
            views: 3_100_000,
            published: "2 months ago".into(),
            duration: 1500,
            category: "Tutorial".into(),
            description: "The legendary beginner Blender tutorial, updated for 4.2. By the end you'll have modelled, textured and rendered a donut — and learned the whole pipeline doing it.".into(),
            likes: 96_000,
            sponsor: vec![
                seg(Category::Intro, 0.0, 25.0, 600),
                seg(Category::Interaction, 120.0, 134.0, 350),
            ],
            comments: vec![
                comment(
                    "new_to_3d",
                    "NT",
                    "Came for the donut, stayed for the existential dread of UV unwrapping.",
                    4_100,
                    "1 month ago",
                ),
                comment(
                    "blendr",
                    "BL",
                    "the coffee cup is the real final boss",
                    2_050,
                    "3 weeks ago",
                ),
            ],
            ..video(
                "vid_donut",
                "The Donut Tutorial That Started It All (Blender 4.2)",
                None,
                c(0),
            )
        },
        Video {
            views: 1_240_000,
            published: "5 days ago".into(),
            duration: 900,
            category: "Privacy".into(),
            description: "Not all end-to-end encryption is equal. We compare Signal, Matrix, iMessage and the rest on metadata exposure, threat model and who can subpoena what. With a clear recommendation table at the end.".into(),
            likes: 72_000,
            sponsor: vec![
                seg(Category::Sponsor, 95.0, 180.0, 2210),
                seg(Category::SelfPromo, 760.0, 810.0, 640),
                seg(Category::Filler, 300.0, 360.0, 120),
            ],
            comments: vec![
                Comment {
                    pinned: true,
                    replies: 33,
                    ..comment(
                        "Privacy Guides",
                        "PG",
                        "We are a non-profit. No segment of this video is paid — the 'sponsor' tag is our own recommendations, flagged honestly.",
                        9_800,
                        "5 days ago",
                    )
                },
                comment(
                    "threat_model",
                    "TM",
                    "metadata is the message. great breakdown.",
                    3_300,
                    "4 days ago",
                ),
            ],
            ..video(
                "vid_privacy",
                "STOP Using These Apps RIGHT NOW (they're SPYING on you!!)",
                Some("Which messaging apps actually protect your metadata — a 2026 comparison"),
                c(3),
            )
        },
        Video {
            views: 2_800_000,
            published: "3 weeks ago".into(),
            duration: 1100,
            category: "Music".into(),
            description: "The vocoder, the talkbox, and the EHX in between. A history of how two French robots made the most human-sounding machines in pop.".into(),
            likes: 145_000,
            sponsor: vec![
                seg(Category::Sponsor, 720.0, 815.0, 1502),
                seg(Category::MusicOfftopic, 0.0, 42.0, 880),
                seg(Category::Outro, 980.0, 1024.0, 410),
            ],
            comments: vec![
                comment(
                    "groove_is",
                    "GI",
                    "harder. better. faster. *vocoder intensifies*",
                    6_700,
                    "2 weeks ago",
                ),
                comment(
                    "eternal_synth",
                    "ES",
                    "the MusicOfftopic tag skipping straight to the song is god-tier",
                    3_100,
                    "1 week ago",
                ),
            ],
            ..video(
                "vid_synth",
                "How Daft Punk Engineered The Perfect Robot Voice",
                None,
                c(4),
            )
        },
        Video {
            views: 41_000_000,
            published: "Streaming".into(),
            duration: 7200,
            category: "Music".into(),
            description: "A 24/7 livestream of mellow beats. No sponsor, no intro, no outro — just the girl, her desk, and the rain.".into(),
            likes: 980_000,
            comments: vec![
                comment(
                    "midnight_dev",
                    "MD",
                    "anyone else here at 3am shipping a feature nobody asked for",
                    22_000,
                    "now",
                ),
                comment(
                    "studious",
                    "ST",
                    "the cat moved. THE CAT MOVED.",
                    14_500,
                    "2 hours ago",
                ),
            ],
            ..video(
                "vid_lofi",
                "lofi hip hop radio - beats to relax/study to",
                None,
                c(5),
            )
        },
        Video {
            views: 534_000,
            published: "4 days ago".into(),
            duration: 480,
            category: "Programming".into(),
            description: "Warps, lanes, occupancy and why a branch can cost you 32x. A from-scratch mental model of how a modern GPU schedules work.".into(),
            likes: 29_000,
            sponsor: vec![
                seg(Category::Intro, 0.0, 14.0, 210),
                seg(Category::Sponsor, 300.0, 372.0, 940),
            ],
            comments: vec![comment(
                "simd_enjoyer",
                "SE",
                "occupancy is the new cache miss",
                1_900,
                "3 days ago",
            )],
            ..video(
                "vid_gpu",
                "Why Your GPU Is Actually A Tiny Supercomputer",
                Some("An introduction to GPU architecture and SIMT execution"),
                c(1),
            )
        },
        Video {
            views: 9_100_000,
            published: "1 month ago".into(),
            duration: 700,
            category: "Science".into(),
            description: "New observations are quietly rewriting the textbook. From the Kuiper belt to a possible ninth planet, here's what changed.".into(),
            likes: 420_000,
            sponsor: vec![
                seg(Category::Sponsor, 480.0, 560.0, 3300),
                seg(Category::Preview, 0.0, 30.0, 700),
            ],
            comments: vec![comment(
                "planet9",
                "P9",
                "it's always 'a possible ninth planet' and never an invitation to dinner",
                18_000,
                "3 weeks ago",
            )],
            ..video(
                "vid_solar",
                "We Were Wrong About The Solar System",
                None,
                c(2),
            )
        },
        Video {
            views: 188_000,
            published: "6 days ago".into(),
            duration: 720,
            category: "Programming".into(),
            description: "Projection vs hydration, content addressing with blake3, and why your open() doesn't mean the bytes are local. A tour of a real P2P FUSE design.".into(),
            likes: 14_000,
            sponsor: vec![
                seg(Category::Sponsor, 600.0, 690.0, 510),
                seg(Category::Interaction, 200.0, 210.0, 88),
            ],
            comments: vec![comment(
                "mountpoint",
                "MP",
                "10s metadata TTL is a bold choice and I respect it",
                920,
                "5 days ago",
            )],
            ..video(
                "vid_fuse",
                "Building a P2P Filesystem in 100 Lines (sort of)",
                Some("Writing a FUSE driver with content-addressed P2P storage"),
                c(1),
            )
        },
        Video {
            views: 5_200_000,
            published: "2 weeks ago".into(),
            duration: 360,
            category: "Science".into(),
            description: "Magenta isn't on the rainbow. Your brain invents it. A short, beautiful detour into how colour perception actually works.".into(),
            likes: 260_000,
            sponsor: vec![seg(Category::Sponsor, 240.0, 300.0, 1600)],
            comments: vec![comment(
                "rgb_to_xyz",
                "RX",
                "magenta is a vibe your visual cortex made up and honestly same",
                7_700,
                "1 week ago",
            )],
            ..video(
                "vid_color",
                "The Color That Doesn't Exist",
                None,
                c(2),
            )
        },
        Video {
            views: 1_100_000,
            published: "1 week ago".into(),
            duration: 800,
            category: "Music".into(),
            description: "Why a stadium gig sounds like mud — and the delay towers, line arrays and psychoacoustics that fix it.".into(),
            likes: 58_000,
            sponsor: vec![
                seg(Category::Sponsor, 540.0, 620.0, 720),
                seg(Category::Outro, 700.0, 740.0, 230),
            ],
            comments: vec![comment(
                "front_of_house",
                "FH",
                "delay towers are the unsung heroes of every festival",
                2_400,
                "5 days ago",
            )],
            ..video(
                "vid_amp",
                "The Hidden Genius of Stadium Sound Design",
                None,
                c(4),
            )
        },
        Video {
            views: 670_000,
            published: "2 days ago".into(),
            duration: 960,
            category: "Tech".into(),
            description: "From i3 to the new spatial 2D-cluster compositors. Ergonomics, keybinds, multi-monitor and which one actually survives a workday.".into(),
            likes: 33_000,
            sponsor: vec![
                seg(Category::Intro, 0.0, 20.0, 300),
                seg(Category::Sponsor, 410.0, 500.0, 1100),
                seg(Category::SelfPromo, 880.0, 920.0, 260),
            ],
            comments: vec![comment(
                "tile_or_die",
                "TD",
                "2D clusters on an infinite canvas is the first new idea in WMs in a decade",
                4_900,
                "1 day ago",
            )],
            ..video(
                "vid_compose",
                "I Tried Every Tiling Window Manager So You Don't Have To",
                Some("A practical comparison of tiling window managers in 2026"),
                c(3),
            )
        },
    ];

    let settings = Settings::default();
    TubeModel {
        videos,
        channels: chans,
        playlists: Vec::new(),
        // Start subscribed to a few so the Subscriptions tab is populated.
        subscriptions: vec![
            "ch_lowlevel".into(),
            "ch_kurz".into(),
            "ch_privacyguides".into(),
            "ch_privacy".into(),
            "ch_blender".into(),
        ]
        .into_iter()
        .filter(|id: &String| ["ch_lowlevel", "ch_kurz", "ch_blender"].contains(&id.as_str()))
        .collect(),
        // The shared queue starts empty; screenshots should not imply a video
        // was queued by a collaborator.
        queue: Vec::new(),
        history_owner: "profile:local".into(),
        history: vec!["vid_black_holes".into(), "vid_donut".into()],
        history_recovery_notice: None,
        settings,
        route: Route::Home,
        search_query: String::new(),
        playhead: 0.0,
        playing: false,
        liked: Vec::new(),
        comment_likes: std::collections::BTreeMap::new(),
        // No pre-seeded notices: the toast stack fills from real activity
        // (auto-skips, share/like acknowledgements, worker status) so it never
        // shows events that didn't happen.
        notices: Vec::new(),
        palette_open: false,
        palette_query: String::new(),
        // The bundled catalogue is, by definition, the offline sample.
        data_source: crate::model::DataSource {
            live: false,
            label: "offline — sample data".into(),
        },
        search_hits_query: String::new(),
        search_hits: Vec::new(),
        search_playlist_hits: Vec::new(),
        search_channel_hits: Vec::new(),
        search_type_filter: crate::model::SearchTypeFilter::default(),
        related_of: String::new(),
        related_hits: Vec::new(),
        // A few crowdsourced dislike counts (Return YouTube Dislike) so the
        // offline watch page shows a real dislikes pill from a cold start; live
        // fetch folds in the true numbers on top.
        dislikes: [
            ("vid_rust_async", 1_240u64),
            ("vid_black_holes", 8_400),
            ("vid_donut", 2_300),
            ("vid_privacy", 3_050),
            ("vid_gpu", 410),
        ]
        .into_iter()
        .map(|(id, n)| (id.to_string(), n))
        .collect(),
        // Offline: no real DeArrow community thumbnails (the sample has no
        // network); live fetch populates this.
        dearrow_thumb_url: std::collections::BTreeMap::new(),
        // The sample is fully populated, so it never shows a loading skeleton.
        feed_loading: false,
        feed_error: None,
        search_loading: false,
        search_error: None,
    }
}
