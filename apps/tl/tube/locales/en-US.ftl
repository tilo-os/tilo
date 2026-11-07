# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-lang-tube = YouTube

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
    .doc = A video, as the service says it is, with what the person did with it: liked, kept for later, and where they left off (the history entry says that).
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

d-vvtm26ea = Watch later
    .doc = The person kept it to watch later.

d-4mojz3hw = Trending
    .doc = The trending feed listed it.

d-5o5fpqgh = Found for
    .doc = The search that found it, empty for a video a feed or a page brought in.

d-albed4jx-filter-liked = Liked
    .doc = The person liked it.

d-albed4jx-filter-watch_later = Watch later
    .doc = The person kept it to watch later.

d-albed4jx-filter-trending = Trending
    .doc = The trending feed listed it.

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

d-dmm6f63t = Watched

d-omff5ciz = Progress
    .doc = How far in they were, as a share of its length; zero when they watched it to the end.

d-thwdswkk = You tube
    .doc = What the client holds: the channels, videos and playlists the service answered with, the person's subscriptions, their watch history, and how the last search stands.
    .noun = { $count ->
        [one] you tube
       *[other] you tubes
    }

d-olrung72 = Title
    .doc = What the client is called.

d-ihhzrm3f = Channels

d-qnfjkti5 = Videos

d-v5sh2bpr = Playlists

d-iputtmua = Subscriptions

d-hv25j74h = History

d-bqesicxq = Search text
    .doc = What the last search asks for, as it was typed.

d-hcmy6dp7 = Search state
    .doc = How the search of `search_text` stands: `searching`, `done`, `failed` or empty.

d-zjw3anaz = Search trouble
    .doc = Why the last search failed; empty while it went through.

d-nnvco6w3 = Feed state
    .doc = How the subscriptions feed stands: `loading`, `done`, `failed` or empty.

d-hycswkws = Feed trouble
    .doc = Why the feed could not be fetched; empty while it went through.

d-tmam46un = Subscribe
    .doc = Subscribes to the channel, or unsubscribes: the button says which it will do.

d-hcgvcroq = Subscribe
    .doc = Subscribes to the channel, or takes the subscription back.

d-weyz2uyl = Like
    .doc = Likes the video, or takes the like back.

d-yw4tuoq4 = Watch later
    .doc = Keeps the video to watch later, or takes it off the list.

d-6ux63onw = Play
    .doc = Plays the video, whether or not it was played before.

d-s334bmn6 = Resume
    .doc = Plays the video from where the person left off, when they did not finish it.

d-jxokfijg = Progress
    .doc = Records how far into the video the person is, and that they watched it now: it moves to the front of the history, and asks the service for the address its bytes play from when that is not known yet.

d-oqumfxkj = Delete from history
    .doc = Takes one video out of the history.

d-wzt3qvyb = Delete history
    .doc = Deletes the watch history: everything the person watched.

d-6lgqby4s = Search
    .doc = Searches YouTube: the service answers with channels, playlists and videos, written where the search view lists them.

d-vhf33pa7 = Leave search
    .doc = Leaves the search the feed is showing for the place the person chose: the results stay where the service wrote them (found again by the query that found them), and the client shows the place chosen.

d-hpjdxg6k = Refresh
    .doc = Asks the service for the newest videos of each subscribed channel: what the subscriptions feed shows.

d-wx26izlg = Refresh
    .doc = Asks the service for the channel's videos.

d-khblapap = Trending
    .doc = Asks the service for the videos it lists as trending.

d-yc4rjoeg = Play again
    .doc = Plays the video a history entry names, from where the person left off.

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
    .w-nothing-kept-for-later = Nothing kept for later
    .w-the-videos-you-keep-are-here = The videos you keep are here
    .w-nothing-liked-yet = Nothing liked yet
    .w-the-videos-you-like-are-here = The videos you like are here
    .w-nothing-trending-yet = Nothing trending yet
    .w-ask-youtube-what-is-trending = Ask YouTube what is trending
    .w-no-subscriptions-yet = No subscriptions yet
    .w-a-channel-s-page-subscribes-to = A channel's page subscribes to it, and its newest videos arrive here
    .w-nothing-new-yet = Nothing new yet
    .w-ask-youtube-for-the-newest-videos = Ask YouTube for the newest videos of the channels you follow
    .w-refresh = Refresh
    .w-youtube-could-not-be-reached = YouTube could not be reached

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
    .w-the-videos-you-watch-are-listed = The videos you watch are listed here

d-yh23kjna = Channel page
    .w-unsubscribe = Unsubscribe
    .w-subscribe = Subscribe
    .w-refresh = Refresh
    .w-s-banner = { $name }'s banner
    .w-videos = { $len } videos
    .w-about = About
    .w-videos-2 = Videos
    .w-no-videos-here-yet = No videos here yet
    .w-ask-youtube-for-this-channel-s = Ask YouTube for this channel's videos

d-w7mpbzm4 = Video page
    .w-play = Play
    .w-unsubscribe = Unsubscribe
    .w-subscribe = Subscribe
    .w-liked = Liked
    .w-like = Like
    .w-watch-later = Watch later
    .w-live = Live
    .w-description = Description
    .w-found-for = Found for { $found_for }

d-eijjevst = Playlist page
    .w-videos = { $video_count } videos
    .w-nothing-here-yet = Nothing here yet
    .w-the-playlist-s-videos-arrive-when = The playlist's videos arrive when it is opened

d-z642pnnw = Video row

d-uiu73od5 = Channel row

d-pgyjhoe2 = Watch row
