# Canvas auf Deutsch. Die Schlüssel sind die festen ids der Deklarationen (app.ids); die Wörter der Quelle stehen in den Quellen des Pakets.

d-so-tilo-canvas = Leinwand

d-so-tilo-canvas-keywords = draw, drawing, paint, sketch, whiteboard, art, illustration, canvas, vector, watercolour, oil, zeichnen, malen, skizze, zeichenbrett, leinwand, tafel

d-mwpf2yfh = Bild
    .doc = Ein Bild: Ebenen voller Markierungen, jede ein Vektorobjekt mit fester id, über einem Untergrund.
    .noun = { $count ->
        [one] Bild
       *[other] Bilder
    }
    .untitled = Ohne Titel

d-zp2ztcwq = Titel

d-izpefh4x = Breite

d-5ui5uh6r = Höhe

d-fh35sews = Untergrund-Textur
    .doc = `smooth`, `paper`, `cold-press`, `rough`, `linen` oder `panel`.

d-zdv3lz52 = Untergrundfarbe

d-xu4h6ew2 = Ebenen
    .doc = Von unten nach oben.

d-btwv4xq5 = Markierungen
    .doc = Jede Markierung jeder Ebene; eine Markierung nennt ihre Ebene.

d-4wmc4l7b = Bilddateien
    .doc = Bilder, die im Bild stehen, nach der Adresse ihrer Bytes.

d-ob6rlf7s = Auswahl
    .doc = Der Bereich, der für die nächste Bearbeitung ausgewählt ist, beim Bild behalten.

d-k2slfiuy = Vorschau
    .doc = Das Bild, wie es zuletzt gezeichnet wurde, damit die Fläche unter der Hand Farbe zeigt.

d-meooqdog = Hinweis
    .doc = Warum das zuletzt von der Engine Gefragte nicht geschah; leer, wenn es geschah.

d-erjm2yk6 = Ebene
    .doc = Eine Ebene des Bildes: ihre eigene Farbe und was sie durchlässt.
    .noun = { $count ->
        [one] Ebene
       *[other] Ebenen
    }

d-bplq2q5j = Name

d-ilxjjxxz = Medium
    .doc = `oil`, `watercolour` oder ein anderes im Bild gespeichertes Medium; leer für gewöhnliche Vektorfarbe.

d-smoxtxmk = Deckkraft

d-evne54yw = Sichtbar

d-nagaw3zw = Gesperrt

d-yzvcnjuy = Struktur
    .doc = Wie viel von der Rauheit des Untergrunds durch ihre Markierungen scheint, 0 bis 1.

d-erjm2yk6-filter-opacity = Deckkraft

d-erjm2yk6-filter-visible = Sichtbar

d-erjm2yk6-filter-locked = Gesperrt

d-erjm2yk6-filter-texture = Struktur

d-54j6cyar = Markierung
    .doc = Eine Markierung: ein Pinselstrich (`stroke`) oder eine Form (`path`), auf einer Ebene gezeichnet.
    .noun = { $count ->
        [one] Markierung
       *[other] Markierungen
    }

d-3mgqcu4c = Name

d-xeahkwbd = Ebene

d-pwxdvgoe = Art
    .doc = `stroke` oder `path`.

d-4eq3fpao = Punkte
    .doc = Die Punkte eines Strichs, `x, y, Stärke` der Reihe nach, in Einheiten des Bildes.

d-qxx46dqp = D
    .doc = Die SVG-Pfaddaten einer Form oder der Bereich, in dem sie liegt.

d-vmqyvhnc = Größe

d-fnl5j4dd = Farbe

d-omem6tkf = Pinsel
    .doc = `round`, `pen`, `pencil`, `marker`, `airbrush`, `charcoal`, `calligraphy`, `eraser`, `pixel` oder `pixel-eraser`; leer für den runden Tuschepinsel.

d-kytiqfia = Deckkraft

d-54j6cyar-filter-layer = Ebene

d-54j6cyar-filter-size = Größe

d-54j6cyar-filter-opacity = Deckkraft

d-73pl6fmd = Bereich
    .doc = Der Bereich, der für die nächste Bearbeitung ausgewählt ist.
    .noun = { $count ->
        [one] Bereich
       *[other] Bereiche
    }

d-k26xq4t7 = D
    .doc = SVG-Pfaddaten in Einheiten des Bildes.

d-enongrrr = Weichzeichnen
    .doc = Wie weit seine Kante ausläuft, in Einheiten.

d-73pl6fmd-filter-feather = Weichzeichnen

d-47oq4fqf = Bilddatei
    .doc = Ein Bild, das das Bild zeigt, nach der Adresse seiner Bytes behalten.
    .noun = { $count ->
        [one] Bilddatei
       *[other] Bilddateien
    }

d-di5h6qir = Name

d-4be6ojwm = Adresse

d-rp5ikqj7 = Breite

d-4r7lfx7t = Höhe

d-47oq4fqf-filter-width = Breite

d-47oq4fqf-filter-height = Höhe

d-wngt6wa6 = Bibliothek
    .doc = Die Bilder des Arbeitsbereichs.
    .noun = { $count ->
        [one] Bibliothek
       *[other] Bibliotheken
    }

d-5gxbqk6v = Bilder

d-crj47b4n = Anfrage zum Ansehen
    .doc = Was ein Ansehen von der Zeichen-Engine des Geräts will.
    .noun = { $count ->
        [one] Anfrage zum Ansehen
       *[other] Anfragen zum Ansehen
    }

d-hhu6bz5c = Größe

d-ubid4c7s = Format

d-crj47b4n-filter-size = Größe

d-b3ilsucz = Anfrage zum Exportieren
    .doc = Was ein Export von der Zeichen-Engine des Geräts will.
    .noun = { $count ->
        [one] Anfrage zum Exportieren
       *[other] Anfragen zum Exportieren
    }

d-ottkj6jd = Format

d-5mutx32x = Maßstab

d-b3ilsucz-filter-scale = Maßstab

d-6w6ch7ag = Anfrage zum SVG-Export
    .doc = Was ein SVG-Export will: das ganze Bild, seinen Text als Text.
    .noun = { $count ->
        [one] Anfrage zum SVG-Export
       *[other] Anfragen zum SVG-Export
    }

d-c35hmrf6 = Ids

d-gqfdwswn = Text als Umriss

d-6w6ch7ag-filter-outline_text = Text als Umriss

d-2kyhrm65 = Anfrage zum Platzieren
    .doc = Wohin ein importiertes SVG geht: die oberste Ebene, in einer eigenen Gruppe.
    .noun = { $count ->
        [one] Anfrage zum Platzieren
       *[other] Anfragen zum Platzieren
    }

d-2w4n3dr2 = Ebene

d-zxu35ztu = Gruppe

d-ulngw3pf = Einpassen

d-ywazcrrf = Bild öffnen
    .doc = Öffnet ein gespeichertes Bild aus der Datei, die eine Person, Dateien oder eine andere App übergibt.
    .did = hat ein Bild geöffnet

d-ywazcrrf-file = Datei

d-6b4cocc6 = Leer beginnen
    .doc = Ein leeres Bild, begonnen mit einem Problem zum Zeigen, damit eine Datei, die sich nicht öffnen lässt, dort landet, wo eine Person lesen kann, warum.

d-6b4cocc6-why = Grund

d-i2shd26j = Neues Bild
    .doc = Beginnt ein leeres Bild mit einer einfachen Ebene.
    .did = hat ein neues Bild begonnen
    .w-an-artwork-is-16-to-8192 = Ein Bild ist 16 bis 8192 Einheiten breit
    .w-an-artwork-is-16-to-8192-2 = Ein Bild ist 16 bis 8192 Einheiten hoch

d-i2shd26j-title = Titel

d-i2shd26j-width = Breite

d-i2shd26j-height = Höhe

d-im6e3ij3 = Ebene hinzufügen
    .doc = Fügt oben im Bild eine Ebene hinzu.
    .did = hat eine Ebene hinzugefügt
    .w-a-layer-named-is-already-here = Eine Ebene namens { $trim } gibt es schon

d-im6e3ij3-name = Name

d-25k3qffg = Ebene umbenennen
    .doc = Benennt eine Ebene um.
    .did = hat eine Ebene umbenannt
    .w-a-layer-has-a-name = Eine Ebene hat einen Namen
    .w-another-layer-is-named = Eine andere Ebene heißt { $trim }

d-25k3qffg-layer = Ebene

d-25k3qffg-name = Name

d-26tef7a6 = Ebene zeigen oder verbergen
    .doc = Zeigt eine Ebene oder verbirgt sie.
    .did = hat eine Ebene verborgen oder gezeigt

d-26tef7a6-layer = Ebene

d-26tef7a6-visible = Sichtbar

d-g6eyi6x5 = Ebene entfernen

d-g6eyi6x5-layer = Ebene

d-ws4qckds = Bild festlegen
    .doc = Legt fest, wie das Bild heißt und wie groß es ist.
    .did = hat das Bild geändert

d-ws4qckds-title = Titel

d-ws4qckds-width = Breite

d-ws4qckds-height = Höhe

d-nmal4fcc = Malen
    .doc = Legt einen Pinselstrich dort, wo die Hand entlangging, seinen Punkt `[x, y, Stärke]` in Einheiten des Bildes.
    .did = hat einen Strich gemalt
    .w-a-stroke-point-is-x-and = Ein Strichpunkt ist x und y
    .w-the-layer-is-locked = Die Ebene ist gesperrt

d-nmal4fcc-layer = Ebene

d-nmal4fcc-point = Punkt

d-nmal4fcc-size = Größe

d-nmal4fcc-colour = Farbe

d-nmal4fcc-brush = Pinsel

d-kqbnpkk2 = Malen
    .doc = Fügt dem Strich, der gerade gemalt wird, den nächsten Punkt hinzu.
    .did = hat einen Strich gemalt

d-kqbnpkk2-m = M

d-kqbnpkk2-point = Punkt

d-yudcrjy3 = Oberste Ebene
    .doc = Die Ebene, auf der ein Strich landet, wenn keine gewählt ist: die oberste, die zu sehen ist.

d-kzqsfl7r = Form zeichnen
    .doc = Zeichnet eine Form, ihre SVG-Pfaddaten in Einheiten des Bildes.
    .did = hat eine Form gezeichnet
    .w-a-shape-has-an-outline = Eine Form hat einen Umriss
    .w-the-layer-is-locked = Die Ebene ist gesperrt

d-kzqsfl7r-layer = Ebene

d-kzqsfl7r-d = D

d-kzqsfl7r-colour = Farbe

d-yl33wzft = Markierung entfernen

d-yl33wzft-m = M

d-nv63qcbm = Farbe einer Markierung ändern
    .doc = Gibt einer Markierung eine neue Farbe.
    .did = hat eine Markierung neu gefärbt

d-nv63qcbm-m = M

d-nv63qcbm-colour = Farbe

d-srya3bgt = Markierung groß machen
    .doc = Macht eine Markierung größer oder kleiner.
    .did = hat eine Markierung in der Größe geändert
    .w-a-mark-s-brush-is-wider = Der Pinsel einer Markierung ist breiter als nichts

d-srya3bgt-m = M

d-srya3bgt-size = Größe

d-5zqnfec7 = Bereich auswählen
    .doc = Wählt einen Bereich aus, seinen Umriss als SVG-Pfaddaten.
    .did = hat einen Bereich ausgewählt
    .w-a-selection-has-an-outline = Eine Auswahl hat einen Umriss
    .w-a-feather-is-0-to-500 = Ein Weichzeichnen ist 0 bis 500 Einheiten

d-5zqnfec7-d = D

d-5zqnfec7-feather = Weichzeichnen

d-im6ajcey = Auswahl aufheben
    .doc = Lässt die Auswahl los.

d-x4bmanto = Auswahl füllen
    .doc = Füllt die Auswahl als Pfadmarkierung auf einer Ebene.
    .did = hat die Auswahl gefüllt

d-x4bmanto-layer = Ebene

d-x4bmanto-colour = Farbe

d-trlv6xc5 = Bild ansehen
    .doc = Das Bild, wie es dasteht, beim Bild behalten, damit die Fläche es unter der Hand zeigt.

d-trlv6xc5-size = Größe

d-t4ze7q3o = Bild exportieren
    .doc = Schreibt das Bild in voller Größe als PNG-, JPEG- oder WebP-Bild.
    .did = hat ein Bild exportiert

d-t4ze7q3o-format = Format

d-t4ze7q3o-scale = Maßstab

d-zjw2al2m = SVG exportieren
    .doc = Schreibt das Bild als SVG-Dokument.
    .did = hat ein SVG exportiert

d-pnm5r4tq = Bild speichern
    .doc = Speichert das Bild als `.artwork`-Paket mit seinen Bilddateien.
    .did = hat das Bild gespeichert

d-f6v2tn6o = SVG importieren
    .doc = Importiert ein SVG-Dokument als Markierungen auf einer Ebene.
    .did = hat ein SVG importiert

d-f6v2tn6o-markup = SVG-Text

d-qnk3hupn = Hinweis schließen
    .doc = Schließt den Hinweis, den eine Person gelesen hat.

d-ocgibcwg = Bibliothek
    .w-no-artwork-yet = Noch kein Bild
    .w-new-artwork-starts-one = Neues Bild beginnt eines
    .w-new-artwork = Neues Bild
    .w-open-an-artwork = Bild öffnen…

d-sqei2kjp = Neues Bild~present

d-lb23uu53 = Ebene umbenennen~present

d-fbgqcv4q = Strich hinzufügen~present

d-a3gxy4s7 = Form hinzufügen~present

d-cfa5perl = Markierung skalieren~present

d-y2raac27 = Bereich auswählen~present

d-vbu6ayw5 = Bild
    .w-layers = Ebenen
    .w-add-layer = Ebene hinzufügen
    .w-size = Größe
    .w-paint = Malen
    .w-hide-the-layer = Ebene ausblenden
    .w-show-the-layer = Ebene einblenden
    .w-layer-actions = Ebenenaktionen
    .w-opacity = Deckkraft
    .w-up = Nach oben
    .w-down = Nach unten
    .w-brush = Pinsel
    .w-fill-the-selection = Auswahl füllen
    .w-deselect = Auswahl aufheben
    .w-export = Exportieren
    .w-png-full-size = PNG, volle Größe
    .w-png-twice-the-size = PNG, doppelte Größe
    .w-jpeg-full-size = JPEG, volle Größe
    .w-webp-full-size = WebP, volle Größe
    .w-svg = SVG
    .w-fit-the-view = Ansicht einpassen
    .w-eraser = Radierer
    .w-rectangle = Rechteck
    .w-ellipse = Ellipse
    .w-line = Linie
    .w-select = Auswählen
    .w-zoom-out = Verkleinern
    .w-zoom-in = Vergrößern
    .w-fit = Einpassen
    .w-new-artwork = Neues Kunstwerk
    .w-place-a-picture = Ein Bild platzieren
    .w-look = Erscheinungsbild
    .w-save = Speichern
    .w-brush-2 = Pinsel: { $brush }
    .w-wand = Zauberstab
    .w-text = Text
    .w-set-the-reference = Die Referenz setzen
    .w-lasso = Lasso
    .w-medium = Medium: { $medium_said }
    .w-plain-paint = Einfache Farbe
    .w-oil = Öl
    .w-watercolour = Aquarell
    .w-combine = Kombinieren
    .w-unite = Vereinigen
    .w-subtract = Abziehen
    .w-intersect = Schneiden
    .w-exclude = Ausschließen
    .w-measure = Messen
    .w-pick = Wählen
    .w-picked = { $len } gewählt
    .w-let-the-picks-go = Die gewählten freigeben
    .w-brush-dynamics = Pinseldynamik
    .w-flow = Fluss
    .w-hardness = Härte
    .w-spacing = Abstand
    .w-smoothing = Glättung
    .w-taper = Verjüngung
    .w-saved-media = Gespeicherte Medien
    .w-remove = { $label } entfernen
    .w-pigment = Pigment
    .w-gradient = Verlauf
    .w-linear-into-the-paint = Linear, in die Farbe
    .w-radial-from-the-centre = Radial, aus der Mitte
    .w-clear-the-gradient = Den Verlauf löschen
    .w-reference = Referenz
    .w-side-by-side = Nebeneinander
    .w-overlay = Überlagert
    .w-difference = Differenz
    .w-clear-the-reference = Die Referenz löschen
    .w-the-artwork-against-its-reference = Das Bild gegen seine Referenz

d-y5teghkv = Offen
    .doc = Ob die Hand noch auf ihm liegt: der Strich, den ein bewegter Zeiger verlängert.

d-54j6cyar-filter-open = Offen


d-j3bzgr3f = Titel

d-dowdfhi2 = Breite

d-vlrgzghe = Höhe

d-lotrow4q = Untergrundtextur

d-mpyvrw7z = Untergrundfarbe

d-xubd3xon = Ebenen

d-lowvdtkg = Markierungen

d-5h6vjvk2 = Auswahl

d-xocfgdc7-filter-width = Breite

d-xocfgdc7-filter-height = Höhe

d-xocfgdc7-filter-selection = Auswahl


d-xunw6qmv = Name

d-sxka2uxh = Medium

d-aka7ql3b = Deckkraft

d-4h7cxhog = Sichtbar

d-2r4nberb = Gesperrt

d-xlvw3rkz = Textur

d-tsgjljmf-filter-opacity = Deckkraft

d-tsgjljmf-filter-visible = Sichtbar

d-tsgjljmf-filter-locked = Gesperrt

d-tsgjljmf-filter-texture = Textur


d-67wvmkkq = Name

d-jk7lnwfw = Ebene

d-xy2goji2 = Art

d-ysn6onwt = Punkte

d-vmvvarxw = D

d-hajhmux3 = Größe

d-gtn2xdwh = Farbe

d-pqrpx7k3 = Pinsel

d-kolk77nz = Deckkraft

d-uslws4n3-filter-layer = Ebene

d-uslws4n3-filter-size = Größe

d-uslws4n3-filter-opacity = Deckkraft


d-nxcluiyf = D

d-jxpwvxya = Weichzeichnen

d-3pgxznsb-filter-feather = Weichzeichnen

d-dx7xxp6w = Ebene sperren
    .doc = Hält eine Ebene still, sodass der Zeiger durch ihre Markierungen hindurchgeht, oder gibt ihre Markierungen wieder frei.
    .did = sperrte eine Ebene

d-dx7xxp6w-layer = Ebene

d-dx7xxp6w-locked = Gesperrt

d-nsot2laz = Ebene nach oben
    .doc = Verschiebt eine Ebene im Stapel eine Stelle nach oben.
    .did = verschob eine Ebene

d-nsot2laz-layer = Ebene

d-36qh4o3t = Ebene nach unten
    .doc = Verschiebt eine Ebene im Stapel eine Stelle nach unten.
    .did = verschob eine Ebene

d-36qh4o3t-layer = Ebene

d-hgluvpdm = Ebene duplizieren
    .doc = Kopiert eine Ebene und alles auf ihr, oberhalb des Originals.
    .did = duplizierte eine Ebene

d-hgluvpdm-layer = Ebene

d-cpguer7m = Ebene nach unten zusammenführen
    .doc = Faltet eine Ebene in die darunter: ihre Markierungen gehen in jene über, und sie selbst ist fort.
    .did = führte eine Ebene zusammen

d-cpguer7m-layer = Ebene

d-dwmbuma7 = Malen beginnen
    .doc = Beginnt, was die Hand an `point` hinlegt: einen Strich mit der geladenen Farbe, oder nichts, wenn das Werkzeug eine Form zeichnet, deren Umriss erst entsteht, wenn die Hand sich hebt.

d-dwmbuma7-tool = Werkzeug

d-dwmbuma7-brush = Pinsel

d-dwmbuma7-layer = Ebene

d-dwmbuma7-point = Punkt

d-dwmbuma7-size = Größe

d-dwmbuma7-colour = Farbe

d-fbdnxnze = Malen fortsetzen
    .doc = Nimmt den nächsten Punkt des Strichs, der gerade gemalt wird.

d-fbdnxnze-tool = Werkzeug

d-fbdnxnze-brush = Pinsel

d-fbdnxnze-point = Punkt

d-fbdnxnze-size = Größe

d-fbdnxnze-colour = Farbe

d-3nneq3xo = Malen beenden
    .doc = Beendet, was die Hand an `point` hinlegte: der Strich nimmt seinen letzten Punkt und schließt, oder eine Form wird über den Zug gezeichnet, der sie machte.

d-3nneq3xo-tool = Werkzeug

d-3nneq3xo-brush = Pinsel

d-3nneq3xo-layer = Ebene

d-3nneq3xo-from = Von

d-3nneq3xo-point = Punkt

d-3nneq3xo-size = Größe

d-3nneq3xo-colour = Farbe

d-qjgxagpq = Ebene hinzufügen~present

d-xocfgdc7 = Bild-JSON
    .doc = Das Bild, wie die Zeichen-Engine des Geräts es liest: seine Felder und jede Markierung mit der Ebene, auf der sie steht.
    .noun = { $count ->
        [one] Bild-JSON
       *[other] Bild-JSONs
    }

d-tsgjljmf = Ebenen-JSON
    .noun = { $count ->
        [one] Ebenen-JSON
       *[other] Ebenen-JSONs
    }

d-uslws4n3 = Markierungs-JSON
    .noun = { $count ->
        [one] Markierungs-JSON
       *[other] Markierungs-JSONs
    }

d-3pgxznsb = Bereichs-JSON
    .noun = { $count ->
        [one] Bereichs-JSON
       *[other] Bereichs-JSONs
    }

d-cdvrgjan = Quelle
    .doc = Die Bytes einer Bilddatei, nach der Adresse, unter der sie liegen, und der Rahmen, in dem sie gezeichnet wird.

d-vniyw6t3 = X

d-v3wlgndz = Y

d-4nv5sha3 = Breite

d-hlvljkif = Höhe

d-54j6cyar-filter-x = X

d-54j6cyar-filter-y = Y

d-54j6cyar-filter-width = Breite

d-54j6cyar-filter-height = Höhe

d-7hgp7nbk = Quelle

d-vtafbavc = X

d-el3izuuy = Y

d-jzpg25ot = Breite

d-k7jjvary = Höhe

d-uslws4n3-filter-x = X

d-uslws4n3-filter-y = Y

d-uslws4n3-filter-width = Breite

d-uslws4n3-filter-height = Höhe

d-ynku6xaf = Bild platzieren
    .doc = Platziert eine Bilddatei, die die Person gewählt hat, auf der obersten Ebene, in das Bild eingepasst und zentriert: die Bilddatei bleibt ihr eigenes Objekt, sie bewegt, schneidet und synchronisiert sich also wie Farbe.
    .did = platzierte ein Bild
    .w-a-picture-is-placed-from-a = Ein Bild wird aus einer Datei platziert

d-ynku6xaf-file = Datei

d-od5n7ehr = Bilddatei platzieren~present
d-4bht3tzh = Medien
    .doc = Die im Bild gespeicherten Medien-Voreinstellungen, die seine Ebenen nennen können.

d-hrqejear = Ausgewählt
    .doc = Die Markierungen, die eine Person ausgewählt hat, auf die Kombinieren und Messen wirken.

d-exp6h3eo = Referenz
    .doc = Die Bilddatei, von der aus gearbeitet wird, gegen die `compare_reference` das Bild zeigt; sie gehört nicht zum Werk und wird nie exportiert.

d-mksntvfu = Vergleich
    .doc = Das Bild neben, über oder gegen seine Referenz, wie zuletzt verglichen, zum Ansehen behalten.

d-jmoqryks = Versionsnotiz
    .doc = Was der Vergleich zweier Versionen ergab, in einer Zeile, die eine Person liest.

d-fydl4rsk = Versionsüberlagerung
    .doc = Der geänderte Bereich der verglichenen Version, wie das Bild jetzt dasteht, über die Fläche gelegt, wo er steht.

d-i5yildir = Versionsbereich
    .doc = Dieser Bereich, `[x, y, Breite, Höhe]` in Einheiten des Bildes.

d-cwenklqw = Gruppe
    .doc = Der Ordner, in dem die Ebene steht; leer für eine Ebene ohne Ordner.

d-rxz73i7f = Maske für
    .doc = Der Name der Ebene, die diese maskiert: ihre Markierungen werden in Grautönen gezeichnet, Hell zeigt die Ebene und Dunkel verbirgt sie.

d-umkqodij = Basis
    .doc = Das Grau einer Maske vor jeder Markierung, 0 bis 1.

d-owktdb7d = Invertieren

d-n43pvuyc = Beschneiden
    .doc = Auf die Ebene darunter beschnitten: sie zeigt nur, wo jene Farbe hat.

d-2rtbmdqf = Unverknüpft
    .doc = Eine Maske, die stehen bleibt, wenn sich die Markierungen ihrer Ebene bewegen.

d-zrxehlis = Belichtung
    .doc = Farb- und Tonanpassungen von allem auf der Ebene, nach denen jeder Markierung.

d-erzbebsl = Kontrast

d-qcpp23hg = Lichter

d-xr5c7gly = Tiefen

d-cxp7piot = Sättigung

d-frr4f7qk = Dynamik

d-7iz5iokq = Farbton

d-lqesc6ol = Farbtemperatur

d-4hpqnjer = Farbstich

d-2zsbapjy = Graustufen

d-giajnlvh = Invertieren

d-76aq4pc3 = Unschärfe

d-nraansa6 = Schärfen

d-yxn6dvna = Vignette

d-mvgiivbx = Tonwertkurve
    .doc = Tonwertkurve `[[in, out], …]`, 0 bis 1.

d-erjm2yk6-filter-base = Basis

d-erjm2yk6-filter-invert = Invertieren

d-erjm2yk6-filter-clip = Beschneiden

d-erjm2yk6-filter-unlinked = Unverknüpft

d-erjm2yk6-filter-exposure = Belichtung

d-erjm2yk6-filter-contrast = Kontrast

d-erjm2yk6-filter-highlights = Lichter

d-erjm2yk6-filter-shadows = Tiefen

d-erjm2yk6-filter-saturation = Sättigung

d-erjm2yk6-filter-vibrance = Dynamik

d-erjm2yk6-filter-hue = Farbton

d-erjm2yk6-filter-temperature = Farbtemperatur

d-erjm2yk6-filter-tint = Farbstich

d-erjm2yk6-filter-adjust_grayscale = Graustufen

d-erjm2yk6-filter-adjust_invert = Invertieren

d-erjm2yk6-filter-blur = Unschärfe

d-erjm2yk6-filter-sharpen = Schärfen

d-erjm2yk6-filter-vignette = Vignette
d-hyumfmpg = Gruppe
    .doc = Der Ordner, in dem die Markierung steht; leer ohne Ordner.

d-lu2gjrfu = Nass
    .doc = Auf einer gemalten Ebene: in das noch Nasse malen statt über trockene Farbe.

d-s2tjjp2p = Parameter
    .doc = Auf einer gemalten Ebene: die für diese Markierung überschriebenen Parameter des Mediums.

d-o4zghm7q = Mischmodus
    .doc = Wie sich die Markierung mit dem darunter mischt: leer oder `normal`, `multiply`, `screen`, `overlay`, `darken`, `lighten`, `color-dodge`, `color-burn`, `hard-light`, `soft-light`, `difference` oder `exclusion`.

d-wpv66rxy = Ausschnitt x
    .doc = Der Rahmen der Bilddatei in ihren eigenen Pixeln, als Anteile davon: standardmäßig alles.

d-sz6fzruq = Ausschnitt y

d-ax7jjgo5 = Ausschnittsbreite

d-7jtq2b7o = Ausschnittshöhe

d-mzyxo2mo = Begradigen
    .doc = Eine Drehung innerhalb des Rahmens der Bilddatei, in Grad.

d-xndatwwg = Waagerecht spiegeln

d-vyaxfcb2 = Senkrecht spiegeln

d-37ailkzg = Belichtung
    .doc = Farb- und Tonanpassungen, angewandt, wenn das Bild gezeigt und exportiert wird; null (oder aus, bei den Schaltern) lässt einen Schritt aus.

d-g3ifa5op = Kontrast

d-3taebyjo = Lichter

d-5d6dfhop = Tiefen

d-nebzyt3a = Sättigung

d-6ib7oqy4 = Dynamik

d-vzagvyr3 = Farbton

d-bdxzjdby = Farbtemperatur

d-skvl4xui = Farbstich

d-3tuoc3tu = Graustufen

d-tr7kuqc5 = Invertieren

d-qe7z2w5c = Unschärfe

d-h6rqzqu5 = Schärfen

d-glowmwgx = Vignette

d-jzqxuy4d = Farbverlauf
    .doc = Eine Verlaufsfüllung statt der einfachen Farbe: `linear` oder `radial`, ihre zwei Enden und ihre Stufen.

d-2afylrq3 = Verlauf x0

d-skgor2yj = Verlauf y0

d-ctpi3sbl = Verlauf x1

d-nosksv3s = Verlauf y1

d-n4zfbtzn = Verlaufsstufen

d-7vai53br = Verlaufsfarben

d-63yhsfnf = Text
    .doc = Die Worte einer Textmarkierung, ihre Schrift und ihr Satz; `kind` ist für sie `text`.

d-xbmipp4j = Schrift

d-ymnuknae = Schriftschnitt

d-u62kvwpy = Kursiv

d-mj3rz5s2 = Ausrichtung

d-i7zjtca3 = Zeilenhöhe

d-sc445r7q = Laufweite

d-e7l24fze = Textpfad

d-5lpu3cr5 = Transformation
    .doc = Eine affine Transformation `[a, b, c, d, e, f]` auf die Geometrie der Markierung: wie sie bewegt, skaliert und gedreht wird.

d-7kaylfaa = Beschneidung
    .doc = Die Markierung oder Gruppe, auf deren Form diese Markierung beschnitten ist; leer für unbeschnitten.

d-adangnyz = Verpixelt
    .doc = Bildpixel mit Nächster-Nachbar-Abtastung vergrößern.

d-p45w3ec2 = Körnung
    .doc = Filmkorn über der Markierung, 0 bis 1.

d-gdv4nrn7 = Weichzeichnen
    .doc = Die Kante der Markierung nach außen über so viele Einheiten auslaufen lassen.
d-dvxs4mkf = Schatten x
    .doc = Ein Schatten, der außerhalb der Markierung fällt, und einer, der in sie fällt.

d-ebyye73f = Schatten y

d-lpv4a2ao = Schattenunschärfe

d-j3x5co4v = Schattenfarbe

d-4d36wjcg = Innerer Schatten x

d-aukevlpf = Innerer Schatten y

d-wz2pco6l = Unschärfe des inneren Schattens

d-3hhduagr = Farbe des inneren Schattens

d-66bccfgi = Tonwertkurve
    .doc = Tonwertkurve `[[in, out], …]`, 0 bis 1.

d-54j6cyar-filter-wet = Nass

d-54j6cyar-filter-crop_x = Ausschnitt x

d-54j6cyar-filter-crop_y = Ausschnitt y

d-54j6cyar-filter-crop_width = Ausschnittsbreite

d-54j6cyar-filter-crop_height = Ausschnittshöhe

d-54j6cyar-filter-straighten = Begradigen

d-54j6cyar-filter-flip_h = Waagerecht spiegeln

d-54j6cyar-filter-flip_v = Senkrecht spiegeln

d-54j6cyar-filter-exposure = Belichtung

d-54j6cyar-filter-contrast = Kontrast

d-54j6cyar-filter-highlights = Lichter

d-54j6cyar-filter-shadows = Tiefen

d-54j6cyar-filter-saturation = Sättigung

d-54j6cyar-filter-vibrance = Dynamik

d-54j6cyar-filter-hue = Farbton

d-54j6cyar-filter-temperature = Farbtemperatur

d-54j6cyar-filter-tint = Farbstich

d-54j6cyar-filter-grayscale = Graustufen

d-54j6cyar-filter-invert = Invertieren

d-54j6cyar-filter-blur = Unschärfe

d-54j6cyar-filter-sharpen = Schärfen

d-54j6cyar-filter-vignette = Vignette

d-54j6cyar-filter-gradient_x0 = Verlauf x0

d-54j6cyar-filter-gradient_y0 = Verlauf y0

d-54j6cyar-filter-gradient_x1 = Verlauf x1

d-54j6cyar-filter-gradient_y1 = Verlauf y1

d-54j6cyar-filter-weight = Schriftschnitt

d-54j6cyar-filter-italic = Kursiv

d-54j6cyar-filter-line_height = Zeilenhöhe

d-54j6cyar-filter-tracking = Laufweite

d-54j6cyar-filter-pixelated = Verpixelt

d-54j6cyar-filter-grain = Körnung

d-54j6cyar-filter-feather = Weichzeichnen

d-54j6cyar-filter-shadow_x = Schatten x

d-54j6cyar-filter-shadow_y = Schatten y

d-54j6cyar-filter-shadow_blur = Schattenunschärfe

d-54j6cyar-filter-inner_shadow_x = Innerer Schatten x

d-54j6cyar-filter-inner_shadow_y = Innerer Schatten y

d-54j6cyar-filter-inner_shadow_blur = Unschärfe des inneren Schattens

d-j7jomf5b = Weichzeichnungsbild
    .doc = Die weiche Kante, die eine verfeinerte Auswahl behält: eine Bilddatei, weiß, wo ausgewählt, und ihr Rahmen.
d-oeihhduj = Matte x

d-iqzt5i5r = Matte y

d-l6zpym3q = Mattenbreite

d-to7qczkv = Mattenhöhe

d-73pl6fmd-filter-matte_x = Matte x

d-73pl6fmd-filter-matte_y = Matte y

d-73pl6fmd-filter-matte_width = Mattenbreite

d-73pl6fmd-filter-matte_height = Mattenhöhe

d-hxu4t2k5 = Medium
    .doc = Ein im Bild gespeichertes Medium: eine Voreinstellung für Öl oder Aquarell, mit der Ebenen gemalt werden können.
    .noun = { $count ->
        [one] Medium
       *[other] Medien
    }

d-bevph2qk = Schlüssel

d-cgytyo4j = Bezeichnung

d-2opuzdzr = Engine
    .doc = `oil` oder `watercolour`.

d-mgi6schv = Beschreibung

d-6r6opsjz = Untergrund
    .doc = Der Untergrund, zu dem es passt; leer für jeden.

d-hids323q = Parameter

d-rdqoi6b4 = Palette
    .doc = Die Pigmente, die es trägt, geladen als `pigment:<name>`.

d-hxh5n7pe = Pigment
    .doc = Ein Pigment, das ein Medium trägt.
    .noun = { $count ->
        [one] Pigment
       *[other] Pigmente
    }

d-ph4qfvlv = Name
    .doc = Kleingeschriebene Wörter mit `-` verbunden, etwa `burnt-sienna`.

d-6heumgwe = Farbe
    .doc = Der Massenton, `#rrggbb`.

d-tm7hljog = Granulation
    .doc = Aquarell: wie stark es sich in die Rauheit des Papiers setzt, 0 bis 1.

d-t2xyhkxn = Stärke
    .doc = Wie stark es tönt, um 1.

d-hxh5n7pe-filter-granulation = Granulation

d-hxh5n7pe-filter-strength = Stärke

d-yfajq2fi = Referenz
    .doc = Eine Bilddatei, von der aus gearbeitet wird, nach der Adresse ihrer Bytes behalten: das Bild wird gegen sie verglichen.
    .noun = { $count ->
        [one] Referenz
       *[other] Referenzen
    }

d-3ze3qk7e = Bilddatei

d-ky6t7zk4 = X

d-k5uohe26 = Y

d-vyzji3vv = Breite

d-peuuxvxb = Höhe

d-decytxpt = Deckkraft

d-j3jxn7s3 = Sichtbar

d-yfajq2fi-filter-x = X

d-yfajq2fi-filter-y = Y

d-yfajq2fi-filter-width = Breite

d-yfajq2fi-filter-height = Höhe

d-yfajq2fi-filter-opacity = Deckkraft

d-yfajq2fi-filter-visible = Sichtbar

d-hoix2o4s = Medien

d-n77pojfm = Referenz

d-xocfgdc7-filter-reference = Referenz

d-km2u7cyx = Gruppe

d-qhmnxju4 = Maske für

d-6oqanygz = Basis
d-y6iyulut = Invertieren

d-k2r2z54p = Beschneiden

d-ppv32nn6 = Unverknüpft

d-z56o2ag2 = Belichtung

d-ui3khk6w = Kontrast

d-yynr6bsh = Lichter

d-3kml5egf = Tiefen

d-5c4xwrcg = Sättigung

d-gi73p5cu = Dynamik

d-7r6f5gkz = Farbton

d-tfdtaklm = Farbtemperatur

d-oanzkyda = Farbstich

d-aj3mmig4 = Graustufen

d-2cktvie6 = Anpassung invertieren

d-zkir2g3c = Unschärfe

d-frk7zwuc = Schärfen

d-pftddo22 = Vignette

d-eenyv7e2 = Tonwertkurve

d-tsgjljmf-filter-base = Basis

d-tsgjljmf-filter-invert = Invertieren

d-tsgjljmf-filter-clip = Beschneiden

d-tsgjljmf-filter-unlinked = Unverknüpft

d-tsgjljmf-filter-exposure = Belichtung

d-tsgjljmf-filter-contrast = Kontrast

d-tsgjljmf-filter-highlights = Lichter

d-tsgjljmf-filter-shadows = Tiefen

d-tsgjljmf-filter-saturation = Sättigung

d-tsgjljmf-filter-vibrance = Dynamik

d-tsgjljmf-filter-hue = Farbton

d-tsgjljmf-filter-temperature = Farbtemperatur

d-tsgjljmf-filter-tint = Farbstich

d-tsgjljmf-filter-grayscale = Graustufen

d-tsgjljmf-filter-invert_adjust = Anpassung invertieren

d-tsgjljmf-filter-blur = Unschärfe

d-tsgjljmf-filter-sharpen = Schärfen

d-tsgjljmf-filter-vignette = Vignette

d-xow6iril = Medium-JSON
    .noun = { $count ->
        [one] Medium-JSON
       *[other] Medium-JSONs
    }

d-iztu5hbq = Id

d-shen6puc = Bezeichnung

d-pcwg6twm = Engine

d-tucyloxp = Beschreibung

d-3g5vtp7l = Untergrund

d-athamtu3 = Parameter

d-pq2ipocw = Palette

d-6dypgs2o = Pigment-JSON
    .noun = { $count ->
        [one] Pigment-JSON
       *[other] Pigment-JSONs
    }

d-dgwlsywk = Name

d-b74p3dso = Farbe
d-rfus746j = Granulation

d-veeu7fpc = Stärke

d-6dypgs2o-filter-granulation = Granulation

d-6dypgs2o-filter-strength = Stärke

d-4d5hhlsw = Referenz-JSON
    .noun = { $count ->
        [one] Referenz-JSON
       *[other] Referenz-JSONs
    }

d-go76fdwt = Bilddatei

d-ax3jntcc = Rahmen

d-urv4gnep = Deckkraft

d-zbqd3iad = Sichtbar

d-4d5hhlsw-filter-opacity = Deckkraft

d-4d5hhlsw-filter-visible = Sichtbar

d-lrfhvryb = Gruppe

d-f2yfdpsw = Mischmodus

d-gbswtfkm = Ausschnitt x

d-cnv33zk4 = Ausschnitt y

d-yzjgjp5q = Ausschnittsbreite

d-d2tduo5n = Ausschnittshöhe

d-j7tormbq = Begradigen

d-vmes3q7g = Waagerecht spiegeln

d-4tv2wnsi = Senkrecht spiegeln

d-tnsniv2d = Belichtung

d-xsuhepir = Kontrast

d-rhmkylpp = Lichter

d-qqdl7hij = Tiefen

d-asuq3lgp = Sättigung

d-fxjyqy3l = Dynamik

d-rf2rek22 = Farbton

d-oqo6d26l = Farbtemperatur

d-suw3mw5t = Farbstich

d-prqekzin = Graustufen

d-ulnewmko = Invertieren

d-hhwbutye = Unschärfe

d-aooc5sn5 = Schärfen

d-y32qfhyc = Vignette

d-r77f22g6 = Verlaufsart

d-nuba72na = Verlauf x0

d-yvaatile = Verlauf y0

d-jdbhzols = Verlauf x1

d-tv6gyxgx = Verlauf y1

d-ih74f5za = Verlaufsstufen

d-zvcp7y2l = Verlaufsfarben

d-xub3tsun = Text

d-fkp53ei3 = Schrift

d-7nusoc27 = Schriftschnitt

d-n3izluzh = Kursiv

d-5dk4ceey = Ausrichtung

d-gnfltu6g = Zeilenhöhe

d-u2tmalez = Laufweite

d-hyqcnmz5 = Textpfad
d-q73msd2q = Nass

d-gfq7akbn = Parameter

d-uk5tcnwm = Transformation

d-w4ew2p2r = Beschneidung

d-3bhfidei = Verpixelt

d-6emj7eax = Körnung

d-tcpy73ad = Weichzeichnen

d-3oq5rjv3 = Schatten x

d-ho5hmfdi = Schatten y

d-7v73mzgx = Schattenunschärfe

d-aktlep6h = Schattenfarbe

d-aayx2zu4 = Innerer Schatten x

d-52usozk2 = Innerer Schatten y

d-2h5mxl46 = Unschärfe des inneren Schattens

d-f7cd75z6 = Farbe des inneren Schattens

d-zalrvie7 = Tonwertkurve

d-uslws4n3-filter-crop_x = Ausschnitt x

d-uslws4n3-filter-crop_y = Ausschnitt y

d-uslws4n3-filter-crop_width = Ausschnittsbreite

d-uslws4n3-filter-crop_height = Ausschnittshöhe

d-uslws4n3-filter-straighten = Begradigen

d-uslws4n3-filter-flip_h = Waagerecht spiegeln

d-uslws4n3-filter-flip_v = Senkrecht spiegeln

d-uslws4n3-filter-exposure = Belichtung

d-uslws4n3-filter-contrast = Kontrast

d-uslws4n3-filter-highlights = Lichter

d-uslws4n3-filter-shadows = Tiefen

d-uslws4n3-filter-saturation = Sättigung

d-uslws4n3-filter-vibrance = Dynamik

d-uslws4n3-filter-hue = Farbton

d-uslws4n3-filter-temperature = Farbtemperatur

d-uslws4n3-filter-tint = Farbstich

d-uslws4n3-filter-grayscale = Graustufen

d-uslws4n3-filter-invert = Invertieren

d-uslws4n3-filter-blur = Unschärfe

d-uslws4n3-filter-sharpen = Schärfen

d-uslws4n3-filter-vignette = Vignette

d-uslws4n3-filter-gradient_x0 = Verlauf x0

d-uslws4n3-filter-gradient_y0 = Verlauf y0

d-uslws4n3-filter-gradient_x1 = Verlauf x1

d-uslws4n3-filter-gradient_y1 = Verlauf y1

d-uslws4n3-filter-weight = Schriftschnitt

d-uslws4n3-filter-italic = Kursiv

d-uslws4n3-filter-line_height = Zeilenhöhe

d-uslws4n3-filter-tracking = Laufweite

d-uslws4n3-filter-wet = Nass

d-uslws4n3-filter-pixelated = Verpixelt

d-uslws4n3-filter-grain = Körnung

d-uslws4n3-filter-feather = Weichzeichnen

d-uslws4n3-filter-shadow_x = Schatten x
d-uslws4n3-filter-shadow_y = Schatten y

d-uslws4n3-filter-shadow_blur = Schattenunschärfe

d-uslws4n3-filter-inner_shadow_x = Innerer Schatten x

d-uslws4n3-filter-inner_shadow_y = Innerer Schatten y

d-uslws4n3-filter-inner_shadow_blur = Unschärfe des inneren Schattens

d-algnlk7t = Matte

d-3pgxznsb-filter-matte = Matte

d-b6pe4dlg = Matten-JSON
    .noun = { $count ->
        [one] Matten-JSON
       *[other] Matten-JSONs
    }

d-7s7g6rha = Quelle

d-erbjejvt = Rechteck

d-ueoeipix = Anfrage zur Farbkorrektur
    .doc = Was eine Farbkorrektur von der Zeichen-Engine des Geräts will.
    .noun = { $count ->
        [one] Anfrage zur Farbkorrektur
       *[other] Anfragen zur Farbkorrektur
    }

d-fy7u7ntb = Belichtung

d-h7ykiuc5 = Kontrast

d-akjbo2e5 = Lichter

d-gx2o2imz = Tiefen

d-gwfjuivz = Sättigung

d-y7wzyyqq = Dynamik

d-pkwjhrnw = Farbton

d-gvwo4klz = Farbtemperatur

d-6bogm2iw = Farbstich

d-mz4xapep = Graustufen

d-7pnhizf2 = Invertieren

d-vzbhth5d = Unschärfe

d-ayg5wxga = Schärfen

d-bporvvfd = Vignette

d-ueoeipix-filter-exposure = Belichtung

d-ueoeipix-filter-contrast = Kontrast

d-ueoeipix-filter-highlights = Lichter

d-ueoeipix-filter-shadows = Tiefen

d-ueoeipix-filter-saturation = Sättigung

d-ueoeipix-filter-vibrance = Dynamik

d-ueoeipix-filter-hue = Farbton

d-ueoeipix-filter-temperature = Farbtemperatur

d-ueoeipix-filter-tint = Farbstich

d-ueoeipix-filter-grayscale = Graustufen

d-ueoeipix-filter-invert = Invertieren

d-ueoeipix-filter-blur = Unschärfe

d-ueoeipix-filter-sharpen = Schärfen

d-ueoeipix-filter-vignette = Vignette

d-h5crbxpj = Zauberstab-Anfrage
    .doc = Was ein Zauberstab von der Zeichen-Engine des Geräts will: den Bereich, der einer Farbe gleicht, auf die eine Person zeigte, beantwortet in Einheiten des Bildes durch den Rahmen der Bilddatei.
    .noun = { $count ->
        [one] Zauberstab-Anfrage
       *[other] Zauberstab-Anfragen
    }

d-yyqmyjfc = Stelle

d-etjvwbve = Farbe

d-giesycja = Toleranz

d-vry7txyu = Überall

d-e3v56jsz = Rahmen
d-h5crbxpj-filter-tolerance = Toleranz

d-h5crbxpj-filter-everywhere = Überall

d-2vbtmxov = Anfrage zum Finden
    .doc = Was ein Motiv oder eine verfeinerte Kante von der Zeichen-Engine des Geräts will.
    .noun = { $count ->
        [one] Anfrage zum Finden
       *[other] Anfragen zum Finden
    }

d-3ug7cc6p = Motiv

d-mgf542kb = Verfeinern

d-oehqnmnq = Stelle

d-t76lt6eq = Farbe

d-rk2wxm5k = Toleranz

d-ipz3kxni = Ebene

d-5ijkh76y = Weichzeichnen

d-2vbtmxov-filter-subject = Motiv

d-2vbtmxov-filter-refine = Verfeinern

d-2vbtmxov-filter-tolerance = Toleranz

d-2vbtmxov-filter-feather = Weichzeichnen

d-pa6r6tbu = Anfrage zum Kombinieren
    .doc = Was ein Kombinieren von der Zeichen-Engine des Geräts will.
    .noun = { $count ->
        [one] Anfrage zum Kombinieren
       *[other] Anfragen zum Kombinieren
    }

d-bp3zj5os = Ids

d-4kf2vu7l = Operation

d-uc64pzzc = Anfrage zum Messen
    .doc = Was eine Messung will: die Markierungen oder Gruppen, die sie wiegt.
    .noun = { $count ->
        [one] Anfrage zum Messen
       *[other] Anfragen zum Messen
    }

d-2tk6phpq = Ids

d-5yhvgcbk = Anfrage zur Auswahl
    .doc = Was eine Auswahl von der Zeichen-Engine des Geräts will: eine Form, ein Lasso, Markierungen und wie sie auf den vorhandenen Bereich trifft.
    .noun = { $count ->
        [one] Anfrage zur Auswahl
       *[other] Anfragen zur Auswahl
    }

d-7omk45ql = Rechteck

d-krhe2i56 = Ellipse

d-5eu7ff7r = Punkte

d-mgxsjhfz = D

d-iqzzaywp = Markierungen

d-l4imaa4l = Modus

d-7zgzz7h2 = Weichzeichnen

d-ohfhq5pm = Zuwachs

d-ojp4xz4a = Invertieren

d-5yhvgcbk-filter-feather = Weichzeichnen

d-5yhvgcbk-filter-grow = Zuwachs

d-5yhvgcbk-filter-invert = Invertieren

d-p4ffbq5x = Prüf-Anfrage
    .doc = Was eine Prüfung des Bildes will: seine Ebenen und neuesten Markierungen.
    .noun = { $count ->
        [one] Prüf-Anfrage
       *[other] Prüf-Anfragen
    }

d-3uypu22a = Palette

d-veyz6j2q = Bereich

d-ylddsw2e = Ebene

d-6nfqt3pd = Grenze

d-p4ffbq5x-filter-palette = Palette

d-p4ffbq5x-filter-limit = Grenze

d-3chzyjat = Anfrage zum Entfernen
    .doc = Was das Entfernen von Markierungen entlang eines Pfades will.
    .noun = { $count ->
        [one] Anfrage zum Entfernen
       *[other] Anfragen zum Entfernen
    }
d-js4zslbp = Entlang

d-besel4qy = Ebene

d-qqxbh7zz = Anfrage zum Versionsvergleich
    .doc = Was der Vergleich zweier Versionen will.
    .noun = { $count ->
        [one] Anfrage zum Versionsvergleich
       *[other] Anfragen zum Versionsvergleich
    }

d-eubkyqqd = Von

d-mit27xah = Bis

d-eiocv26o = Ansehen

d-rgii66ho = Überlagerung

d-qqxbh7zz-filter-look = Ansehen

d-qqxbh7zz-filter-overlay = Überlagerung

d-ebhm4qyp = Anfrage zum Referenzvergleich
    .doc = Was ein Vergleich mit der Referenz will.
    .noun = { $count ->
        [one] Anfrage zum Referenzvergleich
       *[other] Anfragen zum Referenzvergleich
    }

d-mztro4wb = Bereich

d-67462gfa = Modus

d-hbu4rn3a = Größe

d-ebhm4qyp-filter-size = Größe

d-ulsonnwl = Die Ebene in einen Ordner legen
    .doc = Legt eine Ebene in den Ordner `group` oder nimmt sie heraus, wenn der leer ist.
    .did = legte eine Ebene in einen Ordner

d-ulsonnwl-group = Gruppe

d-ny524tm4 = Ebene beschneiden
    .doc = Hält eine Ebene an der Farbe der darunter fest oder lässt sie frei stehen.
    .did = beschnitt eine Ebene

d-ny524tm4-layer = Ebene

d-ny524tm4-clip = Beschneiden

d-yjzlh5dt = Maske hinzufügen
    .doc = Fügt einer Ebene eine Maske hinzu: eine Ebene aus Grautönen, durch die die Ebene gesehen wird.
    .did = fügte eine Maske hinzu
    .w-a-mask-is-made-for-a = Eine Maske wird für eine Ebene gemacht, nicht für eine Maske

d-yjzlh5dt-layer = Ebene

d-ss6hhi4l = Maske festlegen
    .doc = Legt das Grau einer Maske vor jeder Markierung fest, ob sie verbirgt, wo sie hell ist, und ob sie stehen bleibt, wenn sich die Markierungen ihrer Ebene bewegen.
    .did = legte eine Maske fest
    .w-only-a-mask-is-set-this = Nur eine Maske wird so festgelegt
    .w-a-mask-s-grey-is-0 = Das Grau einer Maske ist 0 bis 1

d-ss6hhi4l-layer = Ebene

d-ss6hhi4l-base = Basis

d-ss6hhi4l-invert = Invertieren

d-ss6hhi4l-unlinked = Unverknüpft

d-usk4we7m = Die Markierung in einen Ordner legen
    .doc = Legt eine Markierung in den Ordner `group` oder nimmt sie heraus, wenn der leer ist.
    .did = legte eine Markierung in einen Ordner

d-usk4we7m-group = Gruppe

d-mqefo2tn = Text hinzufügen
    .doc = Fügt eine Textmarkierung hinzu: Worte, in den Schriften des Bildes gesetzt, an `x`, `y` (seiner ersten Grundlinie) in Einheiten des Bildes.
    .did = fügte Text hinzu
    .w-there-is-no-text-to-add = Es gibt keinen Text zum Hinzufügen
    .w-text-is-larger-than-nothing = Text ist größer als nichts
    .w-a-weight-is-100-to-900 = Ein Schriftschnitt ist 100 bis 900

d-mqefo2tn-layer = Ebene

d-mqefo2tn-text = Text

d-mqefo2tn-x = X

d-mqefo2tn-y = Y

d-mqefo2tn-size = Größe

d-mqefo2tn-colour = Farbe

d-mqefo2tn-font = Schrift

d-mqefo2tn-weight = Schriftschnitt

d-mqefo2tn-italic = Kursiv

d-mqefo2tn-align = Ausrichtung

d-mqefo2tn-line_height = Zeilenhöhe

d-mqefo2tn-tracking = Laufweite
d-mqefo2tn-path = Pfad

d-pirwd2l5 = Mit einem Farbverlauf füllen
    .doc = Füllt eine Markierung mit einem Verlauf von `from` nach `to`, seine Stufen als Offsets 0 bis 1 neben ihren Farben.
    .did = füllte eine Markierung mit einem Verlauf
    .w-a-gradient-is-linear-or-radial = Ein Verlauf ist linear oder radial
    .w-a-gradient-has-two-or-more = Ein Verlauf hat zwei oder mehr Stufen, jede mit ihrer Farbe

d-pirwd2l5-kind = Art

d-pirwd2l5-x0 = X0

d-pirwd2l5-y0 = Y0

d-pirwd2l5-x1 = X1

d-pirwd2l5-y1 = Y1

d-pirwd2l5-offsets = Stufen

d-pirwd2l5-colours = Farben

d-tlxhhcet = Die Ebene mit einem Medium malen
    .doc = Malt eine Ebene mit einem Medium: `oil`, `watercolour`, eine im Bild unter ihrer id gespeicherte Voreinstellung oder leer für einfache Vektorfarbe.
    .did = änderte das Medium einer Ebene
    .w-a-medium-is-oil-watercolour-or = Ein Medium ist Öl, Aquarell oder eines, das im Bild gespeichert ist
    .w-a-mask-is-painted-in-greys = Eine Maske wird in Grautönen gemalt, ohne Medium

d-tlxhhcet-layer = Ebene

d-tlxhhcet-medium = Medium

d-4fnb5pj3 = Ein Medium speichern
    .doc = Speichert ein Medium im Bild, sodass seine Ebenen es nennen und jedes Gerät es gleich malt.
    .did = speicherte ein Medium
    .w-a-medium-has-an-id = Ein Medium hat eine Id
    .w-a-medium-is-oil-or-watercolour = Ein Medium ist Öl oder Aquarell

d-4fnb5pj3-id = Id

d-4fnb5pj3-label = Bezeichnung

d-4fnb5pj3-engine = Engine

d-4fnb5pj3-description = Beschreibung

d-4fnb5pj3-ground = Untergrund

d-4fnb5pj3-params = Parameter

d-4fnb5pj3-palette = Palette

d-keg6cder = Ein Medium entfernen

d-keg6cder-m = M

d-icmpbgfp = In Nasses malen
    .doc = Malt eine Markierung in das noch Nasse oder über trockene Farbe.
    .did = änderte, wie eine Markierung auf nasse Farbe trifft

d-icmpbgfp-wet = Nass

d-pwpqgbcc = Den Verlauf löschen
    .doc = Löscht den Verlauf einer Markierung und lässt die einfache Farbe.

d-bzc334mn = Den Text festlegen
    .doc = Legt die Worte einer Textmarkierung fest, ihre Schrift und ihren Satz.
    .did = änderte Text
    .w-only-a-text-mark-is-set = Nur eine Textmarkierung wird so festgelegt
    .w-text-is-larger-than-nothing = Text ist größer als nichts

d-bzc334mn-text = Text

d-bzc334mn-size = Größe

d-bzc334mn-colour = Farbe

d-bzc334mn-font = Schrift

d-bzc334mn-weight = Schriftschnitt

d-bzc334mn-italic = Kursiv

d-bzc334mn-align = Ausrichtung

d-bzc334mn-line_height = Zeilenhöhe

d-bzc334mn-tracking = Laufweite

d-bzc334mn-path = Pfad

d-oov7ulkw = Die Bilddatei zuschneiden
    .doc = Zeigt einen Teil der Bilddatei: den Rahmen `x`, `y`, `width`, `height` ihrer eigenen Pixel, jeder ein Anteil davon.
    .did = schnitt eine Bilddatei zu
    .w-only-a-picture-is-cropped = Nur eine Bilddatei wird zugeschnitten
    .w-a-crop-starts-inside-the-picture = Ein Ausschnitt beginnt innerhalb der Bilddatei
    .w-a-crop-has-a-width-and = Ein Ausschnitt hat eine Breite und eine Höhe
    .w-a-crop-stays-inside-the-picture = Ein Ausschnitt bleibt innerhalb der Bilddatei

d-oov7ulkw-x = X

d-oov7ulkw-y = Y

d-oov7ulkw-width = Breite

d-oov7ulkw-height = Höhe
d-52t4qnah = Die Bilddatei begradigen
    .doc = Dreht die Bilddatei in ihrem Rahmen, in Grad, und vergrößert sie, sodass keine Ecke zeigt.
    .did = begradigte eine Bilddatei
    .w-only-a-picture-is-straightened = Nur eine Bilddatei wird begradigt
    .w-a-turn-is-180-to-180 = Eine Drehung ist -180 bis 180 Grad

d-52t4qnah-degrees = Grad

d-gjkukzs4 = Die Bilddatei spiegeln
    .doc = Spiegelt die Bilddatei an ihrer Mitte.
    .did = spiegelte eine Bilddatei
    .w-only-a-picture-is-flipped = Nur eine Bilddatei wird gespiegelt

d-gjkukzs4-horizontal = Waagerecht

d-gjkukzs4-vertical = Senkrecht

d-gjkukzs4-flip_h = Waagerecht spiegeln

d-gjkukzs4-flip_v = Senkrecht spiegeln

d-apzuceoc = Den Mischmodus ändern
    .doc = Legt fest, wie sich eine Markierung mit dem darunter mischt.
    .did = änderte den Mischmodus einer Markierung

d-apzuceoc-blend = Mischmodus

d-2m4c5qhb = Die Markierung anpassen
    .doc = Stimmt eine Markierung ab, ohne ihre Pixel zu berühren: Belichtung, Kontrast und das Übrige bleiben an der Markierung und werden angewandt, wo das Bild gezeigt oder exportiert wird.
    .did = passte eine Markierung an
    .w-exposure-is-5-to-5-stops = Belichtung ist -5 bis 5 Stufen
    .w-hue-is-180-to-180-degrees = Farbton ist -180 bis 180 Grad
    .w-a-blur-is-0-to-200 = Eine Unschärfe ist 0 bis 200 Einheiten
    .w-a-sharpen-is-0-to-1 = Ein Schärfen ist 0 bis 1

d-2m4c5qhb-exposure = Belichtung

d-2m4c5qhb-contrast = Kontrast

d-2m4c5qhb-highlights = Lichter

d-2m4c5qhb-shadows = Tiefen

d-2m4c5qhb-saturation = Sättigung

d-2m4c5qhb-vibrance = Dynamik

d-2m4c5qhb-hue = Farbton

d-2m4c5qhb-temperature = Farbtemperatur

d-2m4c5qhb-tint = Farbstich

d-2m4c5qhb-grayscale = Graustufen

d-2m4c5qhb-invert = Invertieren

d-2m4c5qhb-blur = Unschärfe

d-2m4c5qhb-sharpen = Schärfen

d-2m4c5qhb-vignette = Vignette

d-nfuxdnmq = Eine Bilddatei farbkorrigieren
    .doc = Eine Bilddatei mit diesen Schritten auf ihre Pixel angewandt, als neue Datei: was eine Person oder eine Aufgabe will, wenn die korrigierte Bilddatei selbst gewollt ist und nicht das Bild, das sie zeigt.

d-nfuxdnmq-file = Datei

d-nfuxdnmq-exposure = Belichtung

d-nfuxdnmq-contrast = Kontrast

d-nfuxdnmq-highlights = Lichter

d-nfuxdnmq-shadows = Tiefen

d-nfuxdnmq-saturation = Sättigung

d-nfuxdnmq-vibrance = Dynamik

d-nfuxdnmq-hue = Farbton

d-nfuxdnmq-temperature = Farbtemperatur

d-nfuxdnmq-tint = Farbstich

d-nfuxdnmq-grayscale = Graustufen

d-nfuxdnmq-invert = Invertieren

d-nfuxdnmq-blur = Unschärfe

d-nfuxdnmq-sharpen = Schärfen

d-nfuxdnmq-vignette = Vignette

d-mm6p34kb = Auswählen, was eine Farbe berührt
    .doc = Wählt, was die Farbe an `at` in der Bilddatei des Bildes berührt, oder diese Farbe überall: der Zauberstab.
    .w-point-at-a-colour-or-name = Zeige auf eine Farbe oder nenne eine
    .w-a-tolerance-is-0-to-1 = Eine Toleranz ist 0 bis 1

d-mm6p34kb-at = Stelle

d-mm6p34kb-colour = Farbe

d-mm6p34kb-tolerance = Toleranz
d-mm6p34kb-everywhere = Überall

d-cbwhrf4z = Das Motiv auswählen
    .doc = Wählt das Motiv der Bilddatei aus, gefunden vom Modell auf dem Gerät, das die Person eingerichtet hat, seine Kante über `refine` Einheiten an die eigenen Kanten der Bilddatei angepasst, wo das gefragt ist.
    .w-a-refine-is-0-to-200 = Ein Verfeinern ist 0 bis 200 Einheiten

d-cbwhrf4z-refine = Verfeinern

d-le5d3j32 = Die Auswahl verfeinern
    .doc = Verfeinert die Kante der Auswahl über `refine` Einheiten an den eigenen Kanten der Bilddatei, Fell und Haare weich lassend.
    .w-select-an-area-first = Wähle zuerst einen Bereich aus
    .w-a-refine-is-over-0-and = Ein Verfeinern ist über 0 und höchstens 200 Einheiten

d-le5d3j32-refine = Verfeinern

d-mt2l35ii = Formen kombinieren
    .doc = Kombiniert die Markierungen zu einem Pfad, an der Stelle und in der Farbe der untersten: Formen vereint, abgezogen, geschnitten oder ausgeschlossen.
    .did = kombinierte Formen
    .w-name-two-or-more-marks-to = Nenne zwei oder mehr Markierungen zum Kombinieren oder wähle sie aus
    .w-shapes-unite-subtract-intersect-or-exclude = Formen vereinen, abziehen, schneiden oder ausschließen

d-mt2l35ii-marks = Markierungen

d-mt2l35ii-op = Operation

d-e3ojd75d = Farbe messen
    .doc = Wiegt die Farbe der Markierungen, wie das Auge sie sieht: die Farbbox jeder, ihre Farbfläche, ihren Schwerpunkt und dessen Versatz von der Mitte der Box, sodass zwei Symbole ausgewogen sind, wenn ihre Flächen und Versätze übereinstimmen.
    .w-name-the-marks-to-measure-or = Nenne die Markierungen zum Messen oder wähle sie aus

d-e3ojd75d-marks = Markierungen

d-pe33b4yd = Die Referenz setzen
    .doc = Setzt die Bilddatei, von der aus gearbeitet wird, in das Bild eingepasst und zentriert.
    .did = änderte die Referenz-Bilddatei
    .w-a-reference-is-a-picture-from = Eine Referenz ist eine Bilddatei
    .w-a-reference-is-0-to-1 = Eine Referenz ist 0 bis 1 deckend

d-pe33b4yd-file = Datei

d-pe33b4yd-opacity = Deckkraft

d-5n6xpa5g = Die Referenz löschen
    .doc = Nimmt die Referenz-Bilddatei weg.
    .did = löschte die Referenz-Bilddatei

d-ioblnef6 = Mit der Referenz vergleichen
    .doc = Zeigt das Bild neben (`side-by-side`), über (`overlay`) oder gegen (`difference`) die Referenz-Bilddatei, als neue Datei, mit dem Abstand zwischen beiden.
    .w-set-a-reference-picture-first = Setze zuerst eine Referenz-Bilddatei
    .w-a-comparison-is-side-by-side = Ein Vergleich ist side-by-side, overlay oder difference
    .w-a-comparison-is-64-to-1024 = Ein Vergleich ist 64 bis 1024 Pixel pro Seite

d-ioblnef6-mode = Modus

d-ioblnef6-size = Größe

d-5xl54bdv = Versionen vergleichen
    .doc = Vergleicht das Bild mit der Version, die der Verlauf nennt, über die Fläche gelegt: was sich zwischen beiden änderte, und der geänderte Bereich, wie er jetzt dasteht, wo er steht.
    .did = verglich Versionen

d-5xl54bdv-change = Änderung

d-zlbwseve = Mit einer Version vergleichen
    .doc = Vergleicht das Bild mit seiner Version an der Stelle `place` der Zeitleiste, die älteste bei `0.0`.
    .did = verglich Versionen

d-zlbwseve-place = Stelle

d-pnnlowuw = Den Vergleich löschen
    .doc = Nimmt den Versionsvergleich von der Fläche.
    .did = löschte einen Vergleich

d-no7aqlkc = Die Markierung bewegen
    .doc = Bewegt eine Markierung um `dx`, `dy` Einheiten des Bildes.
    .did = bewegte eine Markierung

d-no7aqlkc-dx = Dx

d-no7aqlkc-dy = Dy

d-xoiupgeb = Die Markierung skalieren
    .doc = Skaliert eine Markierung um `cx`, `cy` mit `factor`.
    .did = skalierte eine Markierung
    .w-a-scale-is-larger-than-nothing = Eine Skalierung ist größer als nichts

d-xoiupgeb-cx = Cx

d-xoiupgeb-cy = Cy

d-xoiupgeb-factor = Faktor

d-tx3nqerd = Die Markierung drehen
    .doc = Dreht eine Markierung um `cx`, `cy` um `degrees` Grad.
    .did = drehte eine Markierung

d-tx3nqerd-cx = Cx

d-tx3nqerd-cy = Cy

d-tx3nqerd-degrees = Grad

d-pwschjmr = Die Markierung beschneiden
    .doc = Schneidet eine Markierung auf die Form einer anderen Markierung oder einer Gruppe zu oder lässt sie ganz stehen.
    .did = beschnitt eine Markierung

d-pwschjmr-by = Auf

d-ptrlfypq = Die Markierung vollenden
    .doc = Legt fest, wie eine Markierung vollendet wird: Nächster-Nachbar-Pixel, Körnung, eine weiche Kante und ihre Schatten.
    .did = vollendete eine Markierung
    .w-grain-is-0-to-1 = Körnung ist 0 bis 1
    .w-a-feather-is-0-to-200 = Ein Weichzeichnen ist 0 bis 200 Einheiten

d-ptrlfypq-pixelated = Verpixelt

d-ptrlfypq-grain = Körnung

d-ptrlfypq-feather = Weichzeichnen
d-ptrlfypq-shadow_x = Schatten x

d-ptrlfypq-shadow_y = Schatten y

d-ptrlfypq-shadow_blur = Schattenunschärfe

d-ptrlfypq-shadow_colour = Schattenfarbe

d-ptrlfypq-inner_shadow_x = Innerer Schatten x

d-ptrlfypq-inner_shadow_y = Innerer Schatten y

d-ptrlfypq-inner_shadow_blur = Unschärfe des inneren Schattens

d-ptrlfypq-inner_shadow_colour = Farbe des inneren Schattens

d-3lxarsff = Die Tonwertkurve festlegen
    .doc = Legt die Tonwertkurve einer Markierung fest, `[[in, out], …]` von 0 bis 1.
    .did = legte eine Tonwertkurve fest
    .w-a-curve-point-is-in-and = Ein Kurvenpunkt ist in und out

d-3lxarsff-curve = Tonwertkurve

d-klvwsko7 = Die Ebene anpassen
    .doc = Passt alles auf einer Ebene an, nach den eigenen Anpassungen jeder Markierung.
    .did = passte eine Ebene an

d-klvwsko7-exposure = Belichtung

d-klvwsko7-contrast = Kontrast

d-klvwsko7-highlights = Lichter

d-klvwsko7-shadows = Tiefen

d-klvwsko7-saturation = Sättigung

d-klvwsko7-vibrance = Dynamik

d-klvwsko7-hue = Farbton

d-klvwsko7-temperature = Farbtemperatur

d-klvwsko7-tint = Farbstich

d-klvwsko7-grayscale = Graustufen

d-klvwsko7-invert = Invertieren

d-klvwsko7-blur = Unschärfe

d-klvwsko7-sharpen = Schärfen

d-klvwsko7-vignette = Vignette

d-ebvm6osl = Auswahl erfragen
    .doc = Fragt die Engine nach einer Auswahl und behält den Bereich, den sie antwortet.

d-ebvm6osl-ask = Anfrage

d-p4q2p4hb = Eine Form auswählen
    .doc = Wählt ein Rechteck oder eine Ellipse von einer Ecke eines Zugs zur anderen aus und trifft damit nach `mode` auf den vorhandenen Bereich (`replace`, `add`, `subtract` oder `intersect`).
    .w-a-selection-shape-is-a-rectangle = Eine Auswahlform ist ein Rechteck oder eine Ellipse
    .w-a-selection-drag-has-two-corners = Ein Auswahlzug hat zwei Ecken
    .w-a-selection-meets-the-one-there = Eine Auswahl trifft die vorhandene per replace, add, subtract oder intersect

d-p4q2p4hb-kind = Art

d-p4q2p4hb-from = Von

d-p4q2p4hb-to = Bis

d-p4q2p4hb-mode = Modus

d-p4q2p4hb-feather = Weichzeichnen

d-p4q2p4hb-grow = Zuwachs

d-p4q2p4hb-invert = Invertieren

d-h5g4zz4j = Mit dem Lasso auswählen
    .doc = Wählt den Bereich aus, den ein Lasso umfährt, geschlossen, und trifft damit nach `mode` auf den vorhandenen Bereich.
    .w-a-lasso-goes-round-an-area = Ein Lasso fährt um einen Bereich

d-h5g4zz4j-points = Punkte

d-h5g4zz4j-mode = Modus

d-h5g4zz4j-feather = Weichzeichnen

d-h5g4zz4j-grow = Zuwachs

d-h5g4zz4j-invert = Invertieren

d-n66jlhri = Eine Umrisslinie auswählen
    .doc = Wählt den Bereich aus, den eine Umrisslinie nennt, und trifft damit nach `mode` auf den vorhandenen Bereich.
    .w-a-selection-has-an-outline = Eine Auswahl hat eine Umrisslinie

d-n66jlhri-d = D

d-n66jlhri-mode = Modus

d-n66jlhri-feather = Weichzeichnen

d-n66jlhri-grow = Zuwachs
d-n66jlhri-invert = Invertieren

d-4zurdfeg = Die Auswahl wachsen lassen
    .doc = Lässt die Kante der Auswahl um `by` Einheiten wachsen (oder schrumpfen, wenn negativ).

d-4zurdfeg-by = Um

d-ainbuc4o = Die Auswahl umkehren
    .doc = Kehrt die Auswahl um: alles im Bild außer dem, was sie umfasste.

d-ub2qcbmi = Markierungen auswählen
    .doc = Wählt die Form der Markierungen oder Gruppen aus, die `ids` nennt, und trifft damit nach `mode` auf den vorhandenen Bereich.
    .w-name-the-marks-to-select = Nenne die Markierungen zum Auswählen

d-ub2qcbmi-ids = Ids

d-ub2qcbmi-mode = Modus

d-phc3yapc = Die Markierung wählen
    .doc = Wählt eine Markierung oder lässt sie los: Kombinieren und Messen wirken auf die gewählten Markierungen.
    .did = wählte eine Markierung

d-phc3yapc-m = M

d-phc3yapc-on = An

d-cyqeplvp = Die Markierungen wählen
    .doc = Wählt die Markierungen und fügt alle hinzu, die nicht gewählt waren.
    .did = wählte Markierungen

d-cyqeplvp-marks = Markierungen

d-44omrjqq = Die gewählten Markierungen freigeben
    .doc = Gibt jede gewählte Markierung frei.

d-kkjmd2mt = Die Striche entlang eines Pfades entfernen
    .doc = Entfernt die Pinselstriche, die ein Pfad kreuzt, auf `layer` oder jeder Ebene: was ein Strichradierer tut.
    .did = entfernte Striche entlang eines Pfades
    .w-a-path-has-two-points = Ein Pfad hat zwei Punkte

d-kkjmd2mt-points = Punkte

d-kkjmd2mt-layer = Ebene

d-l6gvckmv = Das Bild prüfen
    .doc = Listet, was das Bild hält: seine Ebenen und neuesten Markierungen, mit Ids, Grenzen und Farben, und mit `palette` die Farben, mit denen es malt.
    .w-an-inspection-lists-0-to-400 = Eine Prüfung listet 0 bis 400 Markierungen

d-l6gvckmv-palette = Palette

d-l6gvckmv-limit = Grenze

d-l6gvckmv-layer = Ebene

d-l6gvckmv-region = Bereich

d-v7aky4jd = Die Pinseldynamik festlegen
    .doc = Legt fest, wie der Pinsel eine Markierung legt: seinen Fluss, seine Härte, seinen Abstand, seine Glättung und seine Verjüngung, jeder 0 bis 1.
    .did = legte die Pinseldynamik fest

d-v7aky4jd-flow = Fluss

d-v7aky4jd-hardness = Härte

d-v7aky4jd-spacing = Abstand

d-v7aky4jd-smoothing = Glättung

d-v7aky4jd-taper = Verjüngung

d-3nneq3xo-lasso = Lasso

d-zci3ltpl = Maske hinzufügen~present

d-ipba7qqv = Maske festlegen~present

d-j5cxvpsm = Text hinzufügen~present

d-7lkzxbj5 = Mit Verlauf füllen~present

d-jhedsahx = Medium festlegen~present

d-zu7qeydz = Medium speichern~present

d-6wdxdo5a = Text festlegen~present

d-667f442p = Bilddatei zuschneiden~present

d-jl3at4ci = Bilddatei begradigen~present

d-id3zg3tv = Bilddatei spiegeln~present

d-qdptb2o7 = Markierung anpassen~present

d-s64swn5k = Nach Farbe auswählen~present

d-od43nccq = Motiv auswählen~present

d-o4yvpii6 = Auswahl verfeinern~present

d-d3vnhf3u = Markierungen kombinieren~present

d-m7kwzjqw = Markierungen messen~present
d-uszm3wfi = Referenz setzen~present

d-pku6nuhg = Mit Referenz vergleichen~present

d-s3f4nc7f = Markierung skalieren~present

d-tj3vx3cr = Markierung vollenden~present

d-md6jnqk7 = Tonwertkurve festlegen~present

d-e7pvnf4m = Form auswählen~present

d-dsllm4jo = Mit Lasso auswählen~present

d-4bwpv2zt = Umrisslinie auswählen~present

d-zjijcafr = Markierungen auswählen~present

d-bllls2cq = Entlang eines Pfades entfernen~present

d-7s5gijts = Bild prüfen~present

d-hs75q37a = Textbereich
    .w-text = Text
    .w-size = Größe
    .w-line-height = Zeilenhöhe
    .w-tracking = Laufweite
    .w-set-along-a-path-as-svg = Entlang eines Pfades setzen, als SVG-Pfaddaten
    .w-text-colour = Textfarbe
    .w-light = Mager
    .w-regular = Normal
    .w-bold = Fett
    .w-upright = Aufrecht
    .w-italic = Kursiv

d-x75ur66o = Bildbereich
    .w-picture = Bilddatei
    .w-flip-across = Waagerecht spiegeln
    .w-flip-down = Senkrecht spiegeln
    .w-crop = Zuschneiden
    .w-whole-picture = Ganze Bilddatei
    .w-centre-half = Mittlere Hälfte
    .w-centre-strip = Mittlerer Streifen
    .w-blend = Mischmodus: { $blend_of }
    .w-turn = Drehen
    .w-exposure = Belichtung
    .w-contrast = Kontrast
    .w-highlights = Lichter
    .w-shadows = Tiefen
    .w-saturation = Sättigung
    .w-vibrance = Dynamik
    .w-hue = Farbton
    .w-warmth = Wärme
    .w-tint = Farbstich
    .w-blur = Unschärfe
    .w-sharpen = Schärfen
    .w-vignette = Vignette
    .w-colour = Farbe
    .w-black-and-white = Schwarzweiß
    .w-un-invert = Invertierung aufheben
    .w-invert = Invertieren

d-7ycx6t2x = Versionszeitleiste
    .w-versions = Versionen
    .w-versions-of = Versionen von { $title }
    .w-back-to-now = Zurück zu jetzt
