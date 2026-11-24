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

d-akflqlvk = Geöffnetes Bild
    .doc = Ein Bild, in das eine `.artwork`-Datei geöffnet wurde: die Felder, die diese App behält.
    .noun = { $count ->
        [one] geöffnetes Bild
       *[other] geöffnete Bilder
    }

d-r5usrdnc = Titel

d-yogkal7v = Breite

d-7y7zrgxh = Höhe

d-osqzkwh6 = Ebenen

d-akflqlvk-filter-width = Breite

d-akflqlvk-filter-height = Höhe

d-4rfh6alh = Geöffnete Ebene
    .noun = { $count ->
        [one] geöffnete Ebene
       *[other] geöffnete Ebenen
    }

d-qvppsycy = Name

d-6ksjlnwm = Medium

d-ocg7lpks = Deckkraft

d-7eyz4jyf = Sichtbar

d-4rfh6alh-filter-opacity = Deckkraft

d-4rfh6alh-filter-visible = Sichtbar

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

d-im6e3ij3-name = Name

d-25k3qffg = Ebene umbenennen
    .doc = Benennt eine Ebene um.
    .did = hat eine Ebene umbenannt
    .w-a-layer-has-a-name = Eine Ebene hat einen Namen

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

d-3yu27udg = Punkt malen
    .doc = Malt den Punkt, an dem die Hand ist: der Strich, der gerade gemalt wird, nimmt den nächsten Punkt, sonst beginnt hier ein Strich.

d-3yu27udg-point = Punkt

d-3yu27udg-size = Größe

d-3yu27udg-colour = Farbe

d-3yu27udg-brush = Pinsel

d-ocgibcwg = Bibliothek
    .w-no-artwork-yet = Noch kein Bild
    .w-new-artwork-starts-one = Neues Bild beginnt eines

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
