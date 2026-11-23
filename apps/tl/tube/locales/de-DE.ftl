d-so-tilo-tube = YouTube


d-m5wewovo = Kanal
    .doc = Ein Kanal, auf dem jemand Videos veröffentlicht.
    .noun = { $count ->
        [one] Kanal
       *[other] Kanäle
    }


d-26ctwf34 = Schlüssel


d-ya2u4cw6 = Name


d-woyix7h2 = Handle


d-ah5nkqv7 = Abonnenten


d-2qxzpxur = Avatar


d-d4mzmvrr = Banner


d-rzhtvjsv = Beschreibung


d-lfopp7e7 = Verifiziert


d-2ap24kjm = Gefunden über
    .doc = Die Suche, die ihn fand; leer für einen Kanal, den die Abonnements der Person oder eine Seite hereingebracht haben.


d-albed4jx = Video
    .doc = Ein Video, wie der Dienst es beschreibt, mit dem, was die Person damit gemacht hat: mit „Gefällt mir“ markiert, auf die Warteschlange gesetzt und wo sie aufgehört hat (das sagt der Verlaufseintrag).
    .noun = { $count ->
        [one] Video
       *[other] Videos
    }


d-irzy3c7m = Schlüssel


d-fpxk5w7b = Titel


d-mhkply6c = Schlüssel des Kanals


d-2mklwbfq = Name des Kanals


d-lfky4erv = Avatar des Kanals


d-txtos5p6 = Aufrufe


d-2ruyhh5f = Veröffentlicht


d-mvlf65b5 = Dauer
    .doc = Wie lange es läuft, in Sekunden.


d-pfdoae2w = Beschreibung


d-ddfiordt = Gefällt mir


d-l25oorez = Vorschaubild


d-pkbzjs7l = Stream
    .doc = Die Adresse, von der seine Daten abgespielt werden, wie der Dienst sie geantwortet hat: leer, bis sie gefragt wird.


d-jfam7tjf = Live
    .doc = Ob es ein Live-Stream statt einer Aufnahme ist.


d-bifohbpv = Gefällt mir
    .doc = Die Person hat es mit „Gefällt mir“ markiert.


d-4mojz3hw = Im Trend
    .doc = Der Trend-Feed hat es aufgeführt.


d-5o5fpqgh = Gefunden über
    .doc = Die Suche, die es fand; leer für ein Video, das ein Feed oder eine Seite hereingebracht hat.


d-albed4jx-filter-liked = Gefällt mir


d-albed4jx-filter-trending = Im Trend


d-mmzhfe3o = Wiedergabeliste
    .doc = Eine Wiedergabeliste: Videos in einer Reihenfolge, die ihre Erstellerin gewählt hat. Die Videos sind die des Clients, in der Reihenfolge der Wiedergabeliste gehalten, sodass eine Seite sie so zeigt und abspielt, wie ihre Erstellerin sie geordnet hat.
    .noun = { $count ->
        [one] Wiedergabeliste
       *[other] Wiedergabelisten
    }


d-ow7glfos = Schlüssel


d-ljyc7mx2 = Titel


d-bzfmd6pg = Schlüssel des Kanals


d-zvfpfva3 = Name des Kanals


d-6cllrl7x = Anzahl Videos


d-4xvzshd7 = Vorschaubild


d-p3yw6p2h = Gefunden über
    .doc = Die Suche, die sie fand.


d-ebglq5za = Abonnement
    .doc = Ein Kanal, den die Person abonniert hat, nach seinem Schlüssel: Mitgliedschaft, sodass zwei Personen, die zugleich abonnieren, es einmal behalten, und der Feed die Videos der Kanäle liest.
    .noun = { $count ->
        [one] Abonnement
       *[other] Abonnements
    }


d-slhmwv67 = Schlüssel
    .doc = Der Schlüssel des Kanals, wie der Dienst ihn nennt.


d-ynf67l6e = Name


d-kgojom6f = Verlaufseintrag
    .doc = Ein Video, das die Person gesehen hat: wann und wo sie aufgehört hat. Jeder Eintrag nennt die Person, die es gesehen hat, sodass jede Person nur den eigenen Verlauf liest.
    .noun = { $count ->
        [one] Verlaufseintrag
       *[other] Verlaufseinträge
    }


d-noppuxpb = Schlüssel
    .doc = Der Schlüssel des Videos, wie der Dienst ihn nennt.


d-stuii7l7 = Titel


d-j37y2bbw = Name des Kanals


d-dmm6f63t = Gesehen


d-omff5ciz = Fortschritt
    .doc = Wie weit sie waren, als Anteil seiner Länge; null, wenn sie es bis zum Ende gesehen haben.

# verfeinert

d-thwdswkk = YouTube
    .doc = Was der Client hält: die Kanäle, Videos und Wiedergabelisten, die der Dienst geantwortet hat, die Abonnements der Person, ihr Verlauf und wie die letzte Suche steht.
    .noun = { $count ->
        [one] Kanal
       *[other] Kanäle
    }


d-olrung72 = Titel
    .doc = Wie der Client heißt.


d-ihhzrm3f = Kanäle


d-qnfjkti5 = Videos


d-v5sh2bpr = Wiedergabelisten


d-iputtmua = Abonnements


d-hv25j74h = Verlauf


d-bqesicxq = Suchtext
    .doc = Wonach die letzte Suche fragt, wie es getippt wurde.


d-hcmy6dp7 = Zustand der Suche
    .doc = Wie die Suche nach `search_text` steht: `searching`, `done`, `failed` oder leer.


d-zjw3anaz = Störung der Suche
    .doc = Warum die letzte Suche fehlschlug; leer, solange sie durchging.


d-nnvco6w3 = Zustand des Feeds
    .doc = Wie der Feed der Abonnements steht: `loading`, `done`, `failed` oder leer.


d-r3ksw6bn = Störung
    .doc = Warum der Client gerade nicht mitkommt: YouTube ist nicht erreichbar, oder was es sagte, ließ sich nicht sichern; leer, solange er mitkommt.


d-qv7t2mza = Online
    .doc = Ob YouTube geantwortet hat, als es zuletzt gefragt wurde.


d-thwdswkk-filter-online = Online


d-tmam46un = Abonnieren
    .doc = Abonniert den Kanal oder kündigt das Abonnement: der Knopf sagt, was er tun wird.


d-tmam46un-subscribed = Abonniert


d-hcgvcroq = Abonnieren
    .doc = Abonniert den Kanal oder nimmt das Abonnement zurück.
    .w-unsubscribe = Abonnement beenden
    .w-subscribe = Abonnieren


d-weyz2uyl = Gefällt mir
    .doc = Markiert das Video mit „Gefällt mir“ oder nimmt die Markierung zurück.
    .w-unlike = Gefällt mir zurücknehmen
    .w-like = Gefällt mir


d-weyz2uyl-liked = Gefällt mir


d-yw4tuoq4 = Später ansehen
    .doc = Setzt das Video auf die Warteschlange des Workspace oder nimmt es herunter. Ein neu hinzugefügtes Video steht hinter den schon wartenden, sodass die Warteschlange in der Reihenfolge abgespielt wird, in der Videos hinzugefügt wurden.
    .w-remove-from-the-queue = Von der Warteschlange entfernen
    .w-watch-later = Später ansehen


d-yw4tuoq4-saved = Auf der Warteschlange


d-6ux63onw = Abspielen
    .doc = Spielt das Video ab, ob es schon abgespielt wurde oder nicht.


d-s334bmn6 = Fortsetzen
    .doc = Spielt das Video ab der Stelle ab, an der die Person aufgehört hat, wenn sie es nicht zu Ende gesehen hat.


d-jxokfijg = Fortschritt
    .doc = Hält fest, wie weit die Person im Video ist, und dass sie es gerade gesehen hat: der Eintrag rückt an den Anfang des Verlaufs und fragt den Dienst nach der Adresse, von der seine Daten abgespielt werden, solange sie nicht bekannt ist.


d-jxokfijg-at = Bei


d-oqumfxkj = Aus dem Verlauf löschen
    .doc = Nimmt ein Video aus dem eigenen Verlauf.


d-wzt3qvyb = Verlauf löschen
    .doc = Löscht den eigenen Verlauf, und den niemandes sonst.


d-6lgqby4s = Suchen
    .doc = Durchsucht YouTube: der Dienst antwortet mit Kanälen, Wiedergabelisten und Videos, geschrieben, wo die Suchansicht sie auflistet.
    .w-type-something-to-search-for = Tippe etwas ein, wonach gesucht werden soll


d-6lgqby4s-query = Suchbegriff


d-vhf33pa7 = Suche verlassen
    .doc = Verlässt die Suche, die der Feed zeigt, für den Ort, den die Person gewählt hat: die Ergebnisse bleiben, wo der Dienst sie geschrieben hat (wiedergefunden über die Suche, die sie fand), und der Client zeigt den gewählten Ort.
    .w-no-search-is-showing = Es wird keine Suche gezeigt


d-hpjdxg6k = Aktualisieren
    .doc = Fragt den Dienst nach den neuesten Videos jedes abonnierten Kanals: was der Feed der Abonnements zeigt.


d-wx26izlg = Aktualisieren
    .doc = Fragt den Dienst nach den Videos des Kanals.


d-khblapap = Trends aktualisieren
    .doc = Fragt den Dienst nach den Videos, die er als im Trend führt.


d-yc4rjoeg = Erneut abspielen
    .doc = Spielt das Video ab, das ein Verlaufseintrag nennt, ab der Stelle, an der die Person aufgehört hat.


d-yc4rjoeg-h = H


d-qgtk64v3 = Uhr des Eintrags
    .w-live = Live


d-5ozzrcey = Abonnement umschalten~present


d-2nggfmec = Gefällt mir~present


d-fg57jp5b = Später ansehen~present


d-h6g3amy7 = Suchen~present


d-mufcicuu = Feed
    .w-youtube = YouTube
    .w-search-youtube = YouTube durchsuchen
    .w-subscriptions = Abonnements
    .w-trending = Im Trend
    .w-you = Du
    .w-watch-later = Später ansehen
    .w-liked-videos = Videos mit „Gefällt mir“
    .w-history = Verlauf
    .w-nothing-liked-yet = Noch nichts mit „Gefällt mir“
    .w-nothing-trending-yet = Noch nichts im Trend
    .w-refresh-trending = Trends aktualisieren
    .w-no-subscriptions-yet = Noch keine Abonnements
    .w-nothing-new-yet = Noch nichts Neues
    .w-refresh = Aktualisieren
    .w-can-t-save-what-fetched = Was { $name } geantwortet hat, lässt sich nicht sichern
    .w-can-t-reach = { $name } ist nicht erreichbar
    .w-last-fetched = { $reason }. Zuletzt geholt { $fetched }
    .w-try-again = Erneut versuchen
    .w-nothing-on-the-watch-queue = Nichts auf der Warteschlange
    .w-play-all = Alle abspielen
    .w-videos-oldest-first = { $len } Videos, älteste zuerst
    .w-videos-you-keep-for-later-wait = Videos, die du für später behältst, warten hier, älteste zuerst


d-dfz6u5sa = Videoraster


d-nxne4q6s = Suchergebnisse
    .w-search-youtube = YouTube durchsuchen
    .w-all = Alle
    .w-videos = Videos
    .w-channels = Kanäle
    .w-playlists = Playlists
    .w-length = Länge
    .w-any = Beliebig
    .w-under-4-min = Unter 4 Min.
    .w-4-to-20-min = 4 bis 20 Min.
    .w-over-20-min = Über 20 Min.
    .w-posted = Veröffentlicht
    .w-any-time = Jederzeit
    .w-past-hour = Letzte Stunde
    .w-today = Heute
    .w-this-week = Diese Woche
    .w-this-month = Dieser Monat
    .w-this-year = Dieses Jahr

d-tbcsx2lp = Suche findet
    .w-searching-youtube = YouTube wird durchsucht
    .w-the-search-failed = Die Suche schlug fehl
    .w-search-youtube = YouTube durchsuchen
    .w-channels-playlists-and-videos-by-name = Kanäle, Wiedergabelisten und Videos, nach Namen
    .w-nothing-found = Nichts gefunden
    .w-nothing-matches = Nichts passt zu { $search_text }
    .w-channels = Kanäle
    .w-playlists = Wiedergabelisten
    .w-videos = Videos


d-yf5l4ysw = Gesehen-Seite
    .w-history = Verlauf


d-lhrpz5jd = Verlaufsliste
    .w-nothing-watched-yet = Noch nichts gesehen


d-yh23kjna = Kanalseite
    .w-unsubscribe = Abonnement beenden
    .w-subscribe = Abonnieren
    .w-refresh = Aktualisieren
    .w-s-banner = Banner von { $name }
    .w-videos = { $len } Videos
    .w-about = Über
    .w-videos-2 = Videos
    .w-no-videos-here-yet = Noch keine Videos hier


d-w7mpbzm4 = Videoseite
    .w-play = Abspielen
    .w-unsubscribe = Abonnement beenden
    .w-subscribe = Abonnieren
    .w-liked = Gefällt mir
    .w-like = Gefällt mir
    .w-watch-later = Später ansehen
    .w-live = Live
    .w-description = Beschreibung
    .w-found-for = Gefunden über { $found_for }
    .w-this-video-won-t-play-here = Dieses Video lässt sich hier nicht abspielen
    .w-try-again = Erneut versuchen
    .w-no-comments-yet = Noch keine Kommentare
    .w-refresh-comments = Kommentare aktualisieren
    .w-comments = Kommentare

d-eijjevst = Wiedergabelistenseite
    .w-videos = { $video_count } Videos
    .w-its-videos-appear-once-youtube-says = Ihre Videos erscheinen, sobald YouTube sagt, was sie enthält
    .w-nothing-shown-from-this-playlist-yet = Aus dieser Wiedergabeliste wird noch nichts gezeigt
    .w-play-all = Alle abspielen
    .w-refresh = Aktualisieren
    .w-loading-the-playlist = Playlist wird geladen

d-z642pnnw = Videozeile


d-uiu73od5 = Kanalzeile


d-pgyjhoe2 = Verlaufszeile


d-t3cwabaw = Früheres Tube übernehmen
    .doc = Was das frühere Tube vor der Übernahme dieser Kennung hielt, einmalig in dieses übernommen: die abonnierten Kanäle, die mit Gefällt mir markierten Videos, die Warteschlange und der Verlauf mit der Zeit, zu der jedes Video angesehen wurde. Der übernommene Verlauf entstand, bevor dieses Programm festhielt, wer was gesehen hat; seine Einträge nennen daher niemanden und bleiben die des Workspace, bis jemand eines der Videos erneut sieht.


d-r6snpxjs = Vorgemerktes Video
    .doc = Ein Video, das jemand auf die Warteschlange des Workspace gesetzt hat. Eine Sammlung behält die Reihenfolge, in der ihre Einträge hinzugefügt wurden, sodass die Warteschlange in der Reihenfolge abgespielt wird, in der Videos hinzugefügt wurden.
    .noun = { $count ->
        [one] Vorgemerktes Video
       *[other] Vorgemerkte Videos
    }


d-o3xolmld = Schlüssel
    .doc = Der Schlüssel des Videos, wie der Dienst ihn nennt.


d-qynvz25p = Wiedergabeeinstellungen
    .doc = Wie eine Person gerne sieht. Der eigene Eintrag jeder Person: er folgt ihr auf jedes Gerät, und eine Ansicht liest den Eintrag der Person, die sie ansieht.
    .noun = { $count ->
        [one] Wiedergabeeinstellung
       *[other] Wiedergabeeinstellungen
    }


d-qynvz25p-filter-autoplay = Automatisch weiterspielen


d-qynvz25p-filter-captions = Untertitel


d-qynvz25p-filter-member = Person


d-qynvz25p-filter-speed = Geschwindigkeit


d-5twttwwn = Person


d-vze2yftu = Automatisch weiterspielen
    .doc = Spielt das nächste Video ab, wenn dieses endet.


d-5bpxm33f = Geschwindigkeit
    .doc = Wie schnell es läuft: 0,25×, 0,5×, 1×, 1,5×, 2× oder 3×.


d-lesyml4z = Bevorzugte Qualität
    .doc = Das höchste Bild, das sie möchte, wie YouTube es nennt (1080p, 720p, 480p), oder auto für das beste, das passt.


d-u3fgpv6e = Untertitel
    .doc = Untertitel zeigen, wenn das Video welche hat.


d-7mtvj3xd = Gesehen von
    .doc = Wer es gesehen hat. Leer für einen Eintrag aus dem Verlauf des früheren Tube, bevor dieses Programm festhielt, wer was gesehen hat: er bleibt der des Workspace, und wer das Video erneut sieht, macht ihn zu seinem.


d-dge65utd = Videos
    .doc = Die Schlüssel der Videos, in der Reihenfolge ihrer Erstellerin: keine, bis der Dienst geantwortet hat, was sie enthält.


d-jgjqgesf = Warteschlange
    .doc = Die Warteschlange des Workspace, älteste zuerst.


d-epgpppzy = Wiedergabeeinstellungen aller
    .doc = Die Wiedergabeeinstellungen jeder Person.


d-4bg5hmr2 = Alle abspielen
    .doc = Spielt die Warteschlange ab ihrem ältesten Video ab: was zuerst hinzugefügt wurde, läuft zuerst.


d-7lfzugkh = Alle abspielen
    .doc = Spielt die Wiedergabeliste ab ihrem ersten Video ab.


d-edbp7ej2 = Aktualisieren
    .doc = Fragt den Dienst, was die Wiedergabeliste enthält, in der Reihenfolge ihrer Erstellerin.


d-4hpmg3yw = Erneut versuchen
    .doc = Fragt den Dienst erneut, wo die Daten des Videos abgespielt werden, nachdem der Player sie nicht zeigen konnte: die Adresse, die der Client hält, schreibt allein der Dienst, und seine neue Antwort ist die, die der Player öffnet.


d-lcvxshh4 = Automatisch weiterspielen
    .doc = Jede dieser Aktionen hält eine der Wiedergabeeinstellungen der Person fest: der Eintrag gehört ihr, entsteht beim ersten Setzen, und jede andere Person liest ihre eigenen.


d-3r5wepfg = Wiedergabegeschwindigkeit
    .doc = Wie schnell Videos für die Person laufen: 0,25 bis 3, 1 für die übliche Geschwindigkeit.


d-lzop6dva = Bevorzugte Qualität
    .doc = Das höchste Bild, das die Person möchte, wie YouTube es nennt; auto nimmt das beste, das passt.


d-fmurpek2 = Untertitel
    .doc = Ob Videos ihre Untertitel für die Person zeigen.


d-lcvxshh4-on = Ein


d-3r5wepfg-speed = Geschwindigkeit


d-lzop6dva-quality = Qualität


d-fmurpek2-on = Ein

d-lhbq7qff = Dislikes
    .doc = Wie viele Menschen es nicht mochten, geschätzt von Return YouTube Dislike: null, bis der Dienst antwortet.

d-vvxj4w64 = DeArrow-Titel
    .doc = Der Titel der Community, wenn DeArrow ihn besser nennt als der Kanal selbst.

d-imamvrnr = DeArrow-Vorschaubild
    .doc = Das Bild, mit dem die Community es rahmt, als Inhalt dieses Geräts.

d-4qqv4ujc = Kommentar
    .doc = Ein Kommentar, den jemand unter einem Video hinterlassen hat, wie der Dienst ihn beschreibt.
    .noun = { $count ->
        [one] Kommentar
       *[other] Kommentare
    }

d-pv4laajx = Schlüssel

d-li4xiefh = Videoschlüssel
    .doc = Das Video, unter dem er steht, wie der Dienst es nennt.

d-3ncdj3up = Autor

d-lgtlgzui = Avatar

d-az6iwuma = Text

d-hgdwjwgt = Likes

d-dninsxc6 = Veröffentlicht
    .doc = Wann er geschrieben wurde.

d-tmf5hovn = Angepinnt
    .doc = Der Kanal hat ihn oben angepinnt.

d-gp3basiz = Mit Herz
    .doc = Der Kanal hat ihn mit einem Herz markiert.

d-ha6vpfhz = Antworten
    .doc = Wie viele Antworten er auf YouTube hat.

d-o7bs2bg7 = Abschnitt
    .doc = Ein `SponsorBlock`-Abschnitt eines Videos: wo er beginnt und endet, damit der Player ihn überspringen kann.
    .noun = { $count ->
        [one] Abschnitt
       *[other] Abschnitte
    }

d-uakovynu = Schlüssel

d-pvlf5o2c = Videoschlüssel
    .doc = Das Video, zu dem er gehört, wie der Dienst es nennt.

d-qlgpeq35 = Kategorie
    .doc = Seine Art, wie SponsorBlock sie nennt: `sponsor`, `selfpromo`, `interaction`, `intro`, `outro`, `preview`, `music_offtopic` oder `filler`.

d-bbnmvcim = Beginn
    .doc = Wo er beginnt, in ganzen Sekunden.

d-hts3jgqy = Ende
    .doc = Wo er endet, in ganzen Sekunden.

d-j7wuhxg5 = Stimmen
    .doc = Die Stimmen der Community, die ihn platzieren.

d-3vh5q7os = Antwort
    .doc = Eine Antwort, die ein Mitglied auf einen Kommentar geschrieben hat: die des Workspace, neben dem Kommentar, den sie beantwortet, denn YouTube lässt keine App als die Person schreiben.
    .noun = { $count ->
        [one] Antwort
       *[other] Antworten
    }

d-y4coaxks = Kommentar
    .doc = Der Kommentar, den sie beantwortet, wie der Dienst ihn nennt.

d-kwxg57v6 = Video
    .doc = Das Video, unter dem der Kommentar steht.

d-6psi22ua = Text

d-nzwksffp = Mitglied
    .doc = Wer sie geschrieben hat.

d-k3jskmid = Geschrieben

d-3vh5q7os-filter-member = Mitglied

d-3vh5q7os-filter-said = Geschrieben

d-vbbcsaos = SponsorBlock
    .doc = Die von SponsorBlock markierten Abschnitte beim Abspielen überspringen.

d-7cyrtrv6 = DeArrow
    .doc = Titel und Bilder der Community zeigen, wo DeArrow sie hat.

d-eyoonq3d = Dislikes
    .doc = Zeigen, wie viele Menschen ein Video nicht mochten, geschätzt von Return YouTube Dislike.

d-qynvz25p-filter-sponsorblock = SponsorBlock

d-qynvz25p-filter-dearrow = DeArrow

d-qynvz25p-filter-dislikes = Dislikes

d-puten5ar = Verlauf gelöscht
    .doc = Ob jemand den Verlauf gelöscht hat: ein früherer Verlauf wird nicht darüber importiert, denn die Löschung bleibt.

d-ugv4b6is = Kommentare
    .doc = Die Kommentare, die der Dienst geantwortet hat, unter der Id des Kommentars.

d-cjlgivfs = Abschnitte
    .doc = Die SponsorBlock-Abschnitte, die der Dienst geantwortet hat, danach, wo jeder steht.

d-iphlnbkx = Antworten
    .doc = Die Antworten, die Mitglieder auf Kommentare geschrieben haben: die des Workspace, denn YouTube lässt keine App als die Person schreiben.

d-2pryeqkt = Art der Suche
    .doc = Welche Arten die letzte Suche zeigt: `all`, `video`, `channel` oder `playlist`.

d-egducuxn = Länge der Suche
    .doc = Wie lang die gefundenen Videos sind: `any`, `short` (unter vier Minuten), `medium` (vier bis zwanzig) oder `long` (über zwanzig).

d-ypapv6iy = Zeitpunkt der Suche
    .doc = Wie neu sie sind: `any`, `hour`, `today`, `week`, `month` oder `year`.

d-thwdswkk-filter-history_cleared = Verlauf gelöscht

d-s3fflgse = Abschnitt überspringen
    .doc = Überspringt den SponsorBlock-Abschnitt, in den das Video gerade läuft, bei `at` (ein Anteil seiner Länge), wenn die Person Abschnitte überspringt: der Abspielkopf springt an das Ende des Abschnitts, sodass eine Werbeeinblendung oder ein Abspann nie angesehen wird.

d-s3fflgse-at = Bei

d-ckcysqaa = Beim Öffnen laden
    .doc = Beim Öffnen einer Playlist fragt die App den Dienst, was sie enthält, wenn der Client es noch nicht weiß: eine Playlist, die eine Suche gefunden hat, ist nur mit Name und Cover bekannt, und ihre Videos erscheinen, sobald YouTube sie nennt; die Seite bleibt also nie leer, bis jemand Aktualisieren drückt.

d-xwvivyes = Beim Öffnen lesen
    .doc = Beim Öffnen eines Videos fragt die App den Dienst nach dem, was die Seite zeigt und der Client noch nicht hält: woher die Bytes in der bevorzugten Qualität kommen, die Kommentare, die SponsorBlock-Abschnitte, Titel und Bild der Community (DeArrow) und wie viele Menschen es nicht mochten.

d-hr6a4zpi = Kommentare aktualisieren
    .doc = Fragt den Dienst erneut nach den Kommentaren des Videos, für das, was seit dem Öffnen der Seite gesagt wurde.

d-jw35ym2c = Antworten
    .doc = Antwortet auf einen Kommentar: der Workspace behält die Antwort selbst, neben dem Kommentar, den sie beantwortet, denn YouTube lässt keine App als die Person schreiben.
    .w-type-something-to-reply-with = Schreib etwas als Antwort

d-jw35ym2c-text = Text

d-gzpeyx2d = Antwort löschen
    .doc = Nimmt eine Antwort zurück, die die Person geschrieben hat.
    .w-only-the-person-who-wrote-a = Nur wer eine Antwort geschrieben hat, nimmt sie zurück

d-dinedg2j = Sponsor-Abschnitte überspringen
    .doc = Ob der Player die von SponsorBlock markierten Abschnitte beim Abspielen überspringt.

d-dinedg2j-on = An

d-3hrcdqhd = Titel und Bilder der Community
    .doc = Ob ein Video Titel und Bild der Community zeigt, wo DeArrow sie hat.

d-3hrcdqhd-on = An

d-usutngoe = Dislike-Zahlen
    .doc = Ob ein Video zeigt, wie viele Menschen es nicht mochten, geschätzt von Return YouTube Dislike.

d-usutngoe-on = An

d-eoe2k3dt = Suche filtern
    .doc = Schränkt die Suche auf die gewählten Arten ein: `all`, `video`, `channel` oder `playlist`.
    .w-is-not-a-kind-of-result = { $kind } ist keine Art von Ergebnis

d-eoe2k3dt-kind = Art

d-tfnkfs2w = Nach Länge filtern
    .doc = Schränkt die Suche darauf ein, wie lang ihre Videos sind: `any`, `short`, `medium` oder `long`.
    .w-is-not-a-length = { $length } ist keine Länge

d-tfnkfs2w-length = Länge

d-fheinfhb = Nach Datum filtern
    .doc = Schränkt die Suche darauf ein, wie neu ihre Ergebnisse sind: `any`, `hour`, `today`, `week`, `month` oder `year`.
    .w-is-not-a-period = { $published } ist kein Zeitraum

d-fheinfhb-published = Veröffentlicht

d-33ap24jl = Verlauf importieren
    .doc = Importiert einen Verlauf, den ein anderes Gerät geführt hat, im Format, das der Verlauf der nativen App und ihr `import`-Aufruf nutzten: Zeilen aus `[dem Schlüssel eines Videos, wann es in Millisekunden seit 1970 gesehen wurde]`.

d-33ap24jl-watched = Gesehen

d-j5dzbot6 = Früheren Verlauf importieren
    .doc = Importiert den Verlauf, den eine frühere Version dieser App für die Person geführt hat, in deren eigenem Format: Zeilen aus `[dem Schlüssel eines Videos, wann es in Millisekunden seit 1970 gesehen wurde, wo es in Millisekunden verlassen wurde oder nichts]`.

d-j5dzbot6-previous = Früher

d-7pzrjwch = Auf Kommentar antworten~present

d-eee25ayg = Antwort zurücknehmen~present

d-zk3bb2v4 = Suche filtern~present

d-6aqost67 = Nach Länge filtern~present

d-u3d3kfnk = Nach Datum filtern~present

d-mwai3gvn = Link öffnen
    .doc = Öffnet einen Link des Schemas, das dieser Client beansprucht: das Video (`tube:watch?v=<key>`), den Kanal (`tube:channel?key=<key>`) oder die Playlist (`tube:playlist?key=<key>`), die er nennt, oder die Suche, um die er bittet (`tube:search?q=<query>`).

d-mwai3gvn-link = Link
