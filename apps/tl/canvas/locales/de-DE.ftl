# Canvas auf Deutsch. Die Schlüssel sind die festen ids der Deklarationen (app.ids); siehe locales/en-US.ftl.

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
    .doc = A picture's bytes, by the address they are kept at, and the frame it is drawn in.

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
    .doc = Places a picture the person chose on the top layer, fitted inside the artwork and centred: the picture stays its own object, so it moves, crops and syncs like paint.
    .did = platzierte ein Bild
    .w-a-picture-is-placed-from-a = Ein Bild wird aus einer Datei platziert

d-ynku6xaf-file = File

d-od5n7ehr = Place picture~present
