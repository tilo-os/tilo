# Tube — source locale (en-US).
# Counts arrive twice: `count` (a number, which picks the plural form) and
# `shown` (the number as Tube abbreviates it, such as 1.2K).

## Destinations
tube-nav-home = Home
tube-nav-trending = Trending
tube-nav-subscriptions = Subscriptions
tube-nav-queue = Queue
tube-nav-history = History
tube-nav-settings = Settings
tube-nav-name = Tube

## Header row and navigation
tube-clear-history = Clear history

## Counts and facts
tube-views = { $count ->
    [one] { $shown } view
   *[other] { $shown } views
}
tube-views-and-age = { $views }, { $age }
tube-watching = { $shown } watching
tube-subscribers = { $count ->
    [one] { $shown } subscriber
   *[other] { $shown } subscribers
}
tube-video-count = { $count ->
    [one] { $shown } video
   *[other] { $shown } videos
}
tube-dislikes = { $count ->
    [one] { $shown } dislike
   *[other] { $shown } dislikes
}
tube-likes = { $count ->
    [one] { $shown } like
   *[other] { $shown } likes
}
tube-replies = { $count ->
    [one] { $shown } reply
   *[other] { $shown } replies
}

## Home, Trending, Channels, History, Queue
tube-home-error = Home couldn't refresh
tube-offline-title = You're offline
tube-offline-message = Reconnect to load videos. Your queue and history are still here.
tube-unreached-title = Videos can't load here
tube-sources-no-relay = The video sources don't let web pages read them, and this site has no fetch proxy to relay them. Your queue and history are still here.
tube-sources-failed = No video source answered. Your queue and history are still here.
tube-home-empty-title = Nothing to show yet
tube-home-empty-message = Search for a video to start watching.
tube-search = Search
tube-search-for = Search YouTube for “{ $query }”
tube-trending-empty-title = Nothing trending yet
tube-trending-empty-message = Popular videos show here once the catalogue loads.
tube-subscriptions-empty-title = No subscriptions yet
tube-subscriptions-empty-message = Subscribe to creators and their new videos show here.
tube-latest-videos = Latest videos
tube-history-not-saving = Watch history isn't saving
tube-history-empty-title = Nothing watched yet
tube-history-empty-message = Videos you watch show up here.
tube-queue-empty-title = Your queue is empty
tube-queue-empty-message = Add videos from their watch page.
tube-queue-waiting = { $count ->
    [one] One more queued video will show once its details load.
   *[other] { $count } more queued videos will show once their details load.
}

## Playlists and creators
tube-playlist-missing = This playlist isn't available
tube-play-all = Play all
tube-videos = Videos
tube-creator-missing = This creator isn't available
tube-subscribe = Subscribe
tube-subscribed = Subscribed

## Watch page
tube-video-missing = This video isn't available
tube-resolution = Resolution
tube-quality-now = Quality, playing { $quality }
tube-playback-speed = Playback speed
tube-speed-value = { $rate }×
tube-captions = Captions
tube-captions-named = Captions, { $why }
tube-captions-off = Off
tube-captions-automatic = { $track } (auto-generated)
tube-captions-loading = loading…
tube-captions-not-offered = not offered by this video
tube-captions-unavailable = could not be loaded
tube-resume-at = Resume at { $time }
tube-skip-segment = Skip { $segment }
tube-like = Like
tube-like-count = Like, { $likes }
tube-add-to-queue = Add to queue
tube-remove-from-queue = Remove from queue
tube-copy-link = Copy link
tube-playback-settings = Playback settings
tube-comments = Comments
tube-comments-off = Comments are turned off in Settings.
tube-pinned = Pinned
tube-hearted = Liked by the creator
tube-up-next = Up next
tube-autoplay = Autoplay

## Settings
tube-playback = Playback
tube-autoplay-next = Autoplay next video
tube-show-comments = Show comments
tube-show-dislikes = Show dislike counts
tube-dislikes-note = Dislike counts are estimates from Return YouTube Dislike, based on the votes of its users.
tube-quality = Quality
tube-preferred-quality = Preferred quality
tube-quality-automatic = Automatic
tube-sponsorblock = SponsorBlock
tube-skip-sponsored = Skip sponsored segments
tube-sponsorblock-note = Segments are marked by the SponsorBlock community and shown in their colour on the timeline.
tube-segments = Segments
tube-dearrow = DeArrow
tube-dearrow-community = Use community titles and thumbnails
tube-dearrow-titles = Replace titles
tube-dearrow-thumbnails = Replace thumbnails

## Segment kinds and what Tube does with them
tube-segment-sponsor = Sponsor
tube-segment-sponsor-about = Paid promotion, paid referrals and direct advertisements.
tube-segment-selfpromo = Self-promo
tube-segment-interaction = Interaction reminder
tube-segment-interaction-about = Reminders to like, subscribe or follow, with no other content.
tube-segment-intro = Intro and intermission
tube-segment-intro-about = A stretch without content: a pause, an animation or an intro.
tube-segment-outro = End cards and credits
tube-segment-outro-about = Credits and end cards once the video is over.
tube-segment-preview = Preview and recap
tube-segment-preview-about = A recap of earlier episodes or a preview of what comes next.
tube-segment-music-offtopic = Non-music section
tube-segment-music-offtopic-about = The parts of a music video that are not music.
tube-segment-filler = Filler tangent
tube-segment-filler-about = Tangents added only to make the video longer.

## Device sync
tube-device-sync = Device sync
tube-sync-unavailable-now = Device sync isn't available right now.
tube-sync-not-built = This build of Tube doesn't include Grayjay device sync.
tube-sync-locked-title = Device sync is unavailable
tube-sync-locked-message = Unlock the desktop's secret store, then reopen Tube to pair Grayjay devices.
tube-sync-now = Sync now
tube-pair-title = Pair a device
tube-pair-how = In Grayjay, open Sync and choose Add device, then paste its invite here.
tube-pair-placeholder = Pairing invite from Grayjay
tube-pair-name = Grayjay pairing invite
tube-pair-device = Pair device
tube-pending-title = Waiting for approval
tube-pending-check = Approve it if Grayjay shows the fingerprint { $fingerprint }
tube-approve-device = Approve device
tube-approved-devices = Approved devices
tube-no-approved-devices = No approved devices yet. Pairing needs a confirmation on both devices.
tube-not-synced = Not synced yet
tube-sync-failed = Couldn't sync: { $error }
tube-last-synced = Last synced { $when }
tube-revoke-device = Revoke device
tube-sync-ready = Ready
tube-sync-pairing = Pairing
tube-sync-syncing = Syncing
tube-sync-attention = Needs attention

## Notices
tube-notice-link-copied = Link copied: { $url }
tube-notice-video-gone = That video is no longer available
tube-notice-creator-gone = That creator is no longer available
tube-notice-playlist-gone = That playlist is no longer available
tube-unplayable = This video won't play here right now
tube-unplayable-youtube-said = YouTube refused to play it: "{ $reason }"
tube-unplayable-no-source = None of Tube's sources had a stream for it.
tube-try-again = Try again
tube-live-in-app = Live streams play in the Tilo app for now
tube-notice-playback-limit = Playback limit reached on this host. Try again later.
tube-notice-no-downloads = Tube can't save videos for offline viewing
tube-notice-pairing-busy = Device pairing is busy or unavailable
tube-notice-sync-busy = Device sync is busy or unavailable
tube-notice-skipped-to = Skipped to { $time }
tube-notice-skipped-segment = Skipped { $segment } ({ $start }–{ $end })
tube-notice-history-cleared = Watch history cleared for this profile
tube-notice-pair-first = Paste a pairing invite first, or unlock the desktop's secret store
tube-notice-sync-unavailable = Device sync is unavailable

## History: what each change to the shared library did

tube-library-likes = { $op ->
    [remove] Took back a like
   *[other] Liked a video
}
tube-library-queue = { $op ->
    [remove] Removed a video from the shared queue
   *[other] Added a video to the shared queue
}
tube-library-subscriptions = { $op ->
    [remove] Unsubscribed from a creator
   *[other] Subscribed to a creator
}
tube-library-comment-likes = { $op ->
    [remove] Took back a comment like
   *[other] Liked a comment
}
tube-library-replies = Changed the replies
tube-reply-to-comment = Replied { $text }
tube-retract = Retracted the reply { $item }

## History: the person's own watch history

tube-record = Watched a video
tube-import = Brought in videos watched on another device
tube-forget = Removed a video from the watch history
tube-clear = Cleared the watch history
