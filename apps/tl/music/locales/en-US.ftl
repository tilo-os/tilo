# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-music = Music

d-sxv3fcr6 = Track
    .doc = A song: one the person opened from a device, or one the catalogue named.
    .noun = { $count ->
        [one] track
       *[other] tracks
    }
    .untitled = Unknown song

d-wapyyydf = Key

d-zpnajesf = Title

d-pzaz5i7w = Artist

d-kyegi6xm = Artist key

d-j67d2q4o = Album

d-ojqrhkks = Album key

d-2sm6xpgo = Seconds
    .doc = How long it runs, in seconds; 0 until the player or the catalogue says.

d-wy2gg3hw = Plays

d-yaujsp7u = Cover

d-hlfttyoj = Found for
    .doc = The search that found it, so a query's rows are its own.

d-47b7sivc = Stream
    .doc = Where the bytes play from, as the catalogue answered.

d-fdkr4wxf = Offline
    .doc = Where kept bytes are, as this device holds them (`tilo-blob://…`); empty until kept.

d-2hvaqxk2 = Lyrics
    .doc = Its words, as LRC; `lyrics_state` says whether they are timed.

d-4hjvzjcl = Lyrics state

d-wt3jlodv = Liked

d-sxv3fcr6-filter-liked = Liked

d-rcskaao2 = Album
    .doc = An album the catalogue named.
    .noun = { $count ->
        [one] album
       *[other] albums
    }
    .untitled = Unknown album

d-xlmick3d = Key

d-ui7q6nc2 = Title

d-c36cztfw = Artist

d-2px6jkdl = Artist key

d-aecqneo6 = Year

d-aajvirg5 = Release kind
    .doc = `album`, `single`, `ep` or `release`.

d-lzsbpa3s = Cover

d-hsjkx4hy = Found for

d-6ygyv2n2 = In library
    .doc = Whether the person keeps it in their library.

d-rcskaao2-filter-in_library = In library

d-hgu75iwl = Artist
    .doc = An artist the catalogue named.
    .noun = { $count ->
        [one] artist
       *[other] artists
    }
    .untitled = Unknown artist

d-a3jwtsne = Key

d-aht5vji7 = Name

d-p7suhaoo = Subscribers

d-xmevyzrv = Avatar

d-aazaiqyu = Found for

d-wkrm32am = Followed

d-hgu75iwl-filter-followed = Followed

d-lhfreres = Playlist
    .doc = A playlist the catalogue publishes.
    .noun = { $count ->
        [one] playlist
       *[other] playlists
    }
    .untitled = Untitled playlist

d-zliedtfz = Key

d-rtkxqj2p = Title

d-htibtwee = Subtitle

d-4rfi4yqu = Track count

d-qdbwyy7o = Cover

d-ac7ep6rb = Found for

d-lhfreres-filter-track_count = Track count

d-isg2xifs = Section
    .doc = One titled section of the catalogue's home page.
    .noun = { $count ->
        [one] section
       *[other] sections
    }
    .untitled = Section

d-jsrmddga = Key

d-2kqqdved = Title

d-lp6qsyhj = Order

d-isg2xifs-filter-order = Order

d-agd3hm25 = Suggestion
    .doc = One row of the home page: what the catalogue suggests, under the section it put it in.
    .noun = { $count ->
        [one] suggestion
       *[other] suggestions
    }
    .untitled = Suggestion

d-qh34h2ch = Key

d-xh6birha = Section

d-a5bkjkyq = Kind
    .doc = What the row names: `track`, `album`, `artist` or `playlist`.

d-gqxd4uhg = Target
    .doc = The key of the thing it names.

d-5a3o3zds = Title

d-oeqw4xpb = Subtitle

d-5gpg6lqq = Cover

d-bi4htdya = Mixtape
    .doc = A playlist the person made: their songs, in their order, with a cover they chose.
    .noun = { $count ->
        [one] mixtape
       *[other] mixtapes
    }
    .untitled = New playlist

d-5d2oxk4b = Name

d-dombkrij = Tracks

d-jonjr2zv = Cover

d-tkebe3bj = Taste
    .doc = What the person likes and what the recommendations may draw on, shared by the workspace.
    .noun = { $count ->
        [one] taste
       *[other] tastes
    }

d-biz2rcqc = Similar artists
    .doc = Ask for albums by artists the person already likes.

d-nhxvxvrt = Followed artists
    .doc = Ask for more by the artists followed.

d-fygjzxca = Liked songs
    .doc = Ask for songs like the ones liked.

d-splmmoir = Saved albums
    .doc = Ask for albums saved.

d-gfhj3dh4 = Dismissed
    .doc = Artists the person said to leave out.

d-oa6ryv4n = Preferred
    .doc = Artists the person said to bring more of.

d-tkebe3bj-filter-similar_artists = Similar artists

d-tkebe3bj-filter-followed_artists = Followed artists

d-tkebe3bj-filter-liked_songs = Liked songs

d-tkebe3bj-filter-saved_albums = Saved albums

d-tkebe3bj-filter-dismissed = Dismissed

d-tkebe3bj-filter-preferred = Preferred

d-aymfw7yu = Library
    .doc = The workspace's music: the songs and the catalogue's pages, what the person made, what they like, what plays, and how the catalogue's fetch stands.
    .noun = { $count ->
        [one] library
       *[other] libraries
    }

d-hkr3ia6d = Name

d-cp5uymkx = Tracks

d-phkx3ocv = Albums

d-oarbmgph = Artists

d-bfjw6uca = Playlists

d-k6usb4n2 = Suggestions

d-qu6nxpq3 = Sections

d-bfayjji6 = Mixtapes

d-u4kuzao5 = Taste

d-scejauac = Playing
    .doc = What plays, and where it stands: the shared session Music and Video keep, once this app played.

d-tnkopk2m = Sessions
    .doc = Every session this app made, kept as the resources they are; `playing` names the one.

d-wout6rgv = Catalogue
    .doc = `youtube` or `off`: which catalogue the app asks.

d-olobgipa = Search state
    .doc = What the catalogue's last fetch is doing, and why it failed.

d-foqrtqa3 = Search trouble

d-w3c7fplo = Home state

d-gmizjqwj = Home trouble

d-vnufl2py = Trouble

d-fkvdcomi = Online

d-aymfw7yu-filter-taste = Taste

d-aymfw7yu-filter-playing = Playing

d-aymfw7yu-filter-online = Online

d-4n3tw3px = Start
    .doc = What the library starts with: the recommendations' switches on.

d-oefcidqo = Session
    .doc = The session this app plays through: the one it has, or one made now.

d-jjm34kdi = Open audio file
    .doc = Opens an audio file: the file a person picked, or one Files handed over, and plays it.
    .did = added a song

d-jjm34kdi-file = File

d-luhqjplo = Play song
    .doc = Plays one song, alone: what a row's press does.

d-7aatw4mh = Play album
    .doc = Plays an album's songs from the top.

d-i3sjiskz = Play mixtape
    .doc = Plays a mixtape's songs in the person's order.

d-akdhwldp = Add to queue
    .doc = Adds a song to what follows.

d-akdhwldp-t = T

d-uwlrca54 = Play next
    .doc = Queues a song right after what plays.

d-uwlrca54-t = T

d-b2wwy2bq = Lyrics now
    .doc = Asks for the words of what plays, keeping the session it asks for.

d-dw5iplh4 = Queue moved
    .doc = Moves the dragged row where it was dropped: the two ids its reorder carries.

d-dw5iplh4-p = P

d-dw5iplh4-ids = Ids

d-dy7fl53w = Like
    .doc = Likes a song, or takes the like back.

d-dy7fl53w-liked = Liked

d-bjppiep2 = Like playing
    .doc = Likes what plays.

d-bjppiep2-p = P

d-bjppiep2-liked = Liked

d-2f6h2snc = Keep offline
    .doc = Keeps a song on this device, for playing with no network.

d-54u4oyoo = Remove download
    .doc = Takes the kept copy away; the song still plays from the catalogue.

d-tel4nf6o = Save album
    .doc = Keeps an album in the library, or takes it out.

d-tel4nf6o-keep = Keep

d-kqfycazm = Follow artist
    .doc = Follows an artist.

d-kqfycazm-followed = Followed

d-zgwkhxmj = Search
    .doc = Asks the catalogue for the songs, albums, artists and playlists a query finds.
    .w-type-something-to-search-for = Type something to search for

d-zgwkhxmj-query = Query

d-e3bonqvl = Refresh
    .doc = Asks the catalogue for its home page.

d-yxg55cix = Refresh artist
    .doc = Asks the catalogue for an artist's page.

d-qayzlavt = Refresh
    .doc = Asks the catalogue for a playlist's songs.

d-f2kpza4x = Open suggestion
    .doc = Does what a suggestion on the home page names.

d-f2kpza4x-row = Row

d-txbarxog = Lyrics of
    .doc = Asks for what plays to be sung, into the song's own record.

d-txbarxog-p = P

d-p3qnaypk = Note failure
    .doc = Says why the player stopped.

d-p3qnaypk-reason = Reason

d-xfhbh7qm = New mixtape
    .doc = Makes a playlist of the person's own.

d-xfhbh7qm-name = Name

d-mxym7u3g = Mixtape add
    .doc = Adds a song to a mixtape.

d-mxym7u3g-t = T

d-4k4f6fox = Mixtape remove
    .doc = Takes a song out of a mixtape.

d-4k4f6fox-t = T

d-rmq2tjia = Pick cover
    .doc = Gives a mixtape the cover the person picks.

d-rmq2tjia-file = File

d-vkqi2loo = Set cover
    .doc = Sets a mixtape's cover to the picture the person chose.

d-vkqi2loo-file = File

d-cul6iynf = Set catalogue
    .doc = Chooses which catalogue the app asks.

d-cul6iynf-which = Which

d-ywyw4kwy = Update taste
    .doc = Turns one of the recommendations' switches.

d-ywyw4kwy-which = Which

d-ywyw4kwy-on = On

d-kftmajje = Dismiss artist
    .doc = Leaves an artist out of the recommendations, or brings them back.

d-kftmajje-a = A

d-7fxad2em = Prefer artist
    .doc = Asks for more of an artist.

d-7fxad2em-a = A

d-yyin4m4b = Library
    .w-music = Music
    .w-home = Home
    .w-search = Search
    .w-songs = Songs
    .w-albums = Albums
    .w-artists = Artists
    .w-yours = Yours
    .w-playlists = Playlists
    .w-liked = Liked
    .w-downloads = Downloads
    .w-recommendations = Recommendations
    .w-youtube-music = YouTube Music
    .w-can-t-save-what-fetched = Can't save what { $name } fetched
    .w-can-t-reach = Can't reach { $name }
    .w-last-fetched = { $reason }. Last fetched { $fetched }
    .w-try-again = Try again
    .w-nothing-liked-yet = Nothing liked yet
    .w-nothing-kept-for-offline = Nothing kept for offline

d-fl2vq3rn = Band
    .w-nothing-playing = Nothing playing

d-juhtgzza = Track
    .w-kept-offline = Kept offline
    .w-add-to-queue = Add to queue
    .w-play-next = Play next
    .w-remove-download = Remove download
    .w-lyrics = Lyrics

d-4oohdtzr = Album
    .w-this-album-has-no-songs-yet = This album has no songs yet

d-gpcmrwgr = Artist
    .w-songs = Songs
    .w-this-artist-has-no-songs-yet = This artist has no songs yet
    .w-albums = Albums

d-uvzkg6uq = Playlist
    .w-this-playlist-has-no-songs-yet = This playlist has no songs yet

d-sum4fpxx = Mixtape
    .w-cover = Cover
    .w-add-songs-to-this-playlist = Add songs to this playlist

d-wnohn7ep = Home page
    .w-find-music = Find music
    .w-search-the-catalogue-or-open-an = Search the catalogue, or open an audio file from this device.
    .w-open-audio-file = Open audio file
    .w-refresh = Refresh

d-x2xe6gcf = Search page
    .w-search-youtube-music = Search YouTube Music
    .w-search-the-catalogue = Search the catalogue
    .w-find-songs-albums-and-artists-with = Find songs, albums and artists with the system's search.
    .w-searching = Searching…
    .w-the-catalogue-did-not-answer = The catalogue did not answer
    .w-albums = Albums
    .w-artists = Artists
    .w-catalogue-playlists = Catalogue playlists

d-2kdq7ch4 = Track list

d-rmcz4kfi = Album grid
    .w-no-albums-yet = No albums yet

d-uuxbyy6e = Artist grid
    .w-no-artists-yet = No artists yet

d-5hj6kcvh = Mixtape list
    .w-no-playlists-of-your-own-yet = No playlists of your own yet
    .w-new-playlist = New playlist

d-6tpcfsjw = Taste page
    .w-recommendations = Recommendations
    .w-similar-artists = Similar artists
    .w-artists-followed = Artists followed
    .w-more-like-the-songs-liked = More like the songs liked
    .w-more-like-the-albums-saved = More like the albums saved
    .w-catalogue = Catalogue
    .w-youtube-music = YouTube Music
    .w-off = Off
    .w-spotify = Spotify
    .w-sign-in-on-spotify = Sign in on Spotify
    .w-i-signed-in = I signed in
    .w-previous = Previous
    .w-pause = Pause
    .w-play = Play
    .w-next = Next
    .w-refresh = Refresh
    .w-disconnect = Disconnect
    .w-playing-here = Playing here
    .w-play-here = Play here
    .w-connect-spotify = Connect Spotify

d-osqkr7ab = Queue
    .w-queue = Queue
    .w-nothing-queued = Nothing queued

d-pf6yvngz = Lyrics
    .w-no-lyrics-yet = No lyrics yet
    .w-ask-the-catalogue-for-the-words = Ask the catalogue for the words of what plays.
    .w-find-lyrics = Find lyrics
    .w-nothing-playing = Nothing playing

d-6oildalu = Search~present

d-iyodtvzu = Spotify device
    .doc = A place Spotify plays on: a phone, a computer, a speaker or the person's browser.
    .noun = { $count ->
        [one] device
       *[other] devices
    }
    .untitled = Unknown device

d-4ruem7bk = Key
    .doc = The device's id, as Spotify names it.

d-fysa7usi = Name

d-ajgkhxtc = Kind
    .doc = `computer`, `smartphone`, `speaker`, `tv`, `cast` or another Spotify names.

d-njju5rcv = Active
    .doc = Whether what plays plays here.

d-daqeango = Volume
    .doc = How loud it is, `0..=100`, when Spotify says.

d-cjoykprs = Devices
    .doc = The places Spotify plays on, and the person's sign-in to it.

d-d6agrwhu = Spotify state
    .doc = `off`, `connecting`, `connected` or `trouble`.

d-wzbxwvai = Spotify account

d-q6fvzfwi = Spotify product
    .doc = `premium`, `free`, `open` or empty, as Spotify says.

d-bnnshdna = Spotify trouble

d-rwf7lfs6 = Connect URL
    .doc = The page the person signs in on, while `spotify_state` says `connecting`.

d-gghnswba = Spotify active
    .doc = The device Spotify plays on, as its key; empty when nothing does.

d-sdkihe6k = Spotify now
    .doc = What plays there, as people read it.

d-22kwzmah = Spotify playing

d-sbgoce2w = Device

d-cpsa6nlq = Connect Spotify
    .doc = Connects the person's Spotify: the page they sign in on is shown in the Spotify section.

d-bgomdthz = I signed in
    .doc = Takes the sign-in the person gave on Spotify's page.

d-c3zailpi = Disconnect Spotify
    .doc = Forgets this device's Spotify sign-in.

d-lyxtlmu3 = Refresh devices
    .doc = Reads the places Spotify plays on.

d-s5u526ny = Transfer playback
    .doc = Moves what plays to `d`, from the beginning.

d-amt56qpq = Play on Spotify
    .doc = Plays `uri` on Spotify, where it plays: empty plays what was playing.

d-7vrw7iss = Pause on Spotify
    .doc = Pauses what plays on Spotify.

d-y4ceqqvs = Next on Spotify
    .doc = Plays what follows on Spotify.

d-moryk2vk = Previous on Spotify
    .doc = Plays what played before on Spotify.

d-clxyvglt = What plays on Spotify
    .doc = Reads what plays on Spotify now.

d-s5u526ny-d = Device

d-amt56qpq-uri = Address

