# Die Bildschirmtastatur auf Deutsch. Keys sind die stabilen ids der Deklarationen (app.ids); die Wörter der Quelle stehen in den Quellen des Pakets.

d-so-tilo-osk = Bildschirmtastatur

d-so-tilo-osk-keywords = tastatur, bildschirmtastatur, touch-tastatur, tippen, wischen, autokorrektur, vorschläge, emoji, keyboard, on-screen keyboard

d-d3zzmiv7 = Tippen
    .doc = Was die Person gerade tippt, so wie die Tastatur es hält, solange sie steht: das zusammengesetzte Wort, das Wort davor, was die Leiste anbietet und den eigenen Zustand der Tasten (Umschalt, die Seite der Tasten, die gehaltenen Modifikatortasten).
    .noun = { $count ->
        [one] Tippvorgang
       *[other] Tippvorgänge
    }

d-m6tp67xz = Feld
    .doc = Das Feld, zu dem dies gehört, so wie die Sitzung Felder zählt.

d-xgezzsnq = Wort
    .doc = Das zusammengesetzte Wort, im Feld als noch nicht übernommener Text angezeigt.

d-kurpxlhy = Vorheriges
    .doc = Das Wort, das davor übernommen wurde und sagt, welches Wort als Nächstes kommen kann.

d-tduplb36 = Gewischt
    .doc = Das zusammengesetzte Wort stammt aus einem Wischen: Das nächste Leerzeichen oder der nächste Buchstabe übernimmt es so, wie es ist.

d-nrfgg7zc = Vorschläge
    .doc = Was die Leiste anbietet, der Reihe nach: `w:` ein Wort, `p:` eines der eigenen Wörter der Person, `e:` ein Emoji, `t:` das Wort, wie es vor einer Korrektur getippt wurde.

d-oli6hbi7 = Getippt
    .doc = Das Wort, wie es getippt wurde, bevor eine Korrektur ein anderes daraus machte, und was darauf folgte: Eine Rücktaste direkt danach oder der Vorschlag mit dem getippten Wort tippt es zurück.

d-kydmz4at = Rest

d-yir2po5h = Umschalt
    .doc = Umschalt: 0 aus, 1 der nächste Buchstabe, 2 Feststelltaste.

d-64zexfdz = Seite
    .doc = Die angezeigten Tasten: `letters`, `symbols`, `emoji` (die Auswahl an der Stelle der Tasten) oder `emoji-search` (die Auswahl über den Buchstaben, bei der die Buchstaben ihre Suche tippen).

d-lfpmstnh = Suchbegriff
    .doc = Die Emoji-Suche, die die Buchstaben getippt haben.

d-2tfifihw = Ctrl
    .doc = Ctrl und Alt: 0 aus, 1 die nächste Taste, 2 festgestellt.

d-x6mgoh2l = Alt

d-qbj7o6ze = Satz
    .doc = Der nächste Buchstabe beginnt einen Satz.

d-25ultnxs = Weggeräumt
    .doc = Die Tasten sind weggeräumt: Nur der Streifen, der sie zurückbringt, steht.

d-3mckgl7e = Formen
    .doc = Was ein langes Drücken auf jedem Buchstaben anbietet, zuerst die Formen der Tippsprache, und die Sprache und Groß-/Kleinschreibung, in der sie angefragt wurden.

d-jf436se6 = Formen für

d-d3zzmiv7-filter-field = Feld

d-d3zzmiv7-filter-swiped = Gewischt

d-d3zzmiv7-filter-shift = Umschalt

d-d3zzmiv7-filter-ctrl = Ctrl

d-d3zzmiv7-filter-alt = Alt

d-d3zzmiv7-filter-sentence = Satz

d-d3zzmiv7-filter-parked = Weggeräumt

d-a6qx5nsb = Gelehrt
    .doc = Ein Wort, das das Tippen gelehrt hat, so wie die Vorhersage des Geräts es liest.
    .noun = { $count ->
        [one] Gelehrtes Wort
       *[other] Gelehrte Wörter
    }

d-ecaq5x2q = Kontext

d-yybwwgav = Wort

d-htei6x6l = Anzahl

d-a6qx5nsb-filter-count = Anzahl

d-dsbvxito = Vorschlagsanfrage
    .noun = { $count ->
        [one] Vorschlagsanfrage
       *[other] Vorschlagsanfragen
    }

d-u7ua3uwj = Wort

d-x64bsvli = Vorheriges

d-i2l5ongd = Sprache

d-xyp2g7uo = Persönlich

d-btxnh6dk = Personalisierung

d-lbar2w74 = Gelernt

d-3jxblazz = Behalten

d-oqjnl4cg = Grenze

d-dsbvxito-filter-personalization = Personalisierung

d-dsbvxito-filter-limit = Grenze

d-piuwtpgx = Angebot
    .doc = Ein angebotenes Wort.
    .noun = { $count ->
        [one] Angebot
       *[other] Angebote
    }

d-sspaaf2o = Wort

d-ym3zbzik = Punktzahl

d-o6ytofpt = Persönlich

d-zjsvehiu = Emoji

d-piuwtpgx-filter-score = Punktzahl

d-piuwtpgx-filter-personal = Persönlich

d-piuwtpgx-filter-emoji = Emoji

d-xtb52aih = Vorgeschlagen
    .noun = { $count ->
        [one] Vorschlag
       *[other] Vorschläge
    }

d-7qgx3bze = Ergänzungen

d-txmmjsyz = Nächste

d-phvamby5 = Korrekturanfrage
    .noun = { $count ->
        [one] Korrekturanfrage
       *[other] Korrekturanfragen
    }

d-avrdwkwu = Wort

d-xh7nuttw = Sprache

d-rdht4mzn = Belegung

d-ip6lajuh = Zeilen

d-nyu5nrfl = Behalten

d-cwws377t = Korrigiert
    .noun = { $count ->
        [one] Korrigiertes Wort
       *[other] Korrigierte Wörter
    }

d-37fhak2q = Korrektur

d-houozjaj = Wischanfrage
    .noun = { $count ->
        [one] Wischanfrage
       *[other] Wischanfragen
    }

d-vkdpd5r7 = Pfad

d-qhrbdy4q = Fläche

d-p6t77uy7 = Zeilen

d-udzknqo5 = Belegung

d-yhplpjdc = Sprache

d-pxdf2zfl = Umschalt

d-mocqi47p = Grenze

d-a2o5rdfx = Beurteilt

d-mvvgppgc = Seitenverhältnis

d-houozjaj-filter-limit = Grenze

d-houozjaj-filter-judged = Beurteilt

d-houozjaj-filter-aspect = Seitenverhältnis

d-mddag2g7 = Kandidat
    .noun = { $count ->
        [one] Kandidat
       *[other] Kandidaten
    }

d-zsr74dix = Wort

d-mybbluwr = Punktzahl

d-mddag2g7-filter-score = Punktzahl

d-mzgljvry = Gewischt
    .noun = { $count ->
        [one] Gewischtes
       *[other] Gewischte
    }

d-veop4uz2 = Wischen

d-56adnkcz = Kandidaten

d-mzgljvry-filter-swipe = Wischen

d-u2rxtyxc = Berührungsanfrage
    .noun = { $count ->
        [one] Berührungsanfrage
       *[other] Berührungsanfragen
    }

d-ilse75gf = Punkt

d-ayis4zfh = Fläche

d-25lqxg2k = Belegung

d-yjrtaqh2 = Zeilen

d-j7nbu66f = Vorheriges

d-lb64yv45 = Wort

d-6ofwzyyn = Sprache

d-tfzfpcem = Versätze

d-xl5fo2at = Personalisierung

d-u2rxtyxc-filter-personalization = Personalisierung

d-mqwxb4w2 = Treffer
    .doc = Wo die Tipps der Person auf einem Buchstaben landen, so wie das Gerät es aus diesem Tipp gelernt hat.
    .noun = { $count ->
        [one] Treffer
       *[other] Treffer
    }

d-gmprye6b = Ch

d-7e2ewqea = Dx

d-xxbw2p3b = Dy

d-hidcbudr = Messwerte

d-mqwxb4w2-filter-dx = Dx

d-mqwxb4w2-filter-dy = Dy

d-mqwxb4w2-filter-samples = Messwerte

d-26sgcc3w = Berührt
    .noun = { $count ->
        [one] Berührtes
       *[other] Berührte
    }

d-uk2nkbvx = Taste

d-gf3rqhji = Gelehrt

d-26sgcc3w-filter-taught = Gelehrt

d-5okqtxah = Akzentanfrage
    .noun = { $count ->
        [one] Akzentanfrage
       *[other] Akzentanfragen
    }

d-erau6win = Tasten

d-wbtiabii = Sprache

d-6we4suoh = Umschalt

d-wsbsq7ho = Akzente
    .noun = { $count ->
        [one] Akzent
       *[other] Akzente
    }

d-cha7zqyr = Formen

d-zpna6er6 = Gedrückt
    .doc = Eine Taste des Tastenfelds wurde gedrückt: `index` zählt die angezeigten Tasten, `at` ist, wo der Finger war, als Bruchteile des Kastens des Tastenfelds.

d-zpna6er6-k = K

d-zpna6er6-typing = Tippen

d-zpna6er6-index = Index

d-zpna6er6-at = Ort

d-i56ikhmm = Gehalten
    .doc = Ein langes Drücken hat eine der Formen einer Taste gewählt.

d-i56ikhmm-k = K

d-i56ikhmm-typing = Tippen

d-i56ikhmm-choice = Auswahl

d-sw44nryq = Gemeint
    .doc = Der Buchstabe, den ein Tipp gemeint hat.

d-sw44nryq-k = K

d-sw44nryq-t = T

d-sw44nryq-key = Taste

d-sw44nryq-at = Ort

d-7qh7hzpy = Setzt fort
    .doc = Ein Wort der Sprache geht über `stem'` hinaus weiter: `don'`, `l'`, `aujourd'`.

d-7qh7hzpy-k = K

d-7qh7hzpy-t = T

d-7qh7hzpy-stem = Stamm

d-gnuxm6jb = Buchstabe
    .doc = Ein getippter Buchstabe: zum Wort zusammengesetzt, wo das Feld Wörter aufnimmt, sonst so getippt, wie er ist.

d-gnuxm6jb-k = K

d-gnuxm6jb-typing = Tippen

d-gnuxm6jb-key = Taste

d-dccnjqei = Tastendruck
    .doc = Eine Taste, die kein Buchstabe ist: eine der eigenen Tasten, eine Bearbeitungstaste oder ein Zeichen.

d-dccnjqei-k = K

d-dccnjqei-typing = Tippen

d-dccnjqei-key = Taste

d-lnddiih4 = Übernehmen
    .doc = Das zusammengesetzte Wort geht mit `after` dahinter ins Feld: zuvor korrigiert, wenn `correct` es verlangt und das Feld Korrekturen annimmt, und daraus gelernt.

d-lnddiih4-k = K

d-lnddiih4-typing = Tippen

d-lnddiih4-after = Danach

d-lnddiih4-correct = Korrigieren

d-fhkpdkb2 = Korrigiert
    .doc = Wozu das Gerät `word` korrigieren würde, oder das Wort selbst.

d-fhkpdkb2-k = K

d-fhkpdkb2-t = T

d-fhkpdkb2-word = Wort

d-obhkjqjq = Rücktaste
    .doc = Rücktaste: direkt nach einer Korrektur tippt sie das Wort so zurück, wie die Person es getippt hat, und behält es so; in einem Wort nimmt sie dessen letzten Buchstaben (ein gewischtes Wort geht ganz); sonst erreicht sie das Feld.

d-obhkjqjq-k = K

d-obhkjqjq-typing = Tippen

d-t3uwnw4x = Korrektur zurücknehmen
    .doc = Die gerade gemachte Korrektur wird zurückgenommen: Das Wort geht wieder so hinein, wie es getippt wurde, und wird nie wieder korrigiert.

d-t3uwnw4x-k = K

d-t3uwnw4x-typing = Tippen

d-7pdlgd3j = Tastenkombination
    .doc = Eine Taste, gedrückt mit gehaltenem Ctrl oder Alt.

d-7pdlgd3j-k = K

d-7pdlgd3j-typing = Tippen

d-7pdlgd3j-name = Name

d-i5bdwha2 = Cursor
    .doc = Die Leertaste wurde gezogen: Der Cursor bewegt sich um `steps` Buchstaben, nachdem das Wort hineingeht.

d-i5bdwha2-k = K

d-i5bdwha2-typing = Tippen

d-i5bdwha2-steps = Schritte

d-kccc44ey = Gewischt
    .doc = Ein Finger ist über die Buchstaben gefahren: Das Wort, das er buchstabiert hat, wird zusammengesetzt, für Umschalt in der passenden Schreibung, und die anderen Wörter, die er gemeint haben könnte, stehen in der Leiste.

d-kccc44ey-k = K

d-kccc44ey-typing = Tippen

d-kccc44ey-path = Pfad

d-kccc44ey-aspect = Seitenverhältnis

d-rqm2y3gl = Angebot
    .doc = Was die Leiste jetzt anbietet: die Wörter, zu denen das zusammengesetzte werden kann, oder die, die als Nächstes kommen können, die eigenen der Person zuerst, dann bis zu zwei Emoji.

d-rqm2y3gl-k = K

d-rqm2y3gl-typing = Tippen

d-6efbgh4j = Angenommen
    .doc = Eines der Angebote der Leiste wurde angenommen.

d-6efbgh4j-k = K

d-6efbgh4j-typing = Tippen

d-6efbgh4j-chip = Vorschlag

d-w6xwd4px = Ausgewählt
    .doc = Ein Emoji aus der Auswahl geht hinter dem Wort hinein.

d-w6xwd4px-k = K

d-w6xwd4px-typing = Tippen

d-w6xwd4px-emoji = Emoji

d-htytuoqn = Werkzeug
    .doc = Einer der Befehle des Streifens.

d-htytuoqn-k = K

d-htytuoqn-typing = Tippen

d-htytuoqn-name = Name

d-3gb6smu3 = Mit Formen
    .doc = Die Formen, die ein langes Drücken auf jedem Buchstaben anbietet, beim Gerät in der getippten Sprache angefragt.

d-3gb6smu3-k = K

d-3gb6smu3-typing = Tippen

d-lis76ngb = Taste
    .w-shift = Umschalt
    .w-caps-lock-on = Feststelltaste, an
    .w-shift-on-for-the-next-letter = Umschalt, für den nächsten Buchstaben
    .w-backspace = Rücktaste
    .w-return = Eingabe
    .w-no-space = kein Leerzeichen
    .w-space = Leertaste
    .w-space-2 = Leertaste
    .w-abc = ABC
    .w-letters = Buchstaben
    .w-numbers-and-symbols = Ziffern und Zeichen
    .w-emoji = Emoji
    .w-switch-to = Zu { $layout_name } wechseln

d-z4oisiy3 = Name der Belegung
    .w-french-azerty = Französisch (AZERTY)
    .w-english-qwerty = Englisch (QWERTY)

d-xr5qsumm = Name der Umschaltung
    .w-locked = { $name }, festgestellt
    .w-for-the-next-key = { $name }, für die nächste Taste

d-dujtyvam = Tastatur
    .w-show-keys = Tasten zeigen
    .w-hide-keyboard = Tastatur ausblenden
    .w-esc = Esc
    .w-tab = Tab
    .w-left = Links
    .w-down = Runter
    .w-up = Hoch
    .w-right = Rechts
    .w-paste = Einfügen
    .w-move-caret-left = Cursor nach links
    .w-move-caret-right = Cursor nach rechts
    .w-put-the-keys-away = Tasten wegräumen
    .w-suggestions-are-off = Vorschläge sind aus
    .w-type-or-swipe-to-see-suggestions = Tippen oder wischen, um Vorschläge zu sehen
    .w-undo-the-correction = Korrektur zurücknehmen
    .w-emoji = Emoji
    .w-abc = ABC
    .w-back-to-letters = Zurück zu den Buchstaben
    .w-search-emoji = Emoji suchen
    .w-delete = Löschen
    .w-on-screen-keyboard = Bildschirmtastatur
