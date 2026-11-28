# Music auf Deutsch. Keys are the declarations' stable ids (app.ids); the source's words are written from the package's sources.


d-so-tilo-music = Music

d-sxv3fcr6 = Titel
    .doc = Ein Titel: einer, den die Person von ihrem Gerät geöffnet hat, oder einer, den der Katalog genannt hat.
    .noun = { $count ->
        [one] Titel
       *[other] Titel
    }
    .untitled = Unbekannter Titel

d-wapyyydf = Schlüssel

d-zpnajesf = Titel

d-pzaz5i7w = Künstler

d-kyegi6xm = Künstlerschlüssel

d-j67d2q4o = Album

d-ojqrhkks = Albumschlüssel

d-2sm6xpgo = Sekunden
    .doc = Wie lang er läuft, in Sekunden; 0, bis der Player oder der Katalog es sagt.

d-wy2gg3hw = Wiedergaben

d-yaujsp7u = Cover

d-hlfttyoj = Gefunden für
    .doc = Die Suche, die ihn gefunden hat, damit die Zeilen einer Anfrage ihre eigenen sind.

d-47b7sivc = Stream
    .doc = Woraus die Bytes spielen, wie der Katalog geantwortet hat.

d-fdkr4wxf = Offline
    .doc = Wo die behaltenen Bytes liegen, wie dieses Gerät sie hält (`tilo-blob://…`); leer, bis sie behalten sind.

d-2hvaqxk2 = Songtext
    .doc = Sein Text als LRC; `lyrics_state` sagt, ob er mitläuft.

d-4hjvzjcl = Songtext-Status

d-wt3jlodv = Gefällt mir

d-sxv3fcr6-filter-liked = Gefällt mir

d-rcskaao2 = Album
    .doc = Ein Album, das der Katalog genannt hat.
    .noun = { $count ->
        [one] Album
       *[other] Alben
    }
    .untitled = Unbekanntes Album

d-xlmick3d = Schlüssel

d-ui7q6nc2 = Titel

d-c36cztfw = Künstler

d-2px6jkdl = Künstlerschlüssel

d-aecqneo6 = Jahr

d-aajvirg5 = Veröffentlichungsart
    .doc = `album`, `single`, `ep` oder `release`.

d-lzsbpa3s = Cover

d-hsjkx4hy = Gefunden für

d-6ygyv2n2 = In der Bibliothek
    .doc = Ob die Person es in ihrer Bibliothek behält.

d-rcskaao2-filter-in_library = In der Bibliothek

d-hgu75iwl = Künstler
    .doc = Ein Künstler, den der Katalog genannt hat.
    .noun = { $count ->
        [one] Künstler
       *[other] Künstler
    }
    .untitled = Unbekannter Künstler

d-a3jwtsne = Schlüssel

d-aht5vji7 = Name

d-p7suhaoo = Abonnenten

d-xmevyzrv = Profilbild

d-aazaiqyu = Gefunden für

d-wkrm32am = Gefolgt

d-hgu75iwl-filter-followed = Gefolgt

d-lhfreres = Playlist
    .doc = Eine Playlist, die der Katalog veröffentlicht.
    .noun = { $count ->
        [one] Playlist
       *[other] Playlists
    }
    .untitled = Unbenannte Playlist

d-zliedtfz = Schlüssel

d-rtkxqj2p = Titel

d-htibtwee = Untertitel

d-4rfi4yqu = Titelanzahl

d-qdbwyy7o = Cover

d-ac7ep6rb = Gefunden für

d-lhfreres-filter-track_count = Titelanzahl

d-isg2xifs = Abschnitt
    .doc = Ein betitelter Abschnitt der Startseite des Katalogs.
    .noun = { $count ->
        [one] Abschnitt
       *[other] Abschnitte
    }
    .untitled = Abschnitt

d-jsrmddga = Schlüssel

d-2kqqdved = Titel

d-lp6qsyhj = Reihenfolge

d-isg2xifs-filter-order = Reihenfolge

d-agd3hm25 = Vorschlag
    .doc = Eine Zeile der Startseite: was der Katalog vorschlägt, unter dem Abschnitt, in den er es gestellt hat.
    .noun = { $count ->
        [one] Vorschlag
       *[other] Vorschläge
    }
    .untitled = Vorschlag

d-qh34h2ch = Schlüssel

d-xh6birha = Abschnitt

d-a5bkjkyq = Art
    .doc = Was die Zeile nennt: `track`, `album`, `artist` oder `playlist`.

d-gqxd4uhg = Ziel
    .doc = Der Schlüssel dessen, was sie nennt.

d-5a3o3zds = Titel

d-oeqw4xpb = Untertitel

d-5gpg6lqq = Cover

d-bi4htdya = Eigene Playlist
    .doc = Eine Playlist, die die Person gemacht hat: ihre Titel, in ihrer Reihenfolge, mit einem Cover, das sie gewählt hat.
    .noun = { $count ->
        [one] Eigene Playlist
       *[other] Eigene Playlists
    }
    .untitled = Neue Playlist

d-5d2oxk4b = Name

d-dombkrij = Titel

d-jonjr2zv = Cover

d-tkebe3bj = Geschmack
    .doc = Was der Person gefällt und woraus die Empfehlungen schöpfen dürfen, geteilt vom Workspace.
    .noun = { $count ->
        [one] Geschmack
       *[other] Geschmäcke
    }


d-nhxvxvrt = Gefolgte Künstler
    .doc = Mehr von den gefolgten Künstlern erfragen.

d-fygjzxca = Gefällte Titel
    .doc = Titel erfragen, die den gefällten ähneln.

d-splmmoir = Gespeicherte Alben
    .doc = Gespeicherte Alben erfragen.

d-gfhj3dh4 = Ausgeblendet
    .doc = Künstler, die die Person ausgelassen haben will.

d-oa6ryv4n = Bevorzugt
    .doc = Künstler, von denen die Person mehr haben will.


d-tkebe3bj-filter-followed_artists = Gefolgte Künstler

d-tkebe3bj-filter-liked_songs = Gefällte Titel

d-tkebe3bj-filter-saved_albums = Gespeicherte Alben

d-tkebe3bj-filter-dismissed = Ausgeblendet

d-tkebe3bj-filter-preferred = Bevorzugt

d-aymfw7yu = Bibliothek
    .doc = Die Musik des Workspaces: die Titel und die Seiten des Katalogs, was die Person gemacht hat, was ihr gefällt, was spielt und wie der Abruf des Katalogs steht.
    .noun = { $count ->
        [one] Bibliothek
       *[other] Bibliotheken
    }

d-hkr3ia6d = Name

d-cp5uymkx = Titel

d-phkx3ocv = Alben

d-oarbmgph = Künstler

d-bfjw6uca = Playlists

d-k6usb4n2 = Vorschläge

d-qu6nxpq3 = Abschnitte

d-bfayjji6 = Playlists

d-u4kuzao5 = Geschmack

d-scejauac = Wiedergabe
    .doc = Was spielt und wo es steht: die geteilte Sitzung, die Music und Video halten, sobald diese App gespielt hat.

d-tnkopk2m = Sitzungen
    .doc = Jede Sitzung, die diese App gemacht hat, als die Ressourcen behalten, die sie sind; `playing` nennt die eine.

d-wout6rgv = Katalog
    .doc = `youtube`, `spotify` oder `off`: welchen Katalog die App fragt.

d-olobgipa = Suchstatus
    .doc = Was der letzte Abruf des Katalogs tut und warum er scheiterte.

d-foqrtqa3 = Suchproblem

d-w3c7fplo = Startseitenstatus

d-gmizjqwj = Startseitenproblem

d-vnufl2py = Problem

d-jelhifn2 = Wiedergabeproblem
    .doc = Warum die Wiedergabe des Titels endete, in den Worten der Engine, und wo sie endete, in Sekunden: Der Hinweis nennt die Stelle und spielt von dort weiter.

d-bw27kdqh = Stelle

d-x6v7yq2x = Erneut angefragt
    .doc = Der Song, dessen Stream nach einem Abbruch mittendrin von selbst neu angefragt wurde: ein zweiter Abbruch wird der Person gesagt, statt endlos neu anzufragen.

d-fkvdcomi = Online

d-aymfw7yu-filter-taste = Geschmack

d-aymfw7yu-filter-playing = Wiedergabe

d-aymfw7yu-filter-online = Online

d-aymfw7yu-filter-player_at = Wiedergabestelle

d-4n3tw3px = Start
    .doc = Womit die Bibliothek beginnt: die Schalter der Empfehlungen an.

d-oefcidqo = Sitzung
    .doc = Die Sitzung, durch die diese App spielt: die, die sie hat, oder eine jetzt gemachte.

d-jjm34kdi = Audiodatei öffnen
    .doc = Öffnet eine Audiodatei: die Datei, die die Person gewählt hat, oder eine, die Dateien übergeben hat, und spielt sie.
    .did = added a song

d-jjm34kdi-file = Datei

d-ijpx3myl = Link öffnen
    .doc = Öffnet einen `music://`-Link: der Titel, den er benennt, wird gespielt, und das Album, der Künstler oder die Wiedergabeliste, das er benennt, öffnet sich auf seiner Seite.
d-ijpx3myl-link = Link

d-luhqjplo = Titel abspielen
    .doc = Spielt einen Titel allein ab: was ein Druck auf eine Zeile tut.

d-7aatw4mh = Album abspielen
    .doc = Spielt die Titel eines Albums von oben ab.

d-i3sjiskz = Playlist abspielen
    .doc = Spielt die Titel einer eigenen Playlist in der Reihenfolge der Person ab.

d-akdhwldp = Zur Warteschlange
    .doc = Fügt einen Titel dem hinzu, was folgt.

d-akdhwldp-t = Titel

d-iqfljhwb = Wartenden Titel abspielen
    .doc = Spielt einen Titel von seiner Stelle in der Warteschlange ab und lässt den Rest der Warteschlange, wie er steht.

d-iqfljhwb-t = Titel

d-uwlrca54 = Als Nächstes
    .doc = Stellt einen Titel direkt nach der Wiedergabe in die Warteschlange.

d-uwlrca54-t = Titel

d-b2wwy2bq = Songtext
    .doc = Fragt nach dem Text dessen, was spielt, und behält die Sitzung, nach der sie fragt.

d-dw5iplh4 = Warteschlange verschieben
    .doc = Verschiebt die gezogene Zeile dorthin, wo sie abgelegt wurde: die beiden Kennungen, die ihre Umsortierung mitbringt.

d-dw5iplh4-p = Wiedergabe

d-dw5iplh4-ids = Kennungen

d-dy7fl53w = Gefällt mir
    .doc = Gefällt einen Titel oder nimmt das Gefällt-mir zurück.

d-dy7fl53w-liked = Gefällt mir

d-bjppiep2 = Gefällt mir für die Wiedergabe
    .doc = Gefällt, was spielt.

d-bjppiep2-p = Wiedergabe

d-bjppiep2-liked = Gefällt mir

d-2f6h2snc = Offline speichern
    .doc = Behält einen Titel auf diesem Gerät, um ohne Netz zu spielen.

d-54u4oyoo = Download entfernen
    .doc = Nimmt die behaltene Kopie weg; der Titel spielt weiter aus dem Katalog.

d-tel4nf6o = Album speichern
    .doc = Behält ein Album in der Bibliothek oder nimmt es heraus.

d-tel4nf6o-keep = Behalten

d-kqfycazm = Künstler folgen
    .doc = Folgt einem Künstler.

d-kqfycazm-followed = Gefolgt

d-zgwkhxmj = Suchen
    .doc = Fragt den Katalog nach den Titeln, Alben, Künstlern und Playlists, die eine Anfrage findet.
    .w-type-something-to-search-for = Type something to search for

d-zgwkhxmj-query = Anfrage

d-e3bonqvl = Aktualisieren
    .doc = Fragt den Katalog nach seiner Startseite.

d-yxg55cix = Künstler aktualisieren
    .doc = Fragt den Katalog nach der Seite eines Künstlers.

d-qayzlavt = Aktualisieren
    .doc = Fragt den Katalog nach den Titeln einer Playlist.

d-f2kpza4x = Vorschlag öffnen
    .doc = Tut, was ein Vorschlag auf der Startseite nennt.

d-f2kpza4x-row = Zeile

d-txbarxog = Songtext
    .doc = Bittet darum, dass gesungen wird, was spielt, in den Record des Titels.

d-txbarxog-p = Wiedergabe

d-p3qnaypk = Fehler melden
    .doc = Sagt, warum der Player aufhörte, und wo, damit der Hinweis die Stelle nennt und von dort weiterspielt.

d-p3qnaypk-reason = Grund

d-p3qnaypk-at = Stelle

d-tc6nujg6 = Der Wiedergabe folgen
    .doc = Hält die Quelle der Sitzung auf der Adresse des Titels, damit die Live-Karte und ein anderes Gerät die zuletzt vom Katalog geantwortete Adresse spielen.

d-tc6nujg6-p = P

d-t5g7ut27 = Erneut versuchen
    .doc = Fragt den Katalog erneut nach der Adresse des Titels und spielt von der Stelle weiter, an der der Stream abbrach: ein Stream, den seine Quelle unterwegs nicht mehr lieferte, oder eine abgelaufene signierte Adresse setzt dort fort, statt von vorn zu beginnen.

d-xfhbh7qm = Neue Playlist
    .doc = Macht eine eigene Playlist der Person.

d-xfhbh7qm-name = Name

d-mxym7u3g = Titel hinzufügen
    .doc = Fügt einer eigenen Playlist einen Titel hinzu.

d-mxym7u3g-t = Titel

d-4k4f6fox = Titel entfernen
    .doc = Nimmt einen Titel aus einer eigenen Playlist.

d-4k4f6fox-t = Titel

d-rmq2tjia = Cover wählen
    .doc = Gibt einer eigenen Playlist das Cover, das die Person wählt.

d-rmq2tjia-file = Datei

d-vkqi2loo = Cover setzen
    .doc = Setzt das Cover einer eigenen Playlist auf das Bild, das die Person gewählt hat.

d-vkqi2loo-file = Datei

d-cul6iynf = Katalog wählen
    .doc = Wählt, welchen Katalog die App fragt.

d-cul6iynf-which = Welcher

d-ywyw4kwy = Empfehlung ändern
    .doc = Schaltet einen der Schalter der Empfehlungen.

d-ywyw4kwy-which = Welcher

d-ywyw4kwy-on = An

d-kftmajje = Künstler ausblenden
    .doc = Lässt einen Künstler aus den Empfehlungen oder holt ihn zurück.

d-kftmajje-a = Album

d-7fxad2em = Mehr von diesem Künstler
    .doc = Bittet um mehr von einem Künstler.

d-7fxad2em-a = Album

d-3j2slwyd = Empfehlungsgrund
    .w-because-you-follow = Weil du { $artist } folgst.
    .w-from-an-album-you-kept = Aus einem Album, das du behalten hast.
    .w-because-you-liked = Weil dir { $title } gefallen hat.
    .w-because-you-listen-to = Weil du { $kind } hörst.

d-m34xk232 = Spotify-Kontozeile
    .w-spotify = Spotify

d-pju67pre = Gerätezeile
    .w-playing-here = Spielt hier
    .w-spotify-device = Spotify-Gerät

d-33dcmprq = Anzahltext
    .w-1-song = 1 Titel
    .w-songs = { $n } Titel

d-tg3ye5r4 = Hörer
    .w-m-monthly-listeners = { $value } M monatliche Hörer
    .w-k-monthly-listeners = { $value } K monatliche Hörer
    .w-monthly-listeners = { $n } monatliche Hörer

d-yyin4m4b = Bibliothek
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
    .w-youtube-music = YouTube Music
    .w-can-t-save-what-fetched = Can't save what { $name } fetched
    .w-can-t-reach = Can't reach { $name }
    .w-last-fetched = { $reason }. Last fetched { $fetched }
    .w-nothing-liked-yet = Nothing liked yet
    .w-nothing-kept-for-offline = Nothing kept for offline
    .w-recommendations = Empfehlungen

d-fl2vq3rn = Player
    .w-nothing-playing = Nothing playing
    .w-the-stream-stopped-at = Der Stream stoppte bei { $stopped }
    .w-this-song-won-t-play-here = Dieser Titel lässt sich hier nicht abspielen
    .w-try-again = Erneut versuchen

d-juhtgzza = Titel
    .w-kept-offline = Kept offline
    .w-add-to-queue = Add to queue
    .w-play-next = Play next
    .w-remove-download = Remove download
    .w-lyrics = Lyrics
    .w-add-to-spotify-s-queue = In Spotifys Warteschlange

d-4oohdtzr = Album
    .w-this-album-has-no-songs-yet = This album has no songs yet

d-gpcmrwgr = Künstler
    .w-songs = Songs
    .w-this-artist-has-no-songs-yet = This artist has no songs yet
    .w-albums = Albums
    .w-more-of-this-artist = Mehr von diesem Künstler
    .w-not-this-artist = Diesen Künstler nicht
    .w-artists-like-them = Ähnliche Künstler

d-uvzkg6uq = Playlist
    .w-this-playlist-has-no-songs-yet = This playlist has no songs yet

d-sum4fpxx = Eigene Playlist
    .w-cover = Cover
    .w-add-songs-to-this-playlist = Add songs to this playlist
    .w-on-spotify = Auf Spotify
    .w-playlist-name = Name der Playlist
    .w-what-this-playlist-is-for = Wofür diese Playlist ist

d-wnohn7ep = Startseite
    .w-find-music = Find music
    .w-search-the-catalogue-or-open-an = Search the catalogue, or open an audio file from this device.
    .w-open-audio-file = Open audio file
    .w-refresh = Refresh
    .w-made-from-your-library = Aus deiner Bibliothek
    .w-play = Abspielen
    .w-hide = Ausblenden
    .w-more-liked-songs = Mehr gelikte Titel
    .w-more-saved-albums = Mehr gespeicherte Alben
    .w-more-playlists = Mehr Playlists
    .w-more-followed-artists = Mehr gefolgte Künstler

d-x2xe6gcf = Suche
    .w-search-youtube-music = Search YouTube Music
    .w-search-the-catalogue = Search the catalogue
    .w-find-songs-albums-and-artists-with = Find songs, albums and artists with the system's search.
    .w-searching = Searching…
    .w-the-catalogue-did-not-answer = The catalogue did not answer
    .w-albums = Albums
    .w-artists = Artists
    .w-catalogue-playlists = Catalogue playlists

d-2kdq7ch4 = Titelliste

d-rmcz4kfi = Alben
    .w-no-albums-yet = No albums yet

d-uuxbyy6e = Künstler
    .w-no-artists-yet = No artists yet

d-5hj6kcvh = Playlists
    .w-no-playlists-of-your-own-yet = No playlists of your own yet
    .w-new-playlist = New playlist

d-6tpcfsjw = Empfehlungen
    .w-artists-followed = Artists followed
    .w-artists-like-these = Künstler wie diese
    .w-what-plays-next-there = Was dort als Nächstes läuft
    .w-read-spotify-s-queue = Spotifys Warteschlange lesen
    .w-spotify-s-queue-has-not-been = Spotifys Warteschlange wurde noch nicht gelesen.
    .w-reconnect-spotify = Spotify neu verbinden
    .w-spotify-ended-the-sign-in-connect = Spotify hat die Anmeldung beendet. Verbinde dich neu, um weiter zu nutzen, was du dort behältst.
    .w-catalogue = Catalogue
    .w-youtube-music = YouTube Music
    .w-off = Off
    .w-spotify = Spotify
    .w-sign-in-on-spotify = Sign in on Spotify
    .w-i-signed-in = I signed in
    .w-previous = Previous
    .w-pause = Pause
    .w-next = Next
    .w-refresh = Refresh
    .w-disconnect = Disconnect
    .w-connect-spotify = Connect Spotify
    .w-what-suggestions-may-draw-on = Woraus Vorschläge schöpfen
    .w-songs-liked = Songs, die du magst
    .w-albums-kept = Alben, die du behalten hast
    .w-suggestions-come-from-these-choices-alone = Vorschläge kommen allein aus diesen Entscheidungen: kein Hörverlauf und keine Anfrage an einen Katalog.
    .w-more-of-these-artists = Mehr von diesen Künstlern
    .w-stop-preferring = Nicht mehr bevorzugen
    .w-hidden-songs = Verborgene Songs
    .w-suggest-this-again = Wieder vorschlagen
    .w-play = Abspielen
    .w-playing-here = Spielt hier
    .w-play-here = Hier abspielen

d-osqkr7ab = Warteschlange
    .w-queue = Warteschlange
    .w-nothing-queued = Nichts in der Warteschlange
    .w-move-up-in-the-queue = In der Warteschlange nach oben
    .w-this-item-is-already-first-in = Dieser Eintrag steht schon am Anfang der Warteschlange
    .w-move-down-in-the-queue = In der Warteschlange nach unten
    .w-this-item-is-already-last-in = Dieser Eintrag steht schon am Ende der Warteschlange
    .w-remove-from-queue = Aus der Warteschlange entfernen

d-pf6yvngz = Songtext
    .w-no-lyrics-yet = No lyrics yet
    .w-ask-the-catalogue-for-the-words = Ask the catalogue for the words of what plays.
    .w-find-lyrics = Find lyrics
    .w-nothing-playing = Nothing playing

d-6oildalu = Suchen~present

d-iyodtvzu = Spotify-Gerät
    .doc = Ein Ort, an dem Spotify spielt: ein Telefon, ein Computer, ein Lautsprecher oder der Browser der Person.
    .noun = { $count ->
        [one] Gerät
       *[other] Geräte
    }
    .untitled = Unbekanntes Gerät

d-4ruem7bk = Schlüssel
    .doc = Die Id des Geräts, wie Spotify sie nennt.

d-fysa7usi = Name

d-ajgkhxtc = Art
    .doc = `computer`, `smartphone`, `speaker`, `tv`, `cast` oder eine andere, die Spotify nennt.

d-njju5rcv = Aktiv
    .doc = Ob, was läuft, hier läuft.

d-daqeango = Lautstärke
    .doc = Wie laut es ist, `0..=100`, wenn Spotify es sagt.

d-cjoykprs = Geräte
    .doc = Die Orte, an denen Spotify spielt, und die Anmeldung der Person bei Spotify.

d-d6agrwhu = Spotify-Zustand
    .doc = `off`, `connecting`, `connected` oder `trouble`.

d-wzbxwvai = Spotify-Konto

d-q6fvzfwi = Spotify-Abo
    .doc = `premium`, `free`, `open` oder leer, wie Spotify es sagt.

d-bnnshdna = Spotify-Problem

d-rwf7lfs6 = Anmeldeadresse
    .doc = Die Seite, auf der die Person sich anmeldet, während `spotify_state` `connecting` sagt.

d-gghnswba = Spotify aktiv
    .doc = Das Gerät, auf dem Spotify spielt, als sein Schlüssel; leer, wenn keines.

d-sdkihe6k = Spotify jetzt
    .doc = Was dort läuft, wie Menschen es lesen.

d-22kwzmah = Spotify spielt

d-sbgoce2w = Gerät
    .w-playing-here = Spielt hier
    .w-play-here = Hier abspielen

d-cpsa6nlq = Spotify verbinden
    .doc = Verbindet das Spotify der Person: die Seite, auf der sie sich anmeldet, steht im Spotify-Abschnitt.

d-bgomdthz = Ich habe mich angemeldet
    .doc = Nimmt die Anmeldung an, die die Person auf Spotifys Seite gegeben hat.

d-c3zailpi = Spotify trennen
    .doc = Vergisst die Spotify-Anmeldung dieses Geräts.

d-lyxtlmu3 = Geräte aktualisieren
    .doc = Liest die Orte, an denen Spotify spielt.

d-s5u526ny = Wiedergabe übertragen
    .doc = Bewegt, was läuft, zu `d`, von Anfang an.

d-amt56qpq = Auf Spotify abspielen
    .doc = Spielt `uri` auf Spotify ab, wo es spielt: leer spielt, was lief.

d-7vrw7iss = Auf Spotify pausieren
    .doc = Pausiert, was auf Spotify läuft.

d-y4ceqqvs = Weiter auf Spotify
    .doc = Spielt, was auf Spotify folgt.

d-moryk2vk = Zurück auf Spotify
    .doc = Spielt, was auf Spotify davor lief.

d-clxyvglt = Was auf Spotify läuft
    .doc = Liest, was jetzt auf Spotify läuft.

d-s5u526ny-d = Gerät

d-amt56qpq-uri = Adresse


d-nsvv7dnn = Verborgen
    .doc = Songs, die die Person aus den Vorschlägen ausgeblendet hat.

d-qh2hecoe = Vorschlag abspielen
    .doc = Spielt einen vorgeschlagenen Song ab.

d-deybznbt = Diesen Vorschlag ausblenden
    .doc = Blendet einen Song aus den Vorschlägen aus.

d-deybznbt-t = T

d-324mqcom = Wieder vorschlagen
    .doc = Holt einen verborgenen Song zurück in die Vorschläge.

d-324mqcom-t = T

d-iyodtvzu-filter-active = Aktiv

d-iyodtvzu-filter-volume = Lautstärke

d-tkebe3bj-filter-hidden = Verborgen

d-aymfw7yu-filter-spotify_playing = Spielt auf Spotify

d-so-tilo-music-keywords = music, musik, song, songs, titel, track, tracks, album, alben, artist, artists, künstler, kuenstler, playlist, playlists, wiedergabe, player, audio, listen, hören, hoeren, spotify, youtube music, lyrics, songtext, liedtext

# Was Spotify behält, was dort als Nächstes läuft, und was die Künstler für Musik machen.

d-nb4ehhax = Genres
    .doc = Die Musikrichtungen, die der Katalog ihnen zuschreibt: leer, wo er keine nennt, wie YouTube Music keine nennt.

d-qu3zxatt = Beschreibung
    .doc = Was die Playlist über sich sagt; leer, wenn sie nichts sagt.

d-ijme5amc = Spotify-Warteschlangeneintrag
    .doc = Ein Titel dessen, was auf Spotify als Nächstes läuft, in Spotifys eigener Reihenfolge: eine Momentaufnahme, die die Person neu liest, keine Warteschlange, die diese App bearbeitet.
    .noun = { $count ->
        [one] Eintrag
       *[other] Einträge
    }
    .untitled = Unbekannter Titel

d-r4rop6xj = Schlüssel
    .doc = Die Spotify-Adresse des Titels, nach der das Abspielen fragt.

d-teom6gyk = Titel

d-b4nh57k6 = Künstler

d-cjgrr3sx = Album

d-edgjqah3 = Cover

d-gbooyryg = Sekunden

d-ijme5amc-filter-seconds = Sekunden

d-ltrfuiaz = Beschreibung
    .doc = Was die Playlist über sich sagt, für Lesende.

d-xp3dtewi = Spotify
    .doc = Die Spotify-Playlist, die sie ist, sobald der Katalog der Person Spotify ist: jeder hier hinzugefügte Titel wird dort hinzugefügt, und Name und Beschreibung bleiben im Gleichschritt.

d-myoyyape = Ähnliche Künstler
    .doc = Fragt nach Titeln von Künstlern, deren Musikrichtung die Person schon hört.

d-hiupwos3 = Spotify-Warteschlange
    .doc = Was auf Spotify als Nächstes läuft, wie Spotifys eigene Warteschlange es beim letzten Lesen hatte.

d-ekk67ewc = Nächste Spotify-Titel
    .doc = Wo die nächste Seite jeder Spotify-Sammlung der Person beginnt; 0 heißt keine.

d-gwbm6xq4 = Nächste Spotify-Alben

d-q6w6ykgo = Nächste Spotify-Playlists

d-hbxo67cl = Nächste Spotify-Künstler

d-aymfw7yu-filter-spotify_next_tracks = Nächste Spotify-Titel

d-aymfw7yu-filter-spotify_next_albums = Nächste Spotify-Alben

d-aymfw7yu-filter-spotify_next_playlists = Nächste Spotify-Playlists

d-aymfw7yu-filter-spotify_next_artists = Nächste Spotify-Künstler

d-tkebe3bj-filter-similar_artists = Ähnliche Künstler

d-ilknh6a6 = Playlist umbenennen
    .doc = Gibt einer Playlist der Person einen anderen Namen, bei Spotify auch, wenn sie eine Spotify-Playlist ist.

d-ilknh6a6-name = Name

d-mj6ddil6 = Playlist beschreiben
    .doc = Setzt, was eine Playlist über sich sagt, bei Spotify auch, wenn sie eine Spotify-Playlist ist.

d-mj6ddil6-description = Beschreibung

d-o6zynibu = Playlist an Spotify senden
    .doc = Schickt Name und Beschreibung einer Playlist an Spotify, die Fassung, die ihre Person auf ihren anderen Geräten liest; eine Playlist, die nicht Spotifys ist, sagt nichts.

d-mirav5ur = Spotifys Warteschlange lesen
    .doc = Liest, was auf Spotify als Nächstes läuft: Spotifys eigene Warteschlange, hier gezeigt und hier nie bearbeitet.

d-sdo3anxm = In Spotifys Warteschlange
    .doc = Stellt einen Titel hinter das, was auf Spotify läuft.

d-sdo3anxm-t = Titel

d-nvvyoutq = Mehr von Spotify
    .doc = Fragt Spotify nach einer weiteren Seite dessen, was die Person dort behält.

d-nvvyoutq-kind = Art

