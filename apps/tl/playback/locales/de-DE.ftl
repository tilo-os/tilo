# Playback auf Deutsch. Keys are the declarations' stable ids (app.ids); see locales/en-US.ftl.

d-so-tilo-playback = Wiedergabe

d-c6fmojnr = Ausgabe
    .doc = Ein Ort, an dem Ton und Bild einer Seh- oder Hörsitzung herauskommen: dieses Gerät, ein Lautsprecher in einem Raum oder ein Cast-Ziel.
    .noun = { $count ->
        [one] Ausgabe
       *[other] Ausgaben
    }

d-mtgbfbe3 = Art
    .doc = `local`, `speaker` oder `cast`.

d-q2afggjg = Gerät
    .doc = Das Gerät, der Lautsprecher oder das Ziel, wie das System es nennt; leer für dieses Gerät.

d-duy3y22z = Name
    .doc = Was Menschen davon lesen: „Küche“, „Lautsprecher Wohnzimmer“.

d-sz4b5nfh = Lautstärke

d-ewrhlhvs = Synchron
    .doc = Ob es im Takt mit der Stelle der Sitzung wiedergibt oder hinter einem Cast-Ziel zurückbleibt.

d-c6fmojnr-filter-volume = Lautstärke

d-c6fmojnr-filter-synced = Synchron

d-kf7cpzbo = Eintrag
    .doc = Ein Eintrag, den die Sitzung wiedergeben kann, mit dem, was Menschen davon lesen.
    .noun = { $count ->
        [one] Eintrag
       *[other] Einträge
    }

d-jsytyrgy = Referenz
    .doc = Die Referenz der App auf den Eintrag (Videos Clip, Musics Titel).

d-hbrdxjws = Titel

d-3sty6xvi = Künstler

d-wzxeznnc = Cover

d-iueovkzr = Quelle
    .doc = Woher seine Bytes wiedergegeben werden: der behaltene Inhalt, sonst die Quelle.

d-ggr7nnxj = Länge
    .doc = Wie lange es läuft, in Sekunden, wenn die App es weiß.

d-kf7cpzbo-filter-length = Länge

d-sa6rjoaz = Fortschritt
    .doc = Wo ein Eintrag verlassen wurde, damit ein späteres Öffnen dort fortsetzt.
    .noun = { $count ->
        [one] Fortschritt
       *[other] Fortschritte
    }

d-ijcc2upy = Referenz
    .doc = Die Referenz der App auf den Eintrag, wie der `track` der Sitzung ihn nennt.

d-owteykf6 = Position
    .doc = Wo er stand, in Sekunden.

d-q64zycak = Aktualisiert
    .doc = Wann die Stelle zuletzt geschrieben wurde.

d-sa6rjoaz-filter-position = Position

d-sa6rjoaz-filter-updated = Aktualisiert

d-omcimail = Wiedergabe
    .doc = Was läuft und wo es steht: eine Sitzung des Arbeitsbereichs, nur durch Aktionen geschrieben, sodass jedes Gerät dasselbe an derselben Stelle öffnet und das System es als den Player der Person zeigt.
    .noun = { $count ->
        [one] Wiedergabe
       *[other] Wiedergaben
    }
    .untitled = Nichts läuft

d-hnwowp42 = Titel
    .doc = Die Referenz der App auf das, was läuft; leer, wenn nichts läuft.

d-sx7vtcfl = Titel

d-eejl5scp = Künstler

d-rjbz4skr = Cover

d-tafuqjic = Quelle
    .doc = Woher Ton oder Bild wiedergegeben wird: die behaltenen Bytes, wo sie sind, sonst die Quelle.

d-xcyilpu4 = Zustand
    .doc = Ob es läuft.

d-p2dmm6gi = Ausgaben
    .doc = Jeder Ort, an dem es herauskommt, in der Reihenfolge, in der er hinzugefügt wurde: zuerst dieses Gerät, dann die Lautsprecher und Cast-Ziele, die die Person hinzugefügt hat.

d-pkexjxh3 = Bei
    .doc = Wo es steht und wie lang es ist, in Sekunden.

d-min4lfee = Länge

d-udayujnj = Lautstärke

d-4t7z5bco = Zufallswiedergabe

d-2l7pa2rs = Wiederholen
    .doc = `off`, `all` oder `one`.

d-wwdrlrqy = Warteschlange
    .doc = Die Einträge, die folgen, in Reihenfolge, und welcher läuft.

d-krqh6s6b = Stelle in der Warteschlange

d-eixby7cm = Fortschritt
    .doc = Wo jeder Eintrag verlassen wurde: wovon ein späteres Öffnen fortsetzt, ein Eintrag pro Eintrag.

d-cvxa6lh7 = Wiedergabezustand
    .doc = Ob der Player stillsteht, läuft oder gar nichts hat.

d-6lrdx5em = Gestoppt

d-x3vz7lp5 = Läuft

d-zb4h4oj4 = Pausiert

d-yeogsqcw = Liste abspielen
    .doc = Spielt die Einträge ab, die `items` nennt, in der gegebenen Reihenfolge, ab dem bei `at`: die Warteschlange ist, was folgt, und der Eintrag bei `at` läuft.

d-yeogsqcw-items = Einträge

d-yeogsqcw-at = Bei

d-ovcgpk6a = Aktuellen setzen
    .doc = Die Sitzung zeigt `item`: ihre Felder sind die des Eintrags, sodass ein gemeinsames Weiter oder Zurück nie Titel, Cover oder Quelle des vorigen behält.

d-ovcgpk6a-item = Eintrag

d-d4zwmel2 = Wiedergabe setzen
    .doc = Spielt, was läuft, oder pausiert es, wie es eine eigene Steuerung und eine Medientaste verlangen.

d-d4zwmel2-on = An

d-osacia6u = Quelle setzen
    .doc = Woher kommt, was läuft, wenn die App, die es weiß, erneut geantwortet hat: eine andere Qualität eines Videos oder die erneut angefragte Adresse.

d-osacia6u-source = Quelle

d-pk7bfu5k = Länge setzen
    .doc = Was der Player über seine Länge meldet, sobald der Decoder sie kennt.

d-pk7bfu5k-seconds = Sekunden

d-i2veyrzn = Position merken
    .doc = Behält die Stelle der Sitzung und die des Eintrags, wie `when` sie festhält.

d-i2veyrzn-seconds = Sekunden

d-i2veyrzn-when = Wann

d-xh3lfbpp = Position setzen
    .doc = Wo der Player steht, wie er es meldet, damit ein späteres Öffnen dort fortsetzt: die Stelle der Sitzung und die des Eintrags, behalten, damit ein anderes Gerät oder ein späteres Öffnen von dort weitermacht.

d-xh3lfbpp-seconds = Sekunden

d-3gntm43c = Anteil setzen
    .doc = Wo der Player steht, als Anteil seiner Länge, den ein Player meldet (`0..=1`), behalten, damit ein späteres Öffnen dort fortsetzt.

d-3gntm43c-share = Anteil

d-lk75gedd = Fortsetzen
    .doc = Bewegt, was läuft, dorthin, wo `ref` verlassen wurde, sodass ein Video oder ein Lied dort weitermacht, wo die Person aufgehört hat, statt von vorn zu beginnen.

d-lk75gedd-ref = Referenz

d-g5ehiohm = Abspielen
    .doc = Spielt, was gewählt ist, oder beginnt wieder, wo es steht.

d-2u5srydi = Umschalten
    .doc = Spielt, was läuft, oder pausiert es.

d-dd6dt32e = Pause
    .doc = Pausiert, was läuft, und startet es wieder, wo es stand (die eigene Aufgabe von [`toggle`]).

d-i5ctxqgd = Stopp
    .doc = Stoppt, was läuft: es läuft auf keinem Gerät, und die Shell zeigt es nicht mehr.

d-fxxarqxu = Weiter
    .doc = Der Eintrag nach diesem: der nächste der Warteschlange oder der erste, wenn Wiederholen an ist.

d-jmnwv624 = Zurück
    .doc = Der Eintrag vor diesem.

d-sueidkl2 = Wählen
    .doc = Spielt den eingereihten Eintrag bei `at` und lässt die Warteschlange, wie sie steht.

d-sueidkl2-at = Bei

d-o5tnzj23 = Umschalten um
    .doc = Bewegt, was läuft, um `seconds`, für die beiden Suchsteuerungen der Live-Karte.

d-o5tnzj23-seconds = Sekunden

d-ftrjircx = Springen
    .doc = Bewegt, was läuft, zu `place`, einem Bruchteil seiner Länge, und behält die Stelle, damit ein späteres Öffnen dort fortsetzt.

d-ftrjircx-place = Stelle

d-4ndxwmgz = Lautstärke setzen
    .doc = Wie laut die Sitzung ist.

d-4ndxwmgz-level = Pegel

d-bpl24zko = Stumm
    .doc = Macht die Sitzung stumm oder bringt die Lautstärke zurück, die sie hatte.

d-kfomtoxg = Zufall umschalten
    .doc = Mischt die Warteschlange oder stellt ihre Reihenfolge wieder her.

d-kfomtoxg-shuffle = Zufall

d-6lghyo6k = Wiederholen weiterschalten
    .doc = Schaltet Wiederholen weiter: aus, alles, eins.

d-kw3s3ite = Zur Warteschlange
    .doc = Fügt `item` nach dem hinzu, was läuft.

d-kw3s3ite-item = Eintrag

d-u7afdn3b = Als Nächstes
    .doc = Fügt `item` direkt nach dem hinzu, was läuft.

d-u7afdn3b-item = Eintrag

d-v4y4chqp = Aus Warteschlange entfernen
    .doc = Nimmt den Eintrag bei `index` aus der Warteschlange.

d-v4y4chqp-index = Index

d-v4y4chqp-queue_at = Stelle in der Warteschlange

d-tcakivo7 = In Warteschlange verschieben
    .doc = Bewegt den Eintrag namens `from` dorthin, wo `to` steht: die Zeile der Warteschlange, die eine Person gezogen hat, und die Ids, die ihr Umsortieren meldet.

d-tcakivo7-from = Von

d-tcakivo7-to = Nach

d-qbjrwcgo = Ausgabe hinzufügen
    .doc = Legt den Ton auf einen weiteren Ort: einen Lautsprecher in einem Raum oder ein Cast-Ziel.

d-qbjrwcgo-output = Ausgabe

d-u7fcmuqw = Ausgabe entfernen
    .doc = Nimmt den Ort bei `index` aus der Sitzung; dieses Gerät wird nie herausgenommen.

d-u7fcmuqw-index = Index

d-wahda3bo = Wechseln nach
    .doc = Bewegt den Ort bei `from` dorthin, wo `to` ist: „auf dem Lautsprecher fortsetzen“ ist das Bewegen der Sitzung von diesem Gerät dorthin.

d-wahda3bo-from = Von

d-wahda3bo-to = Nach

d-gomcpa5y = Ausgabelautstärke setzen
    .doc = Setzt, wie laut der Ort bei `index` ist.

d-gomcpa5y-index = Index

d-gomcpa5y-level = Pegel

d-4zpe77fa = Live
    .w-nothing-playing = Nichts läuft
    .w-previous = Zurück
    .w-pause = Pause
    .w-play = Abspielen
    .w-next = Weiter
    .w-back-15-seconds = 15 Sekunden zurück
    .w-forward-15-seconds = 15 Sekunden vor
    .w-stop = Stopp
