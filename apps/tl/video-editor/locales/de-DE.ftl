# Videobearbeitung auf Deutsch. Keys are the declarations' stable ids (app.ids); the source's words are written from the package's sources.


d-so-tilo-video-editor = Videobearbeitung

d-so-tilo-video-editor-keywords = video editor, video, film, timeline, cut, trim, split, transition, title, export, videobearbeitung, video schneiden, film, zeitleiste, schnitt, exportieren

d-jqhqav7h = Quelle
    .doc = Eine Quelle im Korb: was sie ist, wo ihre Bytes liegen und – bei einem Titel – die Worte, die sie zeichnet.
    .noun = { $count ->
        [one] Quelle
       *[other] media
    }
    .untitled = Unbenannte Quelle

d-efejoszz = Name

d-pcamqw4n = Form

d-wj2aohfd = Dauer

d-hpkuwe3q = Breite

d-krrrznuy = Höhe

d-vmxn3cwg = Bilder/s

d-foxa6zt7 = Palette
    .doc = Ihr Platz in der Palette, der ihr Korbbild und ihre Clips tönt.

d-zltur6gc = Adresse
    .doc = Die Adresse, unter der ihre Bytes lesen (`tilo-blob://…`); leer bei einem Titel.

d-ap457hnd = Hat Ton

d-3vgrlwgv = Standbild
    .doc = Ihr Bild, wie der Korb es zeigt, behalten, sobald die Engine es gezeichnet hat.

d-vddvspwk = Ausschläge
    .doc = Ihre Lautstärke über die ganze Datei, ein Anteil des Vollpegels, wie die Wellenform sie zeichnet.

d-v4yfudyw = Fehlt
    .doc = Ob dieses Gerät die Bytes der Quelle nicht lesen kann: sie wurden nie hierher gebracht, oder sie liegen auf einem anderen Gerät des Arbeitsbereichs.

d-e7atusb5 = Worte
    .doc = Die Worte eines Titels.

d-qrihyxs3 = Größe
    .doc = Die Versalhöhe eines Titels, ein Anteil der Bildhöhe.

d-x2yorlvh = Rot

d-5nv4umhz = Grün

d-ktanswey = Blau

d-lqpu2kuo = Deckkraft

d-3j5mkeir = Ausrichtung

d-u37npdvi = Fett

d-wzbpbjsf = Kursiv

d-hlbhw6tg = Hinterlegung
    .doc = Die Hinterlegung eines Titels, 0 für keine.

d-adomtjkz = Effekt
    .doc = Ein stapelbarer Bildeffekt eines Clips: was er ist und wie stark.
    .noun = { $count ->
        [one] Effekt
       *[other] effects
    }
    .untitled = Effekt

d-ktghxc36 = Form

d-lj3ggezn = Stärke

d-adomtjkz-filter-amount = Stärke

d-2um277kp = Spur
    .doc = Eine Zeile der Zeitleiste.
    .noun = { $count ->
        [one] Spur
       *[other] tracks
    }
    .untitled = Spur

d-hyiqyr2d = Name

d-voeh7uw2 = Form

d-h5vj7qoc = Reihenfolge

d-o7i5df56 = Stumm

d-azpfbjsn = Solo

d-wowizwqg = Gesperrt

d-rofejlx6 = Verstärkung dB
    .doc = Der Regler in Dezibel, 0 bei Einheit.

d-ovon2nm7 = Panorama
    .doc = Wo sie im Stereofeld steht: -1 ganz links, 0 Mitte, 1 ganz rechts.

d-2um277kp-filter-form = Form

d-2um277kp-filter-index = Reihenfolge

d-2um277kp-filter-muted = Stumm

d-2um277kp-filter-solo = Solo

d-2um277kp-filter-locked = Gesperrt

d-2um277kp-filter-gain_db = Verstärkung dB

d-2um277kp-filter-pan = Panorama

d-rggqngof = Schlüsselbild
    .doc = Ein Wert, den ein Clip an einer eigenen Stelle hält, und wie die Kurve ihn verlässt.
    .noun = { $count ->
        [one] Schlüsselbild
       *[other] keys
    }
    .untitled = Schlüsselbild

d-362ioruy = Feld
    .doc = Das Feld, das sie treibt, wie die Engine es nennt (`opacity`, `pos_x`, `effect:…`).

d-ksxcsp7k = Zeit
    .doc = Sekunden ab dem Anfang des Clips, damit ein Verschieben oder Teilen sie mitnimmt.

d-al7ic2wh = Wert

d-4rmokhsj = Verlauf
    .doc = `linear`, `hold` oder `ease`.

d-rggqngof-filter-t = Zeit

d-rggqngof-filter-v = Wert

d-aj2coyre = Clip
    .doc = Eine Stelle einer Quelle auf einer Spur: wo sie beginnt, welchen Teil der Quelle sie zeigt und wie sie aussieht und klingt.
    .noun = { $count ->
        [one] Clip
       *[other] clips
    }
    .untitled = Clip

d-azbufhus = Name

d-tpxg6k4q = Spur

d-y4qdvmxb = Quelle

d-vle43xl6 = Anfang
    .doc = Seine linke Kante auf der Zeitleiste, in Sekunden.

d-tmrpa5lj = Einstieg
    .doc = Der Einstiegspunkt in der Quelle, in Sekunden.

d-ijg7zj64 = Ausstieg
    .doc = Der Ausstiegspunkt in der Quelle, in Sekunden.

d-mzmglpkg = Deckkraft

d-hb6kydcz = Lautstärke

d-5lwntoqr = Übergang
    .doc = Der eingehende Übergang: leer für einen harten Schnitt, sonst `dissolve`, `fade`, `dip` oder `wipe`.

d-ihl7lqbw = Übergangsdauer
    .doc = Wie lange dieser Übergang läuft, in Sekunden; 0 für seine Vorgabe.

d-fmhqhubf = Einblenden

d-bygbi2tw = Ausblenden

d-jrsfcsej = Palette

d-55seasjt = Geschwindigkeit
    .doc = Die Abspielgeschwindigkeit; 1 ist normal.

d-dmthewcr = Rückwärts

d-ttu42344 = Helligkeit

d-sosquagj = Kontrast

d-t6mekjo2 = Sättigung

d-kikxhud5 = Breite

d-67q6ewpx = Höhe

d-nu7xhxvx = Waagerecht

d-vrqmvalw = Senkrecht

d-3ru7tn4m = Drehung

d-kx3c765z = Links beschneiden

d-4rl2tdsz = Rechts beschneiden

d-d4ryk5ir = Oben beschneiden

d-r27hvfe7 = Unten beschneiden

d-7uz3gyat = Effekte

d-tskiyma5 = Farbe entfernen

d-nflhw3wz = Wie viel der Farbe

d-gk3fgv4a = Weiche Kante

d-zxs2bxu6 = Farbstich entfernen

d-ag6tnt5w = Schlüsselbilder
    .doc = Wo sich ein Wert dieses Clips über seine eigene Länge ändert: eine Kurve auf einem Feld behält daneben den konstanten Wert.

d-54ikwabs = Bildstreifen
    .doc = Bilder dieses Clips, wie der Bildstreifen im Inspektor sie zeigt, und der Bereich, für den sie gezeichnet wurden, damit ein Beschnitt sie neu zeichnet.

d-bvukpc5x = Bildstreifenbereich

d-dhh754gx = Farbe Rot
    .doc = Die Farbe, die der Key entfernt, in 0..1 RGB: was eine Aufnahme aus dem Bild setzt.

d-podots7u = Farbe Grün

d-tgpgd3hb = Farbe Blau

d-hfnhir7p = Maskenform
    .doc = Eine Maske, die begrenzt, wo die Ebene zeigt: leer für keine, sonst `rect` oder `ellipse`.

d-hclucc6k = Maske waagerecht

d-g2n4egth = Maske senkrecht

d-lwnm5qmy = Maske Breite

d-casg7gsr = Maske Höhe

d-u3oht37u = Maske Drehung

d-iraobc5s = Maske Weichheit

d-6njmwsei = Maske umkehren

d-vmc7izdi = Verknüpfung
    .doc = Die Verknüpfungsgruppe, deren eine Hälfte dieser Clip ist: sein Bild und sein Ton bewegen sich zusammen.

d-wovh5j2b = Verknüpfung folgt

d-poy7f6dc = Ton abgetrennt
    .doc = Wahr, wenn der eigene Ton dieses Bildes auf einen Tonclip abgetrennt wurde.

d-aj2coyre-filter-track = Spur

d-aj2coyre-filter-media = Quelle

d-aj2coyre-filter-start = Anfang

d-aj2coyre-filter-in_point = Einstieg

d-aj2coyre-filter-out_point = Ausstieg

d-aj2coyre-filter-opacity = Deckkraft

d-aj2coyre-filter-volume = Lautstärke

d-aj2coyre-filter-transition_duration = Übergangsdauer

d-aj2coyre-filter-fade_in = Einblenden

d-aj2coyre-filter-fade_out = Ausblenden

d-aj2coyre-filter-palette = Palette

d-aj2coyre-filter-speed_rate = Geschwindigkeit

d-aj2coyre-filter-reverse = Rückwärts

d-aj2coyre-filter-brightness = Helligkeit

d-aj2coyre-filter-contrast = Kontrast

d-aj2coyre-filter-saturation = Sättigung

d-aj2coyre-filter-scale_x = Breite

d-aj2coyre-filter-scale_y = Höhe

d-aj2coyre-filter-pos_x = Waagerecht

d-aj2coyre-filter-pos_y = Senkrecht

d-aj2coyre-filter-rotation = Drehung

d-aj2coyre-filter-crop_left = Links beschneiden

d-aj2coyre-filter-crop_right = Rechts beschneiden

d-aj2coyre-filter-crop_top = Oben beschneiden

d-aj2coyre-filter-crop_bottom = Unten beschneiden

d-aj2coyre-filter-key_enabled = Farbe entfernen

d-aj2coyre-filter-key_tolerance = Wie viel der Farbe

d-aj2coyre-filter-key_softness = Weiche Kante

d-aj2coyre-filter-key_spill = Farbstich entfernen

d-aj2coyre-filter-key_red = Farbe Rot

d-aj2coyre-filter-key_green = Farbe Grün

d-aj2coyre-filter-key_blue = Farbe Blau

d-aj2coyre-filter-mask_x = Maske waagerecht

d-aj2coyre-filter-mask_y = Maske senkrecht

d-aj2coyre-filter-mask_w = Maske Breite

d-aj2coyre-filter-mask_h = Maske Höhe

d-aj2coyre-filter-mask_rotation = Maske Drehung

d-aj2coyre-filter-mask_feather = Maske Weichheit

d-aj2coyre-filter-mask_invert = Maske umkehren

d-aj2coyre-filter-link_follows = Verknüpfung folgt

d-aj2coyre-filter-sound_split = Ton abgetrennt

d-kicr3edk = Marke
    .doc = Eine Stelle auf der Zeitleiste, die eine Person markiert hat.
    .noun = { $count ->
        [one] Marke
       *[other] markers
    }
    .untitled = Marke

d-6gy3r5jn = Name

d-43eckzwd = Zeit
    .doc = Sekunden auf der Zeitleiste.

d-jvjfto7y = Palette

d-h2qmhyqy = Notiz

d-kicr3edk-filter-t = Zeit

d-kicr3edk-filter-palette = Palette

d-fkdizcdd = Schnitt
    .doc = Ein Schnitt: seine Form, seine Quellen, seine Spuren und Clips und was die Engine zuletzt von ihm gezeichnet hat.
    .noun = { $count ->
        [one] Schnitt
       *[other] projects
    }
    .untitled = Unbenanntes Projekt

d-hdhd7ufr = Name

d-sggyvq5o = Bilder/s

d-rv7tdvl6 = Breite

d-z7cmqzxo = Höhe

d-m7e2vebr = Gewählte Form
    .doc = Ob die Form von einer Person gewählt wurde statt von der ersten Quelle übernommen.

d-enmbeevc = Anfang markieren
    .doc = Wo ein Rendern auf der Zeitleiste beginnt und endet; negativ für Kopf und Ende.

d-vfq5phqm = Ende markieren

d-f7xzwsye = Format
    .doc = Der Container, den ein Export schreibt.

d-qm4juanr = Quelle

d-4z3qlwar = Spuren

d-74vaoosx = Clips

d-qr6qe3cp = Marken

d-47vsdgaf = Standbild
    .doc = Das Bild, das die Vorschau bei `still_at` zeigt, von der Engine gezeichnet.

d-dppbe5en = Bild bei

d-xa5fpldf = Auftrag
    .doc = Das Rendern, das dieser Schnitt laufen hat, und wie weit es ist.

d-myjdbgx2 = Auftrag fertig

d-63bjrnm4 = Auftrag gesamt

d-rhjs5vir = Exportiert
    .doc = Die fertige Datei, die ein Export gemacht hat.

d-4jky56y7 = Störung
    .doc = Warum das Letzte, was die Engine gefragt wurde, scheiterte.

d-fkdizcdd-filter-fps = Bilder/s

d-fkdizcdd-filter-width = Breite

d-fkdizcdd-filter-height = Höhe

d-fkdizcdd-filter-explicit_format = Gewählte Form

d-fkdizcdd-filter-mark_in = Anfang markieren

d-fkdizcdd-filter-mark_out = Ende markieren

d-fkdizcdd-filter-still_at = Bild bei

d-fkdizcdd-filter-job_done = Auftrag fertig

d-fkdizcdd-filter-job_total = Auftrag gesamt

d-ugegxdzp = Bearbeitung
    .doc = Die Schnitte des Arbeitsbereichs und der, den das Fenster offen hat.
    .noun = { $count ->
        [one] Bearbeitung
       *[other] editors
    }

d-ws67tq4i = Projekte

d-i3vrs6nt = Offen

d-qyx2uds6 = Sitzungen
    .doc = Die Sitzung, über die das Fenster abspielt, damit ihr Ton ein Gerät zum Messen hat.

d-3kenkvdb = Spielt

d-ugegxdzp-filter-open = Offen

d-ugegxdzp-filter-playing = Spielt

d-wzqywwxp = Geprüfte Quelle
    .doc = Was das Gerät antwortete, als es gefragt wurde, was eine Datei ist.
    .noun = { $count ->
        [one] probed
       *[other] probeds
    }

d-bc2o5yk6 = Art

d-jb7opba6 = Dauer

d-duh5etpe = Breite

d-ibouz53w = Höhe

d-eci2btbm = Bilder/s

d-cg3roaa5 = Hat Ton

d-r72d7i4q = Codec

d-wzqywwxp-filter-duration = Dauer

d-wzqywwxp-filter-width = Breite

d-wzqywwxp-filter-height = Höhe

d-wzqywwxp-filter-fps = Bilder/s

d-wzqywwxp-filter-has_audio = Hat Ton

d-mxktylr3 = Quellenfrage
    .noun = { $count ->
        [one] probe ask
       *[other] probe asks
    }

d-dx4zjyh5 = Adresse

d-q4b7gd4q = Bildfrage
    .noun = { $count ->
        [one] still ask
       *[other] still asks
    }

d-itbf6wf4 = Schnitt
    .doc = Der Schnitt, wie `json.encode(shot(p))` ihn schreibt.

d-5klklgrm = Bei

d-zrjp2ayo = Breite

d-c7hx2nde = Höhe

d-q4b7gd4q-filter-at = Bei

d-q4b7gd4q-filter-width = Breite

d-q4b7gd4q-filter-height = Höhe

d-t53sw4me = Bildantwort
    .noun = { $count ->
        [one] still answer
       *[other] still answers
    }

d-fqmupsg5 = Adresse

d-xkpsjf7l = Wellenfrage
    .noun = { $count ->
        [one] wave ask
       *[other] wave asks
    }

d-hv2hn45e = Adresse

d-u2gsuh7h = Abschnitte

d-xkpsjf7l-filter-buckets = Abschnitte

d-oeiycvc3 = Ausschläge
    .noun = { $count ->
        [one] peak
       *[other] peaks
    }

d-xatqduw2 = Ausschläge

d-khrei3pe = Renderfrage
    .noun = { $count ->
        [one] render ask
       *[other] render asks
    }

d-4xhy5qpi = Schnitt
    .doc = Der Schnitt, wie `json.encode(shot(p))` ihn schreibt.

d-nlg2n4yg = Auflösung

d-n3bktwt4 = Container

d-ylbeg2vl = Bilder/s

d-mqmxnv4s = Name

d-khrei3pe-filter-fps = Bilder/s

d-cvn34nlm = Auftragsantwort
    .noun = { $count ->
        [one] job answer
       *[other] job answers
    }

d-2qflgu2y = Auftrag

d-cfr4gprt = Auftragsstand
    .noun = { $count ->
        [one] job state
       *[other] job states
    }

d-2lqqe2u4 = Stand

d-wddixskv = Fertig

d-yfo7z67v = Gesamt

d-ivfsfn6q = Adresse

d-tlb73mjx = Fehler

d-cfr4gprt-filter-done = Fertig

d-cfr4gprt-filter-total = Gesamt

d-7ufzdmfk = Schnittaufnahme
    .doc = Der Schnitt, wie das Gerät ihn liest: schlichte Werte mit den Namen, die die Engine kennt.
    .noun = { $count ->
        [one] shot
       *[other] shots
    }

d-fagho4gr = Name

d-fsxs4zep = Bilder/s

d-ebteysdk = Breite

d-a4torznk = Höhe

d-v6ck4d4l = Anfang markieren

d-hzxujhk3 = Ende markieren

d-yjp3sehi = Quelle
    .doc = Ein JSON-Text je Quelle; jeder ist eine `ShotMedia`.

d-q3ecfgq3 = Spuren
    .doc = Ein JSON-Text je Spur; jeder ist eine `ShotTrack`.

d-wgoywspl = Clips
    .doc = Ein JSON-Text je Clip; jeder ist ein `ShotClip`.

d-w26d474g = Marken
    .doc = Ein JSON-Text je Marke; jede ist ein `ShotMarker`.

d-7ufzdmfk-filter-fps = Bilder/s

d-7ufzdmfk-filter-width = Breite

d-7ufzdmfk-filter-height = Höhe

d-7ufzdmfk-filter-mark_in = Anfang markieren

d-7ufzdmfk-filter-mark_out = Ende markieren

d-wxyo7lsu = Aufnahmequelle
    .noun = { $count ->
        [one] shot media
       *[other] shot media
    }

d-jws44pym = Id

d-6irok5cm = Name

d-2yxap4ne = Art

d-j3rpcgim = Dauer

d-pwfziujj = Breite

d-7cokd24o = Höhe

d-ldk5mv2m = Bilder/s

d-5gvee3he = Palette

d-betxnhkz = Adresse

d-7ppacpgk = Hat Ton

d-cosd36fu = Worte
    .doc = Die Worte eines Titels; leer bei jeder dekodierten Quelle.

d-tmz6b4vk = Größe

d-uk5cqaxq = Farbe

d-a5zwa247 = Ausrichtung

d-mjhc4g63 = Schrift

d-lykfeg5x = Fett

d-npxtqckt = Kursiv

d-4b6rl7fz = Hinterlegung

d-wxyo7lsu-filter-duration = Dauer

d-wxyo7lsu-filter-width = Breite

d-wxyo7lsu-filter-height = Höhe

d-wxyo7lsu-filter-fps = Bilder/s

d-wxyo7lsu-filter-palette = Palette

d-wxyo7lsu-filter-has_audio = Hat Ton

d-wxyo7lsu-filter-size = Größe

d-wxyo7lsu-filter-bold = Fett

d-wxyo7lsu-filter-italic = Kursiv

d-wxyo7lsu-filter-backdrop = Hinterlegung

d-szcty4av = Aufnahmespur
    .noun = { $count ->
        [one] shot track
       *[other] shot tracks
    }

d-wzjt7soo = Id

d-2phnzuqk = Name

d-mb6mtsoz = Art

d-jr2eyeqg = Reihenfolge

d-wyqopmjv = Stumm

d-3enbjrqj = Solo

d-lr3pdaah = Gesperrt

d-zse3uh5c = Verstärkung dB

d-aix6cf7x = Panorama

d-szcty4av-filter-index = Reihenfolge

d-szcty4av-filter-muted = Stumm

d-szcty4av-filter-solo = Solo

d-szcty4av-filter-locked = Gesperrt

d-szcty4av-filter-gain_db = Verstärkung dB

d-szcty4av-filter-pan = Panorama

d-twd6225m = Aufnahmeclip
    .noun = { $count ->
        [one] shot clip
       *[other] shot clips
    }

d-jeeiaroo = Id

d-akxlh23d = Spur

d-pptlby5v = Quelle

d-3dgm7b2b = Anfang

d-zbz5amfa = Einstieg

d-pkiaohdd = Ausstieg

d-xi2qtarh = Deckkraft

d-chhkku2t = Lautstärke

d-s2fdck6n = Übergang

d-rrf4qn3g = Übergangsdauer

d-xubtbe5w = Einblenden

d-jllwurew = Ausblenden

d-clwasyxv = Palette

d-uge3tmju = Geschwindigkeit

d-rxbq7fqf = Geschwindigkeit rückwärts

d-5utmqjdy = Helligkeit

d-5kf4asry = Kontrast

d-ugpps35t = Sättigung

d-ueazn2zi = Breite

d-qxxmhhdr = Höhe

d-hl2h6f76 = Waagerecht

d-nhestloi = Senkrecht

d-ckydsf7y = Drehung

d-ttpiryuy = Links beschneiden

d-dazt7wt5 = Rechts beschneiden

d-o5353t6h = Oben beschneiden

d-lsgktf3d = Unten beschneiden

d-mq4hgil7 = Effekte
    .doc = Ein JSON-Text je Effekt, in Stapelreihenfolge.

d-6ahtwc73 = Schlüsselbilder
    .doc = Ein JSON-Text je Schlüsselbild, in clip-eigenen Sekunden.

d-2i54q4ly = Farbe entfernen

d-hcm6xmnu = Farbe des Keys

d-ss3pryvp = Wie viel der Farbe

d-odmxpb6p = Weiche Kante

d-vwxtiqh4 = Farbstich entfernen

d-v6saof6k = Maskenform

d-drrsji3r = Maske waagerecht

d-q63mhtzf = Maske senkrecht

d-w3owjsm6 = Maske Breite

d-lrguhlws = Maske Höhe

d-u4sfvd6p = Maske Drehung

d-zteue6rc = Maske Weichheit

d-ah7jtdcc = Maske umkehren

d-2wno4yo2 = Verknüpfung

d-3rzhv2vf = Verknüpfung folgt

d-hulnlv2t = Ton abgetrennt

d-twd6225m-filter-start = Anfang

d-twd6225m-filter-in_point = Einstieg

d-twd6225m-filter-out_point = Ausstieg

d-twd6225m-filter-opacity = Deckkraft

d-twd6225m-filter-volume = Lautstärke

d-twd6225m-filter-transition_duration = Übergangsdauer

d-twd6225m-filter-fade_in = Einblenden

d-twd6225m-filter-fade_out = Ausblenden

d-twd6225m-filter-palette = Palette

d-twd6225m-filter-speed_rate = Geschwindigkeit

d-twd6225m-filter-speed_reverse = Geschwindigkeit rückwärts

d-twd6225m-filter-brightness = Helligkeit

d-twd6225m-filter-contrast = Kontrast

d-twd6225m-filter-saturation = Sättigung

d-twd6225m-filter-scale_x = Breite

d-twd6225m-filter-scale_y = Höhe

d-twd6225m-filter-pos_x = Waagerecht

d-twd6225m-filter-pos_y = Senkrecht

d-twd6225m-filter-rotation = Drehung

d-twd6225m-filter-crop_left = Links beschneiden

d-twd6225m-filter-crop_right = Rechts beschneiden

d-twd6225m-filter-crop_top = Oben beschneiden

d-twd6225m-filter-crop_bottom = Unten beschneiden

d-twd6225m-filter-key_enabled = Farbe entfernen

d-twd6225m-filter-key_tolerance = Wie viel der Farbe

d-twd6225m-filter-key_softness = Weiche Kante

d-twd6225m-filter-key_spill = Farbstich entfernen

d-twd6225m-filter-mask_x = Maske waagerecht

d-twd6225m-filter-mask_y = Maske senkrecht

d-twd6225m-filter-mask_w = Maske Breite

d-twd6225m-filter-mask_h = Maske Höhe

d-twd6225m-filter-mask_rotation = Maske Drehung

d-twd6225m-filter-mask_feather = Maske Weichheit

d-twd6225m-filter-mask_invert = Maske umkehren

d-twd6225m-filter-link_follows = Verknüpfung folgt

d-twd6225m-filter-sound_split = Ton abgetrennt

d-ta4fbzv3 = Aufnahmeeffekt
    .noun = { $count ->
        [one] shot effect
       *[other] shot effects
    }

d-ai7thy2p = Id

d-seoccvzw = Form

d-y2yjk3ou = Stärke

d-ta4fbzv3-filter-amount = Stärke

d-4ashukng = Aufnahmeschlüsselbild
    .noun = { $count ->
        [one] shot key
       *[other] shot keys
    }

d-fpeyg4lm = Feld

d-frbcwy5c = Zeit

d-hxcqnoii = Wert

d-pf3f6zou = Verlauf

d-4ashukng-filter-t = Zeit

d-4ashukng-filter-v = Wert

d-uwrwmii3 = Bildstreifenfrage
    .doc = Das Bild einer Quelle bei `at`, behalten: ein Bildstreifenbild.
    .noun = { $count ->
        [one] thumb ask
       *[other] thumb asks
    }

d-zbk6etmx = Adresse

d-mom4w6zg = Bei

d-qdxfi3dv = Breite

d-a4wvyrpr = Höhe

d-uwrwmii3-filter-at = Bei

d-uwrwmii3-filter-width = Breite

d-uwrwmii3-filter-height = Höhe

d-3eadse7q = Farbfrage
    .doc = Die Farbe des Bildes des Schnitts bei (x, y), als Anteile seines Kastens.
    .noun = { $count ->
        [one] sample ask
       *[other] sample asks
    }

d-47t7hgmm = Schnitt

d-lph6xeeo = Bei

d-hpbggrop = X

d-nklgpqyv = Y

d-3eadse7q-filter-at = Bei

d-3eadse7q-filter-x = X

d-3eadse7q-filter-y = Y

d-24bzwas5 = Farbantwort
    .noun = { $count ->
        [one] sample answer
       *[other] sample answers
    }

d-twwt37df = Rot

d-6j7i6jgf = Grün

d-qbxkptqm = Blau

d-24bzwas5-filter-red = Rot

d-24bzwas5-filter-green = Grün

d-24bzwas5-filter-blue = Blau

d-ofec2skm = Aufnahmemarke
    .noun = { $count ->
        [one] shot marker
       *[other] shot markers
    }

d-5evudwdu = Id

d-bh2yiei5 = Zeit

d-ajyxetst = Name

d-2wvqaalc = Palette

d-ueznappo = Notiz

d-ofec2skm-filter-t = Zeit

d-ofec2skm-filter-palette = Palette

d-rsaogmjm = Gespeicherter Schnitt
    .doc = Eine Projektdatei, wie sie geschrieben wird: was eine Person jemand anderem gibt.
    .noun = { $count ->
        [one] saved
       *[other] saveds
    }

d-y5f2zb7y = Name

d-pfj7sab5 = Bilder/s

d-4nw6mpgl = Breite

d-tyqdqd4o = Höhe

d-idvvsvmm = Anfang markieren

d-dadn3d73 = Ende markieren

d-hzulw2ku = Quelle
    .doc = Ein JSON-Text je Quelle.

d-sgtdffn3 = Spuren
    .doc = Ein JSON-Text je Spur.

d-tgtkyvzv = Clips
    .doc = Ein JSON-Text je Clip.

d-mbkqck5v = Marken
    .doc = Ein JSON-Text je Marke.

d-rsaogmjm-filter-fps = Bilder/s

d-rsaogmjm-filter-width = Breite

d-rsaogmjm-filter-height = Höhe

d-rsaogmjm-filter-mark_in = Anfang markieren

d-rsaogmjm-filter-mark_out = Ende markieren

d-bykmbwv6 = Gespeicherte Quelle
    .noun = { $count ->
        [one] saved media
       *[other] saved media
    }

d-y4ivogi5 = Name

d-4yybplx2 = Form

d-lrk7dehr = Dauer

d-zzc32v7l = Breite

d-qj5gefuh = Höhe

d-23dpu2sh = Bilder/s

d-soogrhvl = Palette

d-ooln2ibo = Adresse

d-ydskn4p7 = Hat Ton

d-im5ank6o = Worte

d-tmrphs4y = Größe

d-kqelnlf5 = Rot

d-vikauvue = Grün

d-l67cmsns = Blau

d-ozd3qrh5 = Deckkraft

d-ytkxp4t4 = Ausrichtung

d-oecwbkzv = Fett

d-qefzzmnw = Kursiv

d-npetd5wt = Hinterlegung

d-bykmbwv6-filter-duration = Dauer

d-bykmbwv6-filter-width = Breite

d-bykmbwv6-filter-height = Höhe

d-bykmbwv6-filter-fps = Bilder/s

d-bykmbwv6-filter-palette = Palette

d-bykmbwv6-filter-has_audio = Hat Ton

d-bykmbwv6-filter-size = Größe

d-bykmbwv6-filter-red = Rot

d-bykmbwv6-filter-green = Grün

d-bykmbwv6-filter-blue = Blau

d-bykmbwv6-filter-alpha = Deckkraft

d-bykmbwv6-filter-bold = Fett

d-bykmbwv6-filter-italic = Kursiv

d-bykmbwv6-filter-backdrop = Hinterlegung

d-5wot45br = Gespeicherte Spur
    .noun = { $count ->
        [one] saved track
       *[other] saved tracks
    }

d-hahxd2bf = Name

d-6eogjiom = Form

d-kpfeeasv = Reihenfolge

d-objfxiib = Stumm

d-xucxzrou = Solo

d-ziybhwce = Gesperrt

d-mddfj6qk = Verstärkung dB

d-kmg2laxv = Panorama

d-5wot45br-filter-index = Reihenfolge

d-5wot45br-filter-muted = Stumm

d-5wot45br-filter-solo = Solo

d-5wot45br-filter-locked = Gesperrt

d-5wot45br-filter-gain_db = Verstärkung dB

d-5wot45br-filter-pan = Panorama

d-werb5t7p = Gespeicherter Clip
    .noun = { $count ->
        [one] saved clip
       *[other] saved clips
    }

d-xelrculp = Name

d-nakmpr6q = Spur

d-b6xll7vm = Quelle

d-3styztut = Anfang

d-fosnfakw = Einstieg

d-fyyrf7ca = Ausstieg

d-kl2xarv5 = Deckkraft

d-xjyxglh4 = Lautstärke

d-3ayzf2x5 = Übergang

d-dqqoukz2 = Übergangsdauer

d-ked6w3da = Einblenden

d-gs542y2n = Ausblenden

d-xuo76wt5 = Geschwindigkeit

d-joyeavyw = Rückwärts

d-y7jv62ls = Helligkeit

d-g5rdmkth = Kontrast

d-zgf5atrf = Sättigung

d-r4zzuhna = Breite

d-u3nbauiz = Höhe

d-7rxjc3mu = Waagerecht

d-4fkenwwm = Senkrecht

d-n2rqdsw7 = Drehung

d-fycn5pvk = Links beschneiden

d-2tb4h5uc = Rechts beschneiden

d-7madt25j = Oben beschneiden

d-3wy2xkbg = Unten beschneiden

d-2ui2wpyh = Farbe entfernen

d-cxqyoowf = Wie viel der Farbe

d-rjd3q7ct = Weiche Kante

d-avubfu74 = Farbstich entfernen

d-smqr4d4n = Maskenform

d-yr4ncz4w = Maske waagerecht

d-7g7aeosa = Maske senkrecht

d-jasgkfww = Maske Breite

d-f4dke5o2 = Maske Höhe

d-fdrernpm = Maske Drehung

d-to3xcmob = Maske Weichheit

d-6dr3jtxx = Maske umkehren

d-pdhb3rjf = Verknüpfung

d-6oop3g3z = Schlüsselbilder
    .doc = Ein JSON-Text je Schlüsselbild des Clips.

d-werb5t7p-filter-track = Spur

d-werb5t7p-filter-palette = Palette

d-werb5t7p-filter-key_red = Schlüssel Rot

d-werb5t7p-filter-key_green = Schlüssel Grün

d-werb5t7p-filter-key_blue = Schlüssel Blau

d-werb5t7p-filter-media = Quelle

d-werb5t7p-filter-start = Anfang

d-werb5t7p-filter-in_point = Einstieg

d-werb5t7p-filter-out_point = Ausstieg

d-werb5t7p-filter-opacity = Deckkraft

d-werb5t7p-filter-volume = Lautstärke

d-werb5t7p-filter-transition_duration = Übergangsdauer

d-werb5t7p-filter-fade_in = Einblenden

d-werb5t7p-filter-fade_out = Ausblenden

d-werb5t7p-filter-speed_rate = Geschwindigkeit

d-werb5t7p-filter-reverse = Rückwärts

d-werb5t7p-filter-brightness = Helligkeit

d-werb5t7p-filter-contrast = Kontrast

d-werb5t7p-filter-saturation = Sättigung

d-werb5t7p-filter-scale_x = Breite

d-werb5t7p-filter-scale_y = Höhe

d-werb5t7p-filter-pos_x = Waagerecht

d-werb5t7p-filter-pos_y = Senkrecht

d-werb5t7p-filter-rotation = Drehung

d-werb5t7p-filter-crop_left = Links beschneiden

d-werb5t7p-filter-crop_right = Rechts beschneiden

d-werb5t7p-filter-crop_top = Oben beschneiden

d-werb5t7p-filter-crop_bottom = Unten beschneiden

d-werb5t7p-filter-key_enabled = Farbe entfernen

d-werb5t7p-filter-key_tolerance = Wie viel der Farbe

d-werb5t7p-filter-key_softness = Weiche Kante

d-werb5t7p-filter-key_spill = Farbstich entfernen

d-werb5t7p-filter-mask_x = Maske waagerecht

d-werb5t7p-filter-mask_y = Maske senkrecht

d-werb5t7p-filter-mask_w = Maske Breite

d-werb5t7p-filter-mask_h = Maske Höhe

d-werb5t7p-filter-mask_rotation = Maske Drehung

d-werb5t7p-filter-mask_feather = Maske Weichheit

d-werb5t7p-filter-mask_invert = Maske umkehren

d-ng75u2ju = Gespeicherte Marke
    .noun = { $count ->
        [one] saved marker
       *[other] saved markers
    }

d-s6k7ckvj = Name

d-bglc57r5 = Zeit

d-2zmyfxy2 = Palette

d-uqvihrf3 = Notiz

d-ng75u2ju-filter-t = Zeit

d-ng75u2ju-filter-palette = Palette




d-mciac4w4 = Form
    .doc = Was eine Quelle ist: ein bewegtes Bild, ein Ton, ein Standbild oder Worte, die diese App selbst zeichnet.

d-zblz6mlg = Video

d-ynngpmma = Ton

d-tojmog4r = Bild

d-7xc3xbut = Titel

d-lpnhv5tz = Ausrichtung
    .doc = Wie die Zeilen eines Titels in seinem Kasten stehen.

d-o6rryp7r = Links

d-xogrqbl7 = Mitte

d-gcbw3mrx = Rechts






d-ekz7suen = Teilen bei
    .doc = Teilt einen Clip bei `at` in zwei, jede Hälfte behält ihren Teil der Quelle.

d-ekz7suen-c = C

d-ekz7suen-at = Bei

d-vt2rq6ml = Verschoben
    .doc = Bewegt `c` (und seinen verknüpften Ton) nach `start` und lehnt eine Überlappung ab.

d-vt2rq6ml-c = C

d-vt2rq6ml-start = Anfang

d-qz7qr2ru = Störung

d-qz7qr2ru-why = Grund

d-e7mxkoe3 = Aufnehmen
    .doc = Fragt das Gerät, was eine Datei ist, fügt sie als Quelle hinzu und zeichnet ihr Bild und ihre Welle.

d-e7mxkoe3-file = Datei

d-s34s56u6 = Gelesenes übernehmen
    .doc = Schreibt, was das Gerät von einer Quelle gelesen hat, auf sie, so wie die Engine es gesagt hat: was sie ist, wie lange sie läuft, ihre Größe und Rate, und ob sie Ton trägt. Eine Action bekommt die Worte der Engine statt des gelesenen `Probed`, eines Werts, den keine Ressource hält.

d-s34s56u6-m = Quelle

d-s34s56u6-said = Antwort

d-trebb62h = Quelle prüfen
    .doc = Prüft, ob die Bytes einer Quelle hier sind: liest erneut, was sie sind, zeichnet ihr Vorschaubild und ihre Wellenform und markiert sie als nicht auf diesem Gerät, wenn die Engine sie nicht erreicht.

d-trebb62h-m = Quelle

d-rf7qyeis = Quelle neu verknüpfen
    .doc = Verweist eine Quelle auf eine Datei, die die Person gewählt hat: die Clips sind Schnitte der Quelle und behalten sie, und die Quelle liest, was ihre neue Datei sagt.

d-rf7qyeis-m = Quelle

d-rf7qyeis-file = Datei

d-g77prxmq = Als fehlend markieren
    .doc = Sagt, dass die Bytes der Quelle nicht auf diesem Gerät sind, und warum die Engine sie nicht lesen konnte.

d-g77prxmq-m = Quelle

d-g77prxmq-why = Warum

d-o5pyx57x = Korbbild zeichnen
    .doc = Das Bild der Quelle, wie der Korb es zeigt.

d-o5pyx57x-width = Breite

d-csrgoth7 = Wellen lesen
    .doc = Die Lautstärke der Quelle über die Datei, wie die Welle sie zeichnet.

d-blq35ymn = Bild zeichnen
    .doc = Das Bild, das die Vorschau bei `at` zeigt, von der Engine gezeichnet.

d-blq35ymn-at = Bei

d-ozvvnp6f = Rendern beginnen
    .doc = Startet das Rendern, das die Datei schreibt, die eine Person sichert.

d-ozvvnp6f-resolution = Auflösung

d-ozvvnp6f-container = Container

d-ozvvnp6f-fps = Bilder/s

d-tpulmxks = Neues Projekt
    .doc = Startet einen Schnitt: seine Form und seine ersten beiden Spuren.
    .w-untitled-project = Unbenanntes Projekt

d-6oillx2m = Schnitt öffnen
    .doc = Öffnet den Schnitt, den das Fenster zeigt.

d-6oillx2m-p = P

d-heiumxwf = Sitzung
    .doc = Die Sitzung, über die dieses Fenster abspielt: die vorhandene oder eine neue.

d-msh6ollo = Schnitt umbenennen

d-msh6ollo-name = Name

d-p2rzlwd5 = Projekt löschen

d-p2rzlwd5-p = P

d-jly5vvua = Dateien aufnehmen
    .doc = Fügt jede gewählte oder abgelegte Datei als Quelle hinzu und sagt, welche Dateien nicht ankamen und was das Gerät zu jeder sagte.
    .w-not-added = Nicht hinzugefügt: { $join }

d-jly5vvua-files = Dateien

d-q7ohei6b = Eine Datei aufnehmen

d-q7ohei6b-file = Datei

d-g5zie4f5 = In der Videobearbeitung öffnen
    .doc = Öffnet ein Video, einen Ton oder ein Bild in einem eigenen Schnitt: wofür Dateien und das System die Bearbeitung anbieten.

d-g5zie4f5-file = Datei

d-kby4rl22 = Ein Projekt öffnen
    .doc = Öffnet eine gewählte Projektdatei: eine rückgängig machbare Änderung, die den enthaltenen Schnitt macht.

d-kby4rl22-file = Datei

d-ywy6v3p3 = Projekt sichern
    .doc = Der Schnitt als Datei, die eine Person behält: seine Quellen und ihre Stellen, wo sie auf ihrem eigenen Gerät stehen.

d-b7tqrowc = Film sichern
    .doc = Der Schnitt als Film: was ein Export gemacht hat.

d-4occtx3e = Legen
    .doc = Legt `m` auf `track` bei `at` an die erste Stelle, an die sie passt.

d-4occtx3e-m = M

d-4occtx3e-track = Spur

d-4occtx3e-at = Bei

d-puz7vwtv = Quelle legen
    .doc = Legt die Quelle des Korbs auf die erste Spur ihrer Form, bei `at`.

d-puz7vwtv-m = M

d-puz7vwtv-at = Bei

d-j47xlg5b = Ziehen festschreiben
    .doc = Was ein Ziehen der Zeitleiste festgeschrieben hat: ein bewegter oder beschnittener Clip.

d-j47xlg5b-grab = Griff

d-j47xlg5b-clip = Clip

d-j47xlg5b-from = Von

d-j47xlg5b-at = Bei

d-deatp4s7 = Clip verschieben
    .doc = Verschiebt einen Clip nach `start`, wie ein Ziehen auf der Zeitleiste ihn ablegt, und jeden anderen Clip unter `with`, den gewählten, um ebenso viel. Ein allein verschobener Clip kommt auf die Spur, die `track` nennt, wenn sie eine andere Spur seiner Art ist, nicht gesperrt, mit Platz für ihn.

d-deatp4s7-c = Clip

d-deatp4s7-start = Anfang

d-deatp4s7-track = Spur

d-deatp4s7-with = Mit

d-6earukeg = Clip zuschneiden
    .doc = Schneidet einen Clip so zu, dass er bei `start` beginnt und `length` dauert, wie ein Ziehen an seiner Kante auf der Zeitleiste ihn ablegt: ein verschobener Anfang schneidet seinen Kopf, sonst sein Ende.

d-6earukeg-c = Clip

d-6earukeg-start = Anfang

d-6earukeg-length = Länge

d-dgd6pyot = Marke verschieben
    .doc = Verschiebt eine Marke nach `at`, auf das nächste Bild.

d-dgd6pyot-k = Marke

d-dgd6pyot-at = Bei

d-sy5ehyjx = Clip teilen
    .doc = Teilt den Clip an der Abspielmarke in zwei.

d-sy5ehyjx-c = C

d-sy5ehyjx-at = Bei

d-supdi5qb = Teilen bei
    .doc = Teilt jeden Clip, auf dem die Abspielmarke steht.

d-supdi5qb-at = Bei

d-lln5pwvg = Lücke schließen
    .doc = Löscht die Clips und schließt die Lücke, die jeder auf seiner Spur hinterlässt.

d-lln5pwvg-cs = Clips

d-ecft5dqw = Verschieben
    .doc = Verschiebt die Clips um Bilder auf der Zeitleiste.

d-ecft5dqw-cs = Clips

d-ecft5dqw-frames = Bilder

d-ne4it5nd = Geschwindigkeit setzen
    .doc = Setzt die Geschwindigkeit eines Clips: 2 doppelt so schnell und halb so lang, 0.5 in Zeitlupe.

d-ne4it5nd-speed = Geschwindigkeit

d-ig42742r = Normale Geschwindigkeit

d-lzc5gbav = Übergang setzen
    .doc = Setzt den eingehenden Übergang und wie lange er läuft.

d-lzc5gbav-transition = Übergang

d-lzc5gbav-seconds = Sekunden

d-4vxaibys = Schlüsseln bei
    .doc = Setzt ein Schlüsselbild von `param` bei `at` (Sekunden der Zeitleiste) auf den Wert, den das Feld jetzt hält.

d-4vxaibys-at = Bei

d-4vxaibys-param = Feld

d-4vxaibys-interp = Verlauf

d-jmrgc7np = Entschlüsseln
    .doc = Entfernt ein Schlüsselbild.

d-jmrgc7np-k = K

d-7b5o6mn3 = Verlauf setzen
    .doc = Wie die Kurve eines Schlüsselbilds es verlässt.

d-7b5o6mn3-interp = Verlauf

d-ou2lcuwq = Farbe aufnehmen
    .doc = Nimmt die Farbe des Bildes dort, wo die Person gezeigt hat, als die Farbe, die der Key entfernt.

d-ou2lcuwq-c = C

d-ou2lcuwq-at = Bei

d-ou2lcuwq-x = X

d-ou2lcuwq-y = Y

d-oaa6shnc = Leinwandfarbe
    .doc = Setzt die Farbe, die der Key entfernt, auf eine der üblichen Leinwände.

d-oaa6shnc-red = Rot

d-oaa6shnc-green = Grün

d-oaa6shnc-blue = Blau

d-5rq4qoik = Bildstreifenbild
    .doc = Ein Bild einer Quelle bei `at` Sekunden darin, als Bildstreifenbild behalten.

d-5rq4qoik-at = Bei

d-5rq4qoik-width = Breite

d-5rq4qoik-height = Höhe

d-xqw33rc3 = Bildstreifen
    .doc = Zeichnet acht Bilder des Clips über seinen eigenen Teil, behalten, bis ein Beschnitt sie neu verlangt.

d-xqw33rc3-c = C

d-p7dk5fld = Marke beschriften
    .doc = Schreibt, was eine Marke sagt.

d-p7dk5fld-note = Notiz

d-uaxqakao = Nächsten Bildstreifen zeichnen
    .doc = Zeichnet den Bildstreifen des ersten Clips, dessen Bilder fehlen oder veraltet sind.

d-57xowwoj = Ausrichtung setzen
    .doc = Setzt, wo die Zeilen eines Titels in seinem Kasten stehen.

d-57xowwoj-align = Ausrichtung

d-fxlzpync = Titel hinzufügen
    .doc = Fügt einen Titel zum Korb hinzu und legt ihn bei `at` an.
    .w-title = Titel

d-fxlzpync-words = Worte

d-fxlzpync-at = Bei

d-ftp5jwbo = Marke hinzufügen
    .doc = Setzt eine Marke bei `at`.
    .w-marker = Marke { $n }

d-ftp5jwbo-at = Bei

d-hjooda4n = Marke entfernen

d-hjooda4n-k = K

d-ilbzfadr = Anfang markieren

d-ilbzfadr-at = Bei

d-wx6zxdj6 = Ende markieren

d-wx6zxdj6-at = Bei

d-td2tkfzd = Marken löschen

d-7l3mdo7j = Bildrate setzen

d-7l3mdo7j-fps = Bilder/s

d-7kef53r3 = Bildgröße setzen

d-7kef53r3-width = Breite

d-7kef53r3-height = Höhe

d-b3pi3srw = Format übernehmen
    .doc = Übernimmt seine Form von einer Quelle: was „Dessen Format übernehmen“ tut.

d-b3pi3srw-m = M

d-yrqdwima = Spur hinzufügen

d-yrqdwima-form = Form

d-bztzhcrl = Spur löschen

d-bztzhcrl-t = Zeit

d-odoaypxs = Clips verknüpfen
    .doc = Macht zwei Clips zu Bild und Ton einer Aufnahme: sie bewegen und schneiden sich zusammen.

d-odoaypxs-a = A

d-odoaypxs-b = B

d-l5aevct6 = Verknüpfung lösen

d-m4ovyotr = Ton abtrennen
    .doc = Verschiebt den eigenen Ton eines Bildclips auf einen eigenen Tonclip, mit ihm verknüpft.

d-m4ovyotr-c = C

d-iry5qrqv = Effekt hinzufügen

d-iry5qrqv-form = Form

d-7gjywuct = Effekt entfernen

d-7gjywuct-e = E

d-aqdtfj76 = Vorschau
    .doc = Zeichnet das Bild, das die Vorschau bei `at` zeigt.

d-aqdtfj76-at = Bei





d-mrqvrzq6 = Schnitt exportieren
    .doc = Rendert den Schnitt in eine Datei und weigert sich, wenn eine Quelle, die dieses Gerät nicht lesen kann, im Film ihr Material fehlen ließe.
    .w-cannot-export-is-not-on-this = Nicht exportierbar: { $join } ist nicht auf diesem Gerät, im Film fehlte dieses Material

d-mrqvrzq6-resolution = Auflösung

d-mrqvrzq6-container = Container

d-mrqvrzq6-fps = Bilder/s

d-fdcfxbsk = Rendern abfragen
    .doc = Fragt, wie weit das laufende Rendern ist.

d-5m5ch2tv = Rendern abbrechen

d-pupgh4ue = Formworte
    .w-sound = Ton
    .w-still = Standbild
    .w-title = Titel
    .w-video = Video

d-d4536bfd = Verlaufsworte
    .w-hold = Halten
    .w-ease = Weich
    .w-linear = Linear

d-kga7o3ur = Fenster
    .w-edit = Bearbeiten
    .w-select-all-clips = Alle Clips auswählen
    .w-copy = Kopieren
    .w-paste-at-the-playhead = An der Abspielmarke einfügen
    .w-duplicate = Duplizieren
    .w-delete-and-close-the-gap = Löschen und Lücke schließen
    .w-trim-the-start-to-the-playhead = Anfang zur Abspielmarke kürzen
    .w-trim-the-end-to-the-playhead = Ende zur Abspielmarke kürzen
    .w-nudge-a-frame-earlier = Ein Bild früher
    .w-nudge-a-frame-later = Ein Bild später
    .w-nudge-a-second-earlier = Eine Sekunde früher
    .w-nudge-a-second-later = Eine Sekunde später
    .w-previous-marker = Vorige Marke
    .w-next-marker = Nächste Marke
    .w-go-to-the-clip = Zum Clip gehen
    .w-go-to-1-02-5 = Gehe zu 1:02.5
    .w-rendering-the-cut = Schnitt wird gerendert… { $pct } %
    .w-the-film-is-ready = { $name } — der Film ist fertig
    .w-video-editor = Videobearbeitung
    .w-new-project = Neues Projekt
    .w-projects = Projekte
    .w-sources = Quellen
    .w-drop-video-sound-or-pictures = Video, Ton oder Bilder ablegen
    .w-they-become-clips-of-this-cut = Sie werden Clips dieses Schnitts
    .w-not-on-this-device = Nicht auf diesem Gerät
    .w-title = Titel
    .w-nothing-to-show-yet = Noch nichts zu sehen
    .w-add-a-source-and-the-frame = Füge eine Quelle hinzu, dann steht hier das Bild an der Abspielmarke
    .w-play-backwards-faster-with-each-press = Rückwärts abspielen, mit jedem Druck schneller
    .w-play-faster-with-each-press = Schneller abspielen, mit jedem Druck
    .w-width-height-and-turning-go-around = Breite, Höhe und Drehen laufen um diesen Punkt des Bildes
    .w-anchor = Anker
    .w-top-left = Oben links
    .w-top = Oben
    .w-top-right = Oben rechts
    .w-bottom-left = Unten links
    .w-bottom = Unten
    .w-bottom-right = Unten rechts
    .w-this-film-needs-sources-this-device = Dieser Film braucht Quellen, die dieses Gerät nicht hat
    .w-relink-it-in-the-bin-or = { $join }: verknüpfe sie im Korb neu oder exportiere auf dem Gerät, das sie hat
    .w-back-a-second = Eine Sekunde zurück
    .w-pause = Pause
    .w-play-the-cut = Schnitt abspielen
    .w-forward-a-second = Eine Sekunde vor
    .w-mark-in = Anfang markieren
    .w-mark-out = Ende markieren
    .w-clear = Marken löschen
    .w-frame = Einzelbild
    .w-what-the-cut-plays-through = Worüber der Schnitt läuft
    .w-rendering-the-film = Film wird gerendert
    .w-stop-the-render = Rendern stoppen
    .w-the-engine-could-not-do-that = Das konnte die Engine nicht
    .w-zoom-out = Verkleinern
    .w-zoom-in = Vergrößern
    .w-fit = Einpassen
    .w-earlier = Früher
    .w-later = Später
    .w-split = Teilen
    .w-delete = Löschen
    .w-marker = Marke
    .w-add-a-video-track = Videospur hinzufügen
    .w-add-a-sound-track = Tonspur hinzufügen
    .w-the-timeline = Die Zeitleiste
    .w-inspector = Inspektor
    .w-clip = Clip
    .w-look = Aussehen
    .w-effects = Effekte
    .w-name = Name
    .w-opacity = Deckkraft
    .w-volume = Lautstärke
    .w-speed = Geschwindigkeit
    .w-play-it-backwards = Rückwärts abspielen
    .w-fade-in-seconds = Einblenden, Sekunden
    .w-fade-out-seconds = Ausblenden, Sekunden
    .w-transition = Übergang
    .w-cut = Schnitt
    .w-dissolve = Überblenden
    .w-fade-in = Einblenden
    .w-dip-to-black = Schwarzblende
    .w-wipe = Wischen
    .w-match = An { $name } anpassen
    .w-split-the-sound = Ton abtrennen
    .w-unlink = Verknüpfung lösen
    .w-marker-here = Marke hier
    .w-sound-level = Tonpegel
    .w-brightness = Helligkeit
    .w-contrast = Kontrast
    .w-saturation = Sättigung
    .w-width = Breite
    .w-across = Waagerecht
    .w-up-and-down = Senkrecht
    .w-turn-degrees = Drehen, Grad
    .w-remove-a-colour = Eine Farbe entfernen
    .w-screen-colour = Leinwandfarbe
    .w-green = Grün
    .w-blue = Blau
    .w-magenta = Magenta
    .w-pick-from-the-frame = Aus dem Bild aufnehmen
    .w-drag-across-the-picture-where-the = Ziehe über das Bild, wo die Farbe ist
    .w-pick-a-colour-from-the-frame = Eine Farbe aus dem Bild aufnehmen
    .w-how-much-of-it = Wie viel davon
    .w-soft-edge = Weiche Kante
    .w-take-the-tint-away = Farbstich entfernen
    .w-keys = Schlüsselbilder
    .w-key-a-field-here = Ein Feld hier schlüsseln
    .w-turn = Drehen
    .w-this-clip-holds-one-value-all = Dieser Clip hält einen Wert bis zum Ende
    .w-linear = Linear
    .w-hold = Halten
    .w-ease = Weich
    .w-remove-the-key = Schlüsselbild entfernen
    .w-frames = Bilder
    .w-draw-the-filmstrip = Bildstreifen zeichnen
    .w-add-an-effect = Effekt hinzufügen
    .w-grayscale = Graustufen
    .w-sepia = Sepia
    .w-invert = Invertieren
    .w-warm = Warm
    .w-cool = Kalt
    .w-vignette = Vignette
    .w-this-clip-has-no-effect-of = Dieser Clip hat keinen eigenen Effekt
    .w-remove = { $form } entfernen
    .w-words = Worte
    .w-size = Größe
    .w-red = Rot
    .w-bold = Fett
    .w-italic = Kursiv
    .w-lines = Ausrichtung
    .w-left = Links
    .w-centre = Mitte
    .w-right = Rechts
    .w-this-device-cannot-read-the-source = Dieses Gerät kann die Quelle nicht lesen: sie ist nicht hier, oder ein anderes Gerät hält sie. Verknüpfe sie neu mit einer ihrer Dateien.
    .w-place-at-the-playhead = An die Abspielmarke legen
    .w-relink-the-source = Quelle neu verknüpfen
    .w-check-this-device = Dieses Gerät prüfen
    .w-choose-a-clip-or-a-source = Wähle einen Clip oder eine Quelle
    .w-markers = Marken
    .w-remove-the-mark = Marke entfernen
    .w-what-it-marks = Was sie markiert
    .w-the-cut = Der Schnitt
    .w-frames-a-second = Bilder pro Sekunde
    .w-height = Höhe
    .w-renders-from = Rendert von
    .w-to = bis
    .w-export = Export
    .w-mp4 = mp4
    .w-webm = webm
    .w-mov = mov
    .w-format = Format
    .w-render-the-film = Film rendern
    .w-save-the-film = Film sichern
    .w-no-cut-yet = Noch kein Schnitt
    .w-start-one-or-open-a-video = Starte einen oder öffne ein Video in der Bearbeitung
    .w-jump-to-the-start = Zum Anfang
    .w-back-a-frame = Ein Bild zurück
    .w-forward-a-frame = Ein Bild vor
    .w-jump-to-the-end = Zum Ende
    .w-open-a-project = Projekt öffnen
    .w-save-the-project = Projekt speichern
    .w-mask = Maske
    .w-shape = Form
    .w-off = Aus
    .w-rectangle = Rechteck
    .w-ellipse = Ellipse
    .w-keep-the-outside = Das Äußere behalten
    .w-centre-across = Mitte waagerecht
    .w-centre-up-and-down = Mitte senkrecht
    .w-mask-width = Maskenbreite
    .w-mask-height = Maskenhöhe
    .w-mask-turn-degrees = Maske drehen, Grad
    .w-mask-feather = Weiche Maskenkante
    .w-crop = Zuschneiden
    .w-from-the-left = Von links
    .w-from-the-right = Von rechts
    .w-from-the-top = Von oben
    .w-from-the-bottom = Von unten
    .w-transition-seconds = Übergang, Sekunden
    .w-amount = Stärke
    .w-move-earlier = Nach oben
    .w-move-later = Nach unten
    .w-remove-every-effect = Alle Effekte entfernen
    .w-delete-the-source = Quelle löschen
    .w-tracks = Spuren
    .w-mute = Stumm
    .w-solo = Solo
    .w-lock = Sperren
    .w-level-db = Pegel, dB
    .w-pan = Panorama
    .w-reset-the-level-and-pan = Pegel und Panorama zurücksetzen
    .w-delete-the-track = Spur löschen
    .w-backing-plate = Hinterlegung
    .w-system-sans = System-Sans
    .w-curves = Kurven
    .w-all = Alle
    .w-the-curve = Die Kurve { $curve_channel }
    .w-how-much-of-the-curve = Wie viel von der Kurve
    .w-reset-the-curve = Kurve zurücksetzen
    .w-fill-frame = Bildfeld füllen
    .w-fit-it-to-the-picture = An das Bild anpassen
    .w-go-to-where-the-mix-clipped = Dorthin, wo der Mix übersteuert hat
    .w-go-to-where-it-clipped = Zur Übersteuerung springen
    .w-the-mix-is-over-full-scale = Der Mix liegt { $over_words } über Vollaussteuerung
    .w-the-mix-is-the-sum-of = Der Mix ist die Summe aller Spuren; alle zusammen leiser zu machen erhält die Balance
    .w-in-places-the-mix-is-the = An { $len } Stellen; der Mix ist die Summe aller Spuren, alle zusammen leiser zu machen erhält die Balance
    .w-bring-the-mix-down = Mix leiser machen
    .w-check-the-mix = Mix prüfen
    .w-rename-the-mark = Marke umbenennen

d-bi2ut6h2 = Studio
    .w-open-in-the-editor = In der Bearbeitung öffnen
    .w-clips = { $clips } Clips
    .w-this-cut-opens-in-the-video = Dieser Schnitt öffnet in der Videobearbeitung
    .w-its-preview-its-timeline-and-its = Vorschau, Zeitleiste und Inspektor stehen im Fenster der App

d-kfetalt5 = Schnittzeile

d-kqgtcock = Quellenzeile
    .w-title = Titel

d-pjmjk5qx = Aussehen
    .doc = Eine Eigenschaft eines Clips, die der Inspektor mit einem Regler setzt: Bild und Ton des Clips, die die Vorschau beim Ändern neu zeichnet.

d-7bz76hhu = Deckkraft

d-ackkweow = Lautstärke

d-mznepkyb = Geschwindigkeit

d-t3a66fy3 = Einblenden

d-xwb5szc5 = Ausblenden

d-zkfola5e = Helligkeit

d-4dzgy5u5 = Kontrast

d-scr3ivvj = Sättigung

d-nt3j7qwm = Breite

d-zzsb76qw = Höhe

d-3t54kflk = Waagerecht

d-ctks3e6u = Senkrecht

d-j54iofuh = Drehung

d-nayfsbeu = Links beschneiden

d-u4yy2sy5 = Rechts beschneiden

d-66tcewmf = Oben beschneiden

d-pl5pnpzn = Unten beschneiden

d-2fynrz2a = Toleranz der Farbauswahl

d-b446mw3c = Weiche Kante

d-ki4feltm = Farbstich entfernen

d-qi4f4ifl = Maske, Mitte waagerecht

d-6qeg3bqq = Maske, Mitte senkrecht

d-xn3ula5j = Maskenbreite

d-6z4blfug = Maskenhöhe

d-zrovxhbl = Maske, Drehung

d-2hwgozrm = Maske, Weichzeichnen

d-4gl2etau = Übergangslänge

d-yu5ipsqv = Titelaussehen
    .doc = Eine Eigenschaft eines Titels, die der Inspektor mit einem Regler setzt und die beim Ändern neu gezeichnet wird.

d-yf2mstzg = Größe

d-xbmye2tx = Rot

d-qagwwldb = Grün

d-g62cfe37 = Blau

d-vl4qw7vt = Hinterlegung

d-pugj26z7 = Aussehen ändern
    .doc = Setzt eine Eigenschaft des Clips und zeichnet das Bild der Vorschau neu, sodass eine Änderung sofort zu sehen ist.

d-bjresjz2 = Maske setzen
    .doc = Legt fest, welche Form begrenzt, wo ein Clip zu sehen ist: `off`, `rect` oder `ellipse`.
    .w-is-not-a-mask-shape = { $shape } ist keine Maskenform

d-mikjlhmy = Maske umkehren
    .doc = Behält das Innere der Maske des Clips oder ihr Äußeres.

d-bwheqtmt = Rückwärts abspielen
    .doc = Spielt die Quelle des Clips rückwärts oder wieder vorwärts und zeichnet das Bild neu, das sie jetzt zeigt.

d-2cricpxh = Titelstil ändern
    .doc = Eine Stileigenschaft der Zeilen eines Titels, die beim Ändern neu gezeichnet wird.

d-xluldt6f = Spurniveau ändern
    .doc = Der Pegel einer Spur, also wo der Ton von allem auf ihr im Mix liegt.

d-j4nvruzm = Spurpanorama ändern
    .doc = Wo der Ton einer Spur im Stereofeld liegt: -1 ganz links, 1 ganz rechts.

d-dseablg3 = Spur stummschalten

d-32fpyxzd = Spur solo schalten

d-sjg7mrsa = Spur sperren

d-qwyfccp4 = Pegel und Panorama zurücksetzen
    .doc = Setzt eine Spur auf Einheit und Mitte zurück.

d-pmzyq3cp = Titelaussehen ändern

d-vgg5jf2p = Neu zeichnen
    .doc = Zeichnet das zuletzt gezeigte Bild neu, sodass eine danach gemachte Änderung sofort zu sehen ist.

d-puq37lmk = Clip neu zeichnen

d-czjm4yan = Quelle neu zeichnen

d-25a3ji5l = Wörter setzen
    .doc = Was die Zeilen des Titels sagen.

d-x23sha6o = Effektstärke ändern
    .doc = Wie stark ein Effekt des Clips wirkt, sofort auf das Bild angewendet.

d-xiuhdj77 = Effekt verschieben
    .doc = Verschiebt einen Effekt in der Reihenfolge des Clips nach oben oder unten: die Reihenfolge entscheidet, was das Bild zuerst durchläuft.

d-g7feka3j = Alle Effekte entfernen
    .doc = Nimmt jeden Effekt von einem Clip.

d-5oqtuzrw = Quelle löschen
    .doc = Nimmt eine Quelle aus der Ablage und jeden Clip, der sie zeigte, mit.

d-hw26iiy2 = Maske setzen

d-25a3ji5l-words = Wörter

d-2cricpxh-bold = Fett

d-2cricpxh-italic = Kursiv

d-32fpyxzd-solo = Solo

d-5oqtuzrw-m = Quelle

d-bjresjz2-shape = Form

d-bwheqtmt-reverse = Rückwärts

d-dseablg3-muted = Stumm

d-j4nvruzm-pan = Panorama

d-mikjlhmy-invert = Umkehren

d-pmzyq3cp-look = Aussehen

d-pmzyq3cp-value = Wert

d-pugj26z7-look = Aussehen

d-pugj26z7-value = Wert

d-sjg7mrsa-locked = Gesperrt

d-x23sha6o-amount = Stärke

d-x23sha6o-e = Effekt

d-xiuhdj77-e = Effekt

d-xiuhdj77-up = Nach oben

d-xluldt6f-db = Pegel in dB

d-j47xlg5b-with = Mit

d-6rakzxl3 = Clips löschen
    .doc = Löscht die Clips und den mit ihnen verknüpften Ton.

d-6rakzxl3-cs = Clips

d-kuigxhkm = Verschoben
    .doc = Verschiebt die Clips und ihren verknüpften Ton gemeinsam um Sekunden und lehnt eine Überlappung mit einem bleibenden Clip ab.

d-kuigxhkm-cs = Clips

d-kuigxhkm-by = Um

d-qi3ui4fs = Anfang zur Abspielmarke kürzen
    .doc = Kürzt den Anfang jedes Clips, in dem die Abspielmarke steht, bis zur Abspielmarke.

d-qi3ui4fs-cs = Clips

d-qi3ui4fs-at = Bei

d-cvwyptge = Ende zur Abspielmarke kürzen
    .doc = Kürzt das Ende jedes Clips, in dem die Abspielmarke steht, bis zur Abspielmarke.

d-cvwyptge-cs = Clips

d-cvwyptge-at = Bei

d-dpt2pbcu = Kopieren
    .doc = Legt die Clips mit ihrem Aussehen, ihren Schlüsselbildern und Effekten in die Zwischenablage.

d-dpt2pbcu-cs = Clips

d-wugmiukh = Ausschneiden
    .doc = Legt die Clips in die Zwischenablage und nimmt sie aus dem Schnitt.

d-wugmiukh-cs = Clips

d-n4ibspss = Spur zum Einfügen
    .doc = Die Spur, auf die ein eingefügter Clip kommt: seine eigene, solange sie im Schnitt und offen ist, sonst die erste offene seiner Art, sonst eine neue.

d-n4ibspss-track = Spur

d-n4ibspss-form_named = Art

d-wtvgvcst = Einfügen
    .doc = Setzt kopierte Clips ab der Stelle ein, der früheste dort und die anderen im selben Abstand zu ihm; miteinander verknüpfte Clips bleiben verknüpft, nur miteinander.

d-wtvgvcst-at = Bei

d-xmmw6vam = Duplizieren
    .doc = Setzt eine Kopie der Clips direkt hinter sie, auf dieselben Spuren.

d-xmmw6vam-cs = Clips

d-gwopftfd = Clip wiederherstellen
    .doc = Macht den gespeicherten Clip auf der Spur, aus der Quelle, ab der Stelle, mit seinen Schlüsselbildern und Effekten.

d-gwopftfd-saved = Gespeicherter Clip

d-gwopftfd-track = Spur

d-gwopftfd-media = Quelle

d-gwopftfd-start = Anfang

d-gwopftfd-link = Verknüpfung

d-vnvnjpak = Kopierte Clips
    .doc = Clips, wie eine Kopie sie in die Zwischenablage legt: jeder mit der Quelle und Spur, aus der er kam, damit ein Einfügen in denselben Schnitt sie wiederfindet.
    .noun = { $count ->
        [one] Kopie
       *[other] Kopien
    }

d-vnb2e4hr = Art

d-jrywinsx = Clips

d-zi4ddgd7 = Kopierter Clip
    .doc = Ein kopierter Clip: sein Aussehen, seine Zeiten und woher er kam.
    .noun = { $count ->
        [one] kopierter Clip
       *[other] kopierte Clips
    }

d-zi4ddgd7-filter-clip = Clip

d-fazzk3sz = Clip

d-636l34ow = Quelle

d-ebsvhwog = Pfad

d-uuax3sh3 = Spur

d-ep4wqebq = Art

d-igcwp2fn = Effekte
    .doc = Ein JSON-Text je Effekt des Clips, in Stapelreihenfolge.

d-nayfykgr = Schlüssel Rot

d-yl3a4tsd = Schlüssel Grün

d-zwo6h7ka = Schlüssel Blau

d-d6v4or6p = Palette

d-mbctkint = Anker X
    .doc = Der Punkt des Bildes, um den Skalieren und Drehen laufen und den eine Position verschiebt, als Anteil davon: 0,5, 0,5 ist seine Mitte, 0,0, 0,0 seine linke obere Ecke.

d-iyrfpfiq = Anker Y

d-aj2coyre-filter-anchor_x = Anker X

d-aj2coyre-filter-anchor_y = Anker Y

d-5drcv42q = Anker X

d-4whekfjf = Anker Y

d-kti7icrd = Live
    .doc = Die laufende Wiedergabe im Fenster, über ihren Engine-Stream: das Gerät setzt den Schnitt während der Wiedergabe zusammen, und das Fenster zeigt jedes Bild über `tilo-live://{ $live }`.

d-k4vs2hwv = Live-Geschwindigkeit
    .doc = Wie schnell diese Wiedergabe läuft: 1 bei normalem Tempo, mehr zum Shutteln, negativ rückwärts.

d-ulwqwedw = Live-Anfrage
    .doc = Ein live abgespielter Schnitt: das Gerät setzt ihn ab dem Abspielkopf zusammen und mischt den Ton für denselben Moment, und das Fenster zeigt jedes Bild, sobald es ankommt.
    .noun = { $count ->
        [one] Live-Anfrage
       *[other] Live-Anfragen
    }

d-ulwqwedw-filter-from = Von

d-ulwqwedw-filter-rate = Geschwindigkeit

d-d6da6bsq = Breite

d-rydoez3z = Höhe

d-ulwqwedw-filter-sound = Ton

d-bgnkfxlo = Projekt
    .doc = Der Schnitt, so wie `json.encode(shot(p))` ihn schreibt.

d-appc64zf = Von
    .doc = Wo der Abspielkopf startet, in Sekunden der Zeitleiste.

d-xr7l6ray = Geschwindigkeit
    .doc = Sekunden der Zeitleiste je Sekunde; 1 ist normales Tempo, und eine negative Geschwindigkeit spielt rückwärts.

d-ulwqwedw-filter-width = Breite

d-ulwqwedw-filter-height = Höhe

d-cb3fxuvg = Ton
    .doc = Ob das Gerät den Ton des Schnitts so gut wie sein Bild abspielt.

d-ckkce4lq = Schnitt-Anfrage
    .doc = Der Schnitt, wie er jetzt ist, für eine bereits laufende Wiedergabe: ein Schnitt wird gesehen, sobald er gemacht ist.
    .noun = { $count ->
        [one] Schnitt-Anfrage
       *[other] Schnitt-Anfragen
    }

d-6drnl2tz = Auftrag

d-7es5yzhp = Projekt

d-ssl4374c = Scrub-Anfrage
    .doc = Ein kurzes Stück des Tons des Schnitts dort, wohin der Abspielkopf gezogen wurde.
    .noun = { $count ->
        [one] Scrub-Anfrage
       *[other] Scrub-Anfragen
    }

d-52qgffcz = Projekt

d-2mvmk5yr = An

d-ccewf5ft = Sekunden

d-ssl4374c-filter-at = An

d-ssl4374c-filter-seconds = Sekunden

d-ik75ptfo = Anker setzen
    .doc = Verschiebt den Punkt, um den ein Clip skaliert und gedreht wird: eine Ecke, eine Kante oder die Mitte; dort ist ein Bild festgemacht, wenn es halb so groß gemacht, gedreht oder vergrößert wird.

d-ik75ptfo-x = X

d-ik75ptfo-y = Y

d-ghp2ji72 = Schnitt abspielen
    .doc = Spielt den Schnitt ab `at` ab: das Gerät setzt jedes Bild zusammen, sobald der Abspielkopf es erreicht, und mischt den Ton für denselben Moment, sodass das Fenster den Schnitt selbst zeigt statt eines Renders davon.

d-ghp2ji72-at = An

d-ghp2ji72-rate = Geschwindigkeit

d-tzdscmwx = Wiedergabe stoppen
    .doc = Stoppt die laufende Wiedergabe: das Gerät hört auf, zusammenzusetzen, und der Ton endet mit dem Bild.

d-7uqdfx3n = Wiedergabe anhalten
    .doc = Hält die laufende Wiedergabe dort an, wo sie steht, und zeichnet dieses Bild: das ist Pause.

d-jhutq7l5 = Shuttle
    .doc = Ein Druck auf ein Shuttle-Bedienelement: dieselbe Richtung verdoppelt das Tempo bis auf das Sechzehnfache, die andere Richtung beginnt wieder mit normalem Tempo in ihre Richtung.

d-jhutq7l5-at = An

d-jhutq7l5-way = Richtung

d-uawtpsaa = Springen nach
    .doc = Stoppt die laufende Wiedergabe und zeichnet das Bild `at` an ihrer Stelle: das tun ein Sprung und ein Bildschritt.

d-uawtpsaa-at = An

d-orlci7a7 = Live bei
    .doc = Wo die laufende Wiedergabe den Abspielkopf jetzt hat, oder -1, wenn nichts läuft.

d-4kh3dg3r = Scrubben bis
    .doc = Scrubbt zur Sekunde `at`: die laufende Wiedergabe stoppt, das Bild unter dem Abspielkopf wird gezeichnet, und ein kurzes Stück des Tons dort wird abgespielt.

d-4kh3dg3r-at = An

d-ensgtqhm = Wiedergabe läuft
    .doc = Ob die laufende Wiedergabe noch läuft: ein Wort aus dem Nichts heißt, sie fängt gerade an, und ein Wort an einem der beiden Enden heißt, sie ist dort stehen geblieben.

d-24wktakk = Live erneuern
    .doc = Reicht dem Gerät den Schnitt, wie er jetzt ist: ein Schnitt während der Wiedergabe wird gesehen, sobald er gemacht ist, statt erst bei der nächsten Wiedergabe.

d-es6mfprb = Scrub-Ton
    .doc = Der Ton des Schnitts dort, wohin der Abspielkopf gezogen wurde: ein kurzer Ausschnitt, nicht der laufende Ton der Zeitleiste.

d-es6mfprb-at = An

d-mtkg2brb = Bewegungsangabe
    .w-backwards = Rückwärts { $round }×
    .w-forwards = Vorwärts { $round }×
    .w-playing = Wiedergabe

d-zdvk3o64 = Anker X

d-j4zszdmt = Anker Y

d-twd6225m-filter-anchor_x = X

d-twd6225m-filter-anchor_y = Y

d-werb5t7p-filter-anchor_x = X

d-werb5t7p-filter-anchor_y = Y

d-fkdizcdd-filter-live_rate = Live-Geschwindigkeit

d-2iqind2t = Stellen
    .doc = Die Zeitpunkte auf der Zeitachse, an denen er über Vollaussteuerung ging, der Reihe nach.

d-33lufvui = Stellen
    .doc = Die Sekunden auf der Zeitachse, an denen der gemessene Mix über Vollaussteuerung ging.

d-5sdhdf7o = Heiße Spuren
    .doc = Die Spuren, die schon für sich über waren: was zuerst ins Auge fällt, denn der Mix ist die Summe aller.

d-3onhtj2t = Kurve gesamt
    .doc = Die Tonkurve eines Kanals: die x und y ihrer Punkte, je 0..1, aufsteigend in x.

d-ex66wj7a = Kurve gesamt
    .doc = Die Tonkurve eines Kanals, nach den drei Gradationsachsen angewandt: die x und y jedes Punktes, je 0..1, aufsteigend in x.

d-ol3bgzw4 = Kurve gesamt
    .doc = Die Tonkurve eines Kanals als x-und-y-Paare, je 0..1.

d-n5lm4fa3 = Kurve Rot

d-qahutbcr = Kurve Rot

d-p7y2saed = Kurve Rot

d-vl2qc7vg = Kurve Grün

d-zc6isz2b = Kurve Grün

d-gyh2nxn7 = Kurve Grün

d-ba2d3rjj = Kurve Blau

d-7ubzs7t5 = Kurve Blau

d-jf6z7vea = Kurve Blau

d-qai4utug = Kurvenanteil
    .doc = Wie viel von der Kurve einfließt: 0 umgeht sie, 1 ist die Kurve wie gezeichnet.

d-qjnl3pk6 = Kurvenanteil
    .doc = Wie viel von der Kurve einfließt: 0 umgeht sie, 1 ist die Kurve wie gezeichnet.

d-br257zpc = Kurvenanteil
    .doc = Wie viel von der Kurve einfließt: 0 umgeht sie, 1 ist die Kurve wie gezeichnet.

d-aj2coyre-filter-curve_mix = Kurvenanteil

d-twd6225m-filter-curve_mix = Kurvenanteil

d-werb5t7p-filter-curve_mix = Kurvenanteil

d-j34pf5ka = Schrift
    .doc = Die Schriftfamilie, in der die Worte eines Titels stehen; leer für das eigene Sans des Systems.

d-ehpks3pq = Schrift

d-xryucmpo = Übersteuerung, dB
    .doc = Wie weit über Vollaussteuerung dieses Fenster den Mix gemessen hat, in Dezibel; 0, wenn er nicht darüber ging.

d-y32rbhqo = Übersteuerung, dB
    .doc = Wie weit über Vollaussteuerung der Mix ging, in Dezibel; 0, wenn er nicht darüber ging.

d-czr6i2uz = Übersteuerung, dB

d-pm66fu4a = Stellen

d-fkdizcdd-filter-over_db = Übersteuerung, dB

d-o5kywdxh = Live-Wort
    .doc = Was eine laufende Wiedergabe von sich sagt: wo ihre Abspielmarke steht und was der bisher gehörte Mix gemessen hat.
    .noun = { $count ->
        [one] Live-Wort
       *[other] Live-Worte
    }

d-o5kywdxh-filter-at = Zeitpunkt

d-o5kywdxh-filter-over_db = Übersteuerung, dB

d-it7no3yy = Zeitpunkt

d-pejln2oj = Spuren
    .doc = Die Spuren, die schon für sich über waren.

d-hh44z4gm = Spuren

d-6gyd7wgk = Mix-Antwort
    .doc = Was das Gerät über den ganzen Mix eines Schnitts antwortet.
    .noun = { $count ->
        [one] Mix-Antwort
       *[other] Mix-Antworten
    }

d-6gyd7wgk-filter-over_db = Übersteuerung, dB

d-fiysox4k = Mix-Anfrage
    .doc = Ein Schnitt, dessen Mix gefragt ist.
    .noun = { $count ->
        [one] Mix-Anfrage
       *[other] Mix-Anfragen
    }

d-thpnmzhh = Projekt

d-rdaudor4 = Kurve setzen
    .doc = Setzt die Tonkurve eines Kanals aus den Punkten, die jemand geformt hat, und wendet sie sofort auf das Bild an.

d-rdaudor4-channel = Kanal

d-rdaudor4-points = Punkte

d-n2uvapqz = Kurve zurücksetzen
    .doc = Legt die Kurve eines Kanals wieder auf die gerade Linie: das Bild, wie die Gradation allein es lässt.

d-n2uvapqz-channel = Kanal

d-svcqgi2a = Kurvenanteil setzen
    .doc = Wie viel von der Kurve das Bild aufnimmt: 0 umgeht sie ganz, 1 ist die Kurve wie gezeichnet.

d-svcqgi2a-mix = Anteil

d-vocgd4cm = Bildfeld füllen
    .doc = Füllt das Bildfeld mit dem Bild, wie es kommt: jeder Zuschnitt, jede Skalierung, Drehung und Verschiebung zurückgenommen, sodass die Einpassung der Quelle so viel vom Bildfeld füllt, wie sie kann.
    .w-the-picture-already-fills-the-frame = Das Bild füllt das Bildfeld schon

d-pifueysw = An das Bild anpassen
    .doc = Legt die Maske des Clips über das Bild, wie das Bildfeld es zeigt: die Einpassung der Quelle, dann ihr Zuschnitt, ihre Skalierung, ihre Drehung und ihre Verschiebung, sodass die Maske das Bild ausschneidet statt ein Feld geschätzter Größe.

d-eyexjnvh = Schrift setzen
    .doc = Die Schriftfamilie, in der die Worte eines Titels stehen, neu gezeichnet, während sie sich ändert.

d-eyexjnvh-family = Familie

d-yijzfat3 = Mix leiser machen
    .doc = Senkt jede Spur um dieselbe Zahl von Dezibel und erhält dabei die Balance zwischen ihnen.
    .w-is-not-a-number-of-decibels = { $db } ist keine Zahl von Dezibel, um den Mix leiser zu machen

d-yijzfat3-db = Dezibel

d-slc3fisu = Mix prüfen
    .doc = Bittet dieses Gerät, den ganzen Mix des Schnitts zu messen, und merkt sich, wo er über Vollaussteuerung ging, damit Anzeige, Weg zu jeder Stelle und Leisermachen wissen, wovon sie sprechen.
    .w-the-device-s-measure-of-the = Die Messung des Mixes durch das Gerät ließ sich nicht lesen

d-qruse66d = Mix hören
    .doc = Nimmt auf, was eine laufende Wiedergabe bisher gehört hat: das Übersteuern des Mixes wird beim Abspielen gehört, und die Anzeige und ihr Weg dorthin stehen ab der ersten Stelle, an der er über war.

d-yxcs6mwu = Mix vergessen
    .doc = Vergisst eine Messung eines Mixes, der seither geändert wurde: was gemessen wurde, beschreibt eine Summe, die es nicht mehr gibt.

d-gs6wzbnh = Marke umbenennen
    .doc = Benennt eine Marke um.
    .w-a-marker-needs-a-name-to = Eine Marke braucht einen Namen, um gefunden zu werden

d-gs6wzbnh-words = Worte

d-cvul4ldi = Exportname
    .w-webm = webm
    .w-mov = mov
    .w-mp4 = mp4

d-hkdbseno = Schriftworte
    .w-system-sans = System-Sans

d-yhb723tk = Übersteuerungsworte
    .w-db = +{ $db } dB
    .w-db-2 = { $db } dB

d-h6ihb5b6 = Bildfeld füllen~present

d-uog5ccgf = Spuren leiser~present

d-i6lazdy2 = Marke umbenennen~present
