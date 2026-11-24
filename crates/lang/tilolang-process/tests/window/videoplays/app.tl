app so.tilo.lang.videoplays "Video plays" (icon: .film, category: .multimedia)

/// A clip the window plays as soon as it is named, and where playback reported it got to.
type Clip {
    /// Where the clip's bytes are, as a `file://` address.
    source: Text
    /// The share of the clip playback first reported it was at.
    first: Float = -1.0
    /// The share it last reported.
    at: Float = 0.0
    /// How many times playback reported where it got to.
    reports: Int merge counter
    /// Why the player could not show the clip, when it could not.
    failure: Text
    /// Where in the clip the player stopped, in seconds.
    stopped: Float = 0.0
}

root Clip

default view clip for Clip

/// Names the clip the window plays.
action play(c: Clip, source: Text) {
    c.source = source
}

/// Records where playback got to.
action reached(c: Clip, at: Float) {
    if c.first < 0.0 {
        c.first = at
    }
    c.at = at
    c.reports += 1
}

/// Records why the player could not show the clip, and where it stopped.
action failed(c: Clip, reason: Text, at: Float) {
    c.failure = reason
    c.stopped = at
}

/// The clip, playing, and a line once playback reported moving on from where it started.
view clip(c: Clip) {
    std.page(c) {
        if c.source != "" {
            ui.VideoPlayer(source: c.source, playing: true) {
                on progress(at: Float): reached(c, at)
                on fail(reason: Text, at: Float): failed(c, reason, at)
            }
        }
        if c.reports > 1 and c.at > c.first {
            text("Playback moved on") as moved
        }
        if c.failure != "" {
            text(c.failure) as failure
        }
    }
}
