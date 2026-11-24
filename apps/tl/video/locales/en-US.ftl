# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-video = Video

d-so-tilo-video-keywords = video, videos, player, movie, movies, film, films, play, watch, clip, clips, media, mp4, mkv, webm, mov, avi, video abspielen, videoplayer, film ansehen, filme, video, videos, abspielen, anschauen, wiedergabe, medienspieler, klipp

d-j4cx5fiu = Clip
    .doc = A video a person opened: what it is called and where its bytes play from.
    .noun = { $count ->
        [one] clip
       *[other] clips
    }
    .untitled = Untitled video

d-sit6wbwo = Title
    .doc = The video's name: the file it was opened as, extension included, so the player reads the container it holds.

d-vnxskfep = Source
    .doc = The address of its bytes: a file's content (`tilo-blob://…`), or a stream's URL.

d-dzmqpss7 = Playlist
    .doc = A playlist everyone in the workspace sees; its videos are the entries that name it.
    .noun = { $count ->
        [one] playlist
       *[other] playlists
    }

d-hktiqra6 = Name

d-vg4p5tbz = Playlist entry
    .doc = A video of a playlist.
    .noun = { $count ->
        [one] playlist entry
       *[other] playlist entries
    }

d-pmcgj5bo = Playlist

d-gofxf7oh = Clip

d-vg4p5tbz-filter-playlist = Playlist

d-vg4p5tbz-filter-clip = Clip

d-xfzvikx4 = Bookmark
    .doc = A moment of a video, a bookmark when it says nothing.
    .noun = { $count ->
        [one] bookmark
       *[other] bookmarks
    }
    .untitled = Bookmark

d-h4e7iq7t = Clip

d-cwamirwi = At
    .doc = Where in the video it stands, in seconds.

d-n323noym = Text

d-xfzvikx4-filter-clip = Clip

d-xfzvikx4-filter-at = At

d-pu5odurn = Library
    .doc = The videos opened in this workspace.
    .noun = { $count ->
        [one] library
       *[other] libraries
    }

d-cfgqumjg = Clips

d-7sp4sk7x = Playlists

d-ve6kofwh = Entries

d-srzszayp = Watch later
    .doc = Videos kept to watch later.

d-265jfqy5 = Bookmarks

d-jcuq2e4s = Playing
    .doc = What plays, and where it stands: the shared session Video and Music keep, once this app played.

d-eajmuh6i = Sessions
    .doc = Every session this app made, kept as the resources they are; `playing` names the one.

d-pu5odurn-filter-playing = Playing

d-hksyhpqm = Open video
    .doc = Opens a video file: the file a person picked, or one Files handed over, and plays it where it was left.
    .did = opened a video

d-hksyhpqm-file = File

d-7vndi4ty = Create playlist
    .doc = Starts an empty playlist.

d-7vndi4ty-name = Name

d-qx6xvn26 = Delete playlist
    .doc = Deletes a playlist and the entries that name it.

d-qx6xvn26-pl = Pl

d-gy4thduh = Add to playlist
    .doc = Adds `c` to `pl`, once.

d-gy4thduh-pl = Pl

d-gy4thduh-c = C

d-ocgz6qq7 = Add to watch later
    .doc = Keeps `c` to watch later, once.

d-ocgz6qq7-c = C

d-fkgh2zyf = Add bookmark
    .doc = Bookmarks the moment `at` of `c`, with `text` when the person wrote something.

d-fkgh2zyf-c = C

d-fkgh2zyf-at = At

d-fkgh2zyf-text = Text

d-5edgge6m = Delete bookmark
    .doc = Deletes a bookmark or note.

d-5edgge6m-b = B

d-jowwlxkl = Open dropped
    .doc = Opens the videos a person dropped on the window: each file becomes its clip, and the first plays in the shared session.
    .did = opened a video
    .w-drop-a-video-file = Drop a video file

d-jowwlxkl-files = Files

d-qr5yt6i6 = Session
    .doc = The session this app plays through: the one it has, or one made now.

d-o42ih4gs = Toggle playing
    .doc = Plays or pauses the session when it is showing `c`: what the P key and the window's player control run, without needing the root passed in.

d-23vsff7g = Play clip
    .doc = Plays `c` in the shared session, from where it was left (the beginning the first time): what the window's Play button, the palette and an agent's `play` run.
    .did = played a video

d-23vsff7g-c = C

d-4hyldob5 = Play from the beginning
    .doc = Plays `c` from its beginning, as a person who watched it before chooses.

d-4hyldob5-c = C

d-zzrlp3fd = Open dropped~present

d-yydyqfvk = Library
    .w-drop-a-video = Drop a video
    .w-any-video-file = Any video file
    .w-play-a-video = Play a video
    .w-open-a-video-file-from-your = Open a video file from your device.
    .w-open-video = Open video

d-6o4owyb6 = Video
    .w-open-video = Open video
    .w-library = Library
    .w-play-from-the-beginning = Play from the beginning
    .w-bookmark-this-moment = Bookmark this moment
    .w-watch-later = Watch later
    .w-queue = Queue
    .w-shared = Shared
    .w-nothing-queued = Nothing queued
    .w-playlists = Playlists
    .w-new-playlist = New playlist
    .w-nothing-kept-for-later = Nothing kept for later
    .w-bookmarks = Bookmarks
    .w-no-bookmarks = No bookmarks
    .w-play = Play
    .w-can-t-play-this-video = Can't play this video
