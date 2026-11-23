# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-tube = YouTube

d-m5wewovo = Channel
    .doc = A channel someone publishes videos on.
    .noun = { $count ->
        [one] channel
       *[other] channels
    }

d-26ctwf34 = Key

d-ya2u4cw6 = Name

d-woyix7h2 = Handle

d-ah5nkqv7 = Subscribers

d-2qxzpxur = Avatar

d-d4mzmvrr = Banner

d-rzhtvjsv = Description

d-lfopp7e7 = Verified

d-2ap24kjm = Found for
    .doc = The search that found it, empty for a channel the person's subscriptions or a page brought in.

d-albed4jx = Video
    .doc = A video, as the service says it is, with what the person did with it: liked, put on the watch queue, and where they left off (the history entry says that).
    .noun = { $count ->
        [one] video
       *[other] videos
    }

d-irzy3c7m = Key

d-fpxk5w7b = Title

d-mhkply6c = Channel key

d-2mklwbfq = Channel name

d-lfky4erv = Channel avatar

d-txtos5p6 = Views

d-2ruyhh5f = Published

d-mvlf65b5 = Duration
    .doc = How long it runs, in seconds.

d-pfdoae2w = Description

d-ddfiordt = Likes

d-l25oorez = Thumbnail

d-pkbzjs7l = Stream
    .doc = The address its bytes play from, as the service answered: empty until it is asked.

d-jfam7tjf = Live
    .doc = Whether it is a live stream rather than a recording.

d-bifohbpv = Liked
    .doc = The person liked it.

d-4mojz3hw = Trending
    .doc = The trending feed listed it.

d-5o5fpqgh = Found for
    .doc = The search that found it, empty for a video a feed or a page brought in.

d-albed4jx-filter-liked = Liked

d-albed4jx-filter-trending = Trending

d-mmzhfe3o = Playlist
    .doc = A playlist: videos in an order its maker chose.
    .noun = { $count ->
        [one] playlist
       *[other] playlists
    }

d-ow7glfos = Key

d-ljyc7mx2 = Title

d-bzfmd6pg = Channel key

d-zvfpfva3 = Channel name

d-6cllrl7x = Video count

d-4xvzshd7 = Thumbnail

d-dge65utd = Video keys
    .doc = The keys of the videos it holds, in its maker's order: none until the service answered what it holds.

d-p3yw6p2h = Found for
    .doc = The search that found it.

d-ebglq5za = Subscription
    .doc = A channel the person subscribed to, by its key: membership, so two people subscribing at once keep it once, and the feed reads the channels' videos.
    .noun = { $count ->
        [one] subscription
       *[other] subscriptions
    }

d-slhmwv67 = Key
    .doc = The channel's key, as the service names it.

d-ynf67l6e = Name

d-r6snpxjs = Queued video
    .doc = One video someone put on the workspace's watch queue.
    .noun = { $count ->
        [one] queued video
       *[other] queued videos
    }

d-o3xolmld = Key
    .doc = The video's key, as the service names it.

d-qynvz25p = Playback prefs
    .doc = How one member likes to watch.
    .noun = { $count ->
        [one] playback pref
       *[other] playback prefs
    }

d-5twttwwn = Member

d-vze2yftu = Autoplay
    .doc = Play the next video when this one ends.

d-5bpxm33f = Speed
    .doc = How fast it plays: 0.25×, 0.5×, 1×, 1.5×, 2× or 3×.

d-lesyml4z = Quality
    .doc = The tallest picture they want, as YouTube names it (`1080p`, `720p`, `480p`), or `auto` for the best that fits.

d-u3fgpv6e = Captions
    .doc = Show the video's captions when it has them.

d-qynvz25p-filter-member = Member

d-qynvz25p-filter-autoplay = Autoplay

d-qynvz25p-filter-speed = Speed

d-qynvz25p-filter-captions = Captions

d-kgojom6f = Watch entry
    .doc = One video the person watched: when, and where they left off.
    .noun = { $count ->
        [one] watch entry
       *[other] watch entries
    }

d-noppuxpb = Key
    .doc = The video's key, as the service names it.

d-stuii7l7 = Title

d-j37y2bbw = Channel name

d-7mtvj3xd = Member
    .doc = Who watched it.

d-dmm6f63t = Watched

d-omff5ciz = Progress
    .doc = How far in they were, as a share of its length; zero when they watched it to the end.

# refined
d-thwdswkk = YouTube
    .doc = What the client holds: the channels, videos and playlists the service answered with, the person's subscriptions, their watch history, and how the last search stands.
    .noun = { $count ->
        [one] channel
       *[other] channels
    }

d-olrung72 = Title
    .doc = What the client is called.

d-ihhzrm3f = Channels

d-qnfjkti5 = Videos

d-v5sh2bpr = Playlists

d-iputtmua = Subscriptions

d-jgjqgesf = Queue
    .doc = The workspace's queue of videos to watch, oldest first.

d-hv25j74h = History

d-epgpppzy = Playback
    .doc = Each member's own playback preferences.

d-bqesicxq = Search text
    .doc = What the last search asks for, as it was typed.

d-hcmy6dp7 = Search state
    .doc = How the search of `search_text` stands: `searching`, `done`, `failed` or empty.

d-zjw3anaz = Search trouble
    .doc = Why the last search failed; empty while it went through.

d-nnvco6w3 = Feed state
    .doc = How the subscriptions feed stands: `loading`, `done`, `failed` or empty.

d-r3ksw6bn = Trouble
    .doc = Why the client is not in step now: YouTube cannot be reached, or what it said could not be saved; empty while it is.

d-qv7t2mza = Online
    .doc = Whether YouTube answered the last time it was asked.

d-thwdswkk-filter-online = Online

d-tmam46un = Subscribe
    .doc = Subscribes to the channel, or unsubscribes: the button says which it will do.

d-tmam46un-subscribed = Subscribed

d-hcgvcroq = Subscribe
    .doc = Subscribes to the channel, or takes the subscription back.
    .w-unsubscribe = Unsubscribe
    .w-subscribe = Subscribe

d-weyz2uyl = Like
    .doc = Likes the video, or takes the like back.
    .w-unlike = Unlike
    .w-like = Like

d-weyz2uyl-liked = Liked

d-yw4tuoq4 = Watch later
    .doc = Puts the video on the workspace's watch queue, or takes it off.
    .w-remove-from-the-queue = Remove from the queue
    .w-watch-later = Watch later

d-yw4tuoq4-saved = On the watch queue

d-4bg5hmr2 = Play all
    .doc = Plays the queue from its oldest video: what was put on it first plays first.

d-6ux63onw = Play
    .doc = Plays the video, whether or not it was played before.

d-s334bmn6 = Resume
    .doc = Plays the video from where the person left off, when they did not finish it.

d-jxokfijg = Progress
    .doc = Records how far into the video the person is, and that they watched it now: it moves to the front of the history, and asks the service for the address its bytes play from when that is not known yet.

d-jxokfijg-at = At

d-oqumfxkj = Delete from history
    .doc = Takes one video out of the person's own history.

d-wzt3qvyb = Delete history
    .doc = Deletes the person's own watch history, and the entries carried from before it recorded members, which this history shows them; nobody else's.

d-4hpmg3yw = Try again
    .doc = Asks the service for where the video's bytes play from again, after the player could not show them: the address the client holds is the service's alone to write, and the answer it gives now is what the player opens.

d-edbp7ej2 = Refresh
    .doc = Asks the service for what the playlist holds, in its maker's order.

d-7lfzugkh = Play all
    .doc = Plays the playlist from its first video.

d-lcvxshh4 = Autoplay next video
    .doc = Each of these keeps one of the person's own playback preferences: the record is theirs, made the first time they set one, and every other member reads their own.

d-lcvxshh4-on = On

d-3r5wepfg = Playback speed
    .doc = How fast videos play for the person: 0.25 to 3, 1 for the usual speed.

d-3r5wepfg-speed = Speed

d-lzop6dva = Preferred quality
    .doc = The tallest picture the person wants, as YouTube names it; `auto` takes the best that fits.

d-lzop6dva-quality = Quality

d-fmurpek2 = Captions
    .doc = Whether videos show their captions for the person.

d-fmurpek2-on = On

d-6lgqby4s = Search
    .doc = Searches YouTube: the service answers with channels, playlists and videos, written where the search view lists them.
    .w-type-something-to-search-for = Type something to search for

d-6lgqby4s-query = Query

d-vhf33pa7 = Leave search
    .doc = Leaves the search the feed is showing for the place the person chose: the results stay where the service wrote them (found again by the query that found them), and the client shows the place chosen.
    .w-no-search-is-showing = No search is showing

d-hpjdxg6k = Refresh
    .doc = Asks the service for the newest videos of each subscribed channel: what the subscriptions feed shows.

d-wx26izlg = Refresh
    .doc = Asks the service for the channel's videos.

d-khblapap = Refresh trending
    .doc = Asks the service for the videos it lists as trending.

d-mwai3gvn = Open link
    .doc = Opens a link of the scheme this client claims: the video (`tube:watch?v=<key>`), channel (`tube:channel?key=<key>`) or playlist (`tube:playlist?key=<key>`) it names, or the search it asks for (`tube:search?q=<query>`).

d-mwai3gvn-link = Link

d-t3cwabaw = Carry native tube
    .doc = What the native Tube kept before this app took its id over, carried into this one once: the channels subscribed to, the videos liked, the watch queue, and the watch history with when each video was watched.

d-yc4rjoeg = Play again
    .doc = Plays the video a history entry names, from where the person left off.

d-yc4rjoeg-h = H

d-qgtk64v3 = Item clock
    .w-live = Live

d-5ozzrcey = Toggle subscription~present

d-2nggfmec = Like~present

d-fg57jp5b = Watch later~present

d-h6g3amy7 = Search~present

d-mufcicuu = Feed
    .w-youtube = YouTube
    .w-search-youtube = Search YouTube
    .w-subscriptions = Subscriptions
    .w-trending = Trending
    .w-you = You
    .w-watch-later = Watch later
    .w-liked-videos = Liked videos
    .w-history = History
    .w-nothing-on-the-watch-queue = Nothing on the watch queue
    .w-videos-you-keep-for-later-wait = Videos you keep for later wait here, oldest first
    .w-videos-oldest-first = { $len } videos, oldest first
    .w-play-all = Play all
    .w-nothing-liked-yet = Nothing liked yet
    .w-nothing-trending-yet = Nothing trending yet
    .w-refresh-trending = Refresh trending
    .w-no-subscriptions-yet = No subscriptions yet
    .w-nothing-new-yet = Nothing new yet
    .w-refresh = Refresh
    .w-can-t-save-what-fetched = Can't save what { $name } fetched
    .w-can-t-reach = Can't reach { $name }
    .w-last-fetched = { $reason }. Last fetched { $fetched }
    .w-try-again = Try again

d-dfz6u5sa = Video grid

d-nxne4q6s = Search results
    .w-search-youtube = Search YouTube

d-tbcsx2lp = Search found
    .w-searching-youtube = Searching YouTube
    .w-the-search-failed = The search failed
    .w-search-youtube = Search YouTube
    .w-channels-playlists-and-videos-by-name = Channels, playlists and videos, by name
    .w-nothing-found = Nothing found
    .w-nothing-matches = Nothing matches { $search_text }
    .w-channels = Channels
    .w-playlists = Playlists
    .w-videos = Videos

d-yf5l4ysw = Watched page
    .w-history = History

d-lhrpz5jd = History list
    .w-nothing-watched-yet = Nothing watched yet

d-yh23kjna = Channel page
    .w-unsubscribe = Unsubscribe
    .w-subscribe = Subscribe
    .w-refresh = Refresh
    .w-s-banner = { $name }'s banner
    .w-videos = { $len } videos
    .w-about = About
    .w-videos-2 = Videos
    .w-no-videos-here-yet = No videos here yet

d-w7mpbzm4 = Video page
    .w-play = Play
    .w-this-video-won-t-play-here = This video won't play here
    .w-try-again = Try again
    .w-unsubscribe = Unsubscribe
    .w-subscribe = Subscribe
    .w-liked = Liked
    .w-like = Like
    .w-watch-later = Watch later
    .w-live = Live
    .w-description = Description
    .w-found-for = Found for { $found_for }

d-eijjevst = Playlist page
    .w-refresh = Refresh
    .w-videos = { $video_count } videos
    .w-play-all = Play all
    .w-nothing-shown-from-this-playlist-yet = Nothing shown from this playlist yet
    .w-its-videos-appear-once-youtube-says = Its videos appear once YouTube says what it holds

d-z642pnnw = Video row

d-uiu73od5 = Channel row

d-pgyjhoe2 = Watch row
