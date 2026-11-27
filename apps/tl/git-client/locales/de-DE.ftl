# Git auf Deutsch. Die Schlüssel sind die stabilen ids der Deklarationen (app.ids); die Wörter der Quelle stehen in den Quellen des Pakets.

d-so-tilo-git-client = Git

d-so-tilo-git-client-keywords = git, repository, commit, branch, merge, diff, stage, stash, remote, version control, quellcode, versionierung, zweig

d-bgacivdi = Hunk
    .doc = Ein Hunk eines Diffs, der eigens bereitgestellt oder verworfen werden kann.
    .noun = { $count ->
        [one] Hunk
       *[other] Hunks
    }

d-a7uhrt52 = Index

d-5ccadzg6 = Kopfzeile

d-dfethllq = Alter Anfang

d-pigkwwm6 = Neuer Anfang

d-bgacivdi-filter-index = Index

d-bgacivdi-filter-old_start = Alter Anfang

d-bgacivdi-filter-new_start = Neuer Anfang

d-nf7rji7c = Änderung
    .doc = Eine geänderte Datei: wo sie steht, was aus ihr wurde und ihr Diff.
    .noun = { $count ->
        [one] Änderung
       *[other] Änderungen
    }

d-ulyz6e5m = Pfad

d-pyq3ne6a = Alter Pfad

d-gyf3ucvv = Bereich

d-a64nnfrl = Art

d-pgbhcfmd = Zeilen hinzugefügt

d-cb3hp5ts = Zeilen entfernt

d-ddlu6eiw = Binär

d-547agrds = Unsere
    .doc = Was das Behalten jeder Konfliktseite verwerfen würde, in Zeilen.

d-gcyh3qk6 = Ihre

d-5bywp6ty = Sprache
    .doc = Die Sprachkennung der Datei (`rust`, `json`, ...), für die Färbung der Codeansicht.

d-2ejdsikb = Eine Spalte
    .doc = Die Änderung in einer Spalte: jeder Zeile alte und neue Nummer, ihr Zeichen und ihr Text.

d-bdjzphf3 = Links
    .doc = Die alte Seite einer Ansicht nebeneinander, Zeile für Zeile an `right` ausgerichtet.

d-qmisixex = Rechts
    .doc = Die neue Seite einer Ansicht nebeneinander.

d-qo6hyuiv = Hunks
    .doc = Seine Hunks, jeder als eigener JSON-Text.

d-nf7rji7c-filter-area = Bereich

d-nf7rji7c-filter-kind = Art

d-nf7rji7c-filter-additions = Zeilen hinzugefügt

d-nf7rji7c-filter-deletions = Zeilen entfernt

d-nf7rji7c-filter-binary = Binär

d-nf7rji7c-filter-ours = Unsere

d-nf7rji7c-filter-theirs = Ihre

d-xp24lijk = Verweis
    .doc = Ein Verweis, der auf einen Commit zeigt.
    .noun = { $count ->
        [one] Verweis
       *[other] Verweise
    }

d-zl57j56i = Name

d-7lddqvmy = Art

d-xp24lijk-filter-kind = Art

d-ssa2n2ws = Commit
    .doc = Ein Commit des Repositories.
    .noun = { $count ->
        [one] Commit
       *[other] Commits
    }

d-ge4jaage = Id

d-ktcweris = Zusammenfassung

d-qkodo2um = Text

d-mbs2zev2 = Name des Autors

d-2rplzpux = E-Mail des Autors

d-g5wqmtlu = Zeit
    .doc = Wann er gemacht wurde, in Sekunden seit der Epoche.

d-i5ezibch = Alter
    .doc = Wie lange das her ist, in Sekunden.

d-ae7mbonn = Datum
    .doc = Wann er gemacht wurde, wie eine Maschine es liest.

d-vngsr7bo = Vorgänger

d-3k7gvvan = Verweise

d-okssmwxo = Spalte

d-ssa2n2ws-filter-time = Zeit

d-ssa2n2ws-filter-age = Alter

d-ssa2n2ws-filter-column = Spalte

d-uz3urzuh = Zweig
    .doc = Ein Zweig des Repositories.
    .noun = { $count ->
        [one] Zweig
       *[other] Zweige
    }

d-mmht373l = Name

d-2xd7n6ck = Ist Head

d-ejqxjylh = Ist Remote

d-vkfjbs4a = Upstream

d-uvswvf53 = Voraus

d-awh54bp4 = Zurück

d-fkwngzoz = Spitze

d-uz3urzuh-filter-is_head = Ist Head

d-uz3urzuh-filter-is_remote = Ist Remote

d-uz3urzuh-filter-ahead = Voraus

d-uz3urzuh-filter-behind = Zurück

d-erag4w44 = Tag
    .doc = Ein Tag des Repositories.
    .noun = { $count ->
        [one] Tag
       *[other] Tags
    }

d-eimgqjmn = Name

d-rvt4lxwy = Ziel

d-qpus5rj2 = Nachricht

d-is4hlega = Stash
    .doc = Ein Stash des Arbeitsverzeichnisses.
    .noun = { $count ->
        [one] Stash
       *[other] Stashes
    }

d-plxxvefl = Index

d-npo2d33u = Nachricht

d-kr54kttq = Zweig

d-is4hlega-filter-index = Index

d-s2coi475 = Remote
    .doc = Ein Remote, von dem das Repository abruft und zu dem es pusht.
    .noun = { $count ->
        [one] Remote
       *[other] Remotes
    }

d-fi4gej3v = Name

d-ug544zky = Url

d-eozposlx = Arbeitsverzeichnis
    .doc = Ein mit dem Repository verbundenes Arbeitsverzeichnis.
    .noun = { $count ->
        [one] Arbeitsverzeichnis
       *[other] Arbeitsverzeichnisse
    }

d-syhlyvff = Name

d-s43oholm = Pfad

d-zumfqfnq = Head

d-rcy5osnr = Aktuell

d-eozposlx-filter-current = Aktuell

d-b5uulgn7 = Geänderte Datei
    .doc = Eine Datei, die ein Commit geändert hat, mit ihrem Diff.
    .noun = { $count ->
        [one] geänderte Datei
       *[other] geänderte Dateien
    }

d-elliscls = Pfad

d-tlnyi6uz = Alter Pfad

d-65rizway = Art

d-7u7ngqkr = Zeilen hinzugefügt

d-qdtrudtf = Zeilen entfernt

d-4pwix6ke = Binär

d-vgnn6ynm = Sprache

d-j2nczx73 = Eine Spalte

d-kgmcwjbi = Links

d-xtgtu5ad = Rechts

d-b5uulgn7-filter-kind = Art

d-b5uulgn7-filter-additions = Zeilen hinzugefügt

d-b5uulgn7-filter-deletions = Zeilen entfernt

d-b5uulgn7-filter-binary = Binär

d-bduvfbif = Ordner
    .doc = Ein Ordner, den die Repository-Auswahl gelistet hat.
    .noun = { $count ->
        [one] Ordner
       *[other] Ordner
    }

d-mazzivt3 = Name

d-67eqnwql = Pfad

d-amedesig = Ordner

d-bduvfbif-filter-folder = Ordner

d-njfqrkl5 = Fenster
    .doc = Das Fenster einer Person auf das Repository: das Repository, das sie auf diesem Gerät geöffnet hat, ihr Arbeitsverzeichnis zuletzt gelesen, und wie sie Diffs liest.
    .noun = { $count ->
        [one] Fenster
       *[other] Fenster
    }

d-vrfkfgub = Pfad
    .doc = Der Pfad des Repositories, das diese Person hier geöffnet hat; leer, bis sie eines öffnet.

d-qtrusa64 = Änderungen

d-qakm3prq = Ansicht

d-5pb4bgxw = Hervorhebung

d-m5eoofqc = Leerraum

d-mk3evbqh = Umbruch

d-ma5zpjxi = Ausstehender Wechsel
    .doc = Der Zweig, auf den ein Wechsel wartet, solange das Arbeitsverzeichnis nicht festgeschriebene Arbeit hält.

d-taytqzee = Am Durchsuchen
    .doc = Ordner und Einträge der Auswahl, während die Person wählt, wo ein Repository liegt.

d-cxuhuns5 = Ordner

d-ermpuo4w = Einträge

d-gpp2fllc = Commit-Id
    .doc = Der Commit, dessen geänderte Dateien gelesen wurden, und diese Dateien als JSON-Liste von Texten.

d-7ihfqmuw = Commit-Dateien

d-mr62vl75 = Hinweis
    .doc = Was die letzte Aktion tat, einmal gesagt; was schiefging, bis es ausgeblendet wird.

d-hjgppyqi = Fehler

d-njfqrkl5-filter-layout = Ansicht

d-njfqrkl5-filter-highlight = Hervorhebung

d-njfqrkl5-filter-whitespace = Leerraum

d-njfqrkl5-filter-wrap = Umbruch

d-njfqrkl5-filter-browsing = Am Durchsuchen

d-3peaa3l5 = Repository
    .doc = Das Git des Arbeitsbereichs: das Repository, das es öffnet, was Mitglieder davon sehen, und ein Fenster pro Person darauf.
    .noun = { $count ->
        [one] Repository
       *[other] Repositories
    }

d-n4wepl5s = Pfad
    .doc = Wo das Repository auf diesem Gerät liegt.

d-6ju4ea2u = Name

d-ori4pxhp = Head

d-nqllokpk = Arbeitsverzeichnis

d-ev43omls = Name des Autors

d-6ejc2liq = E-Mail des Autors

d-os3blsxu = Abbild
    .doc = Alles, was das Gerät über das Repository geantwortet hat, als Text: seine Commits, Zweige, Tags, Stashes, Remotes und Arbeitsverzeichnisse.

d-vioio4dz = Fenster

d-cdrt77b4 = Geladen
    .noun = { $count ->
        [one] geladen
       *[other] geladen
    }

d-u66layc7 = Antwort
    .doc = Was der antwortende Aufruf gemacht hat, kurz gesagt: die Kurz-Id eines Commits oder Stashes.

d-dbwyzz4f = Name

d-bhabfybv = Head

d-ifdrbibf = Arbeitsverzeichnis

d-eptgnppa = Name des Autors

d-zqo4qf2p = E-Mail des Autors

d-dijqjokd = Änderungen
    .doc = Jede Sammlung als Liste von JSON-Texten, einer je Eintrag.

d-ylfrksiq = Commits

d-lypvtyhb = Zweige

d-g4pu5gfp = Tags

d-cvskkv3u = Stashes

d-sq36vhyn = Remotes

d-izkg5khe = Arbeitsverzeichnisse

d-t3ptayvw = Arbeitsverzeichnis-Fehler

d-533ctb76 = Übertragene Änderung
    .noun = { $count ->
        [one] übertragene Änderung
       *[other] übertragene Änderungen
    }

d-37sxq5bs = Pfad

d-dvptb6yi = Alter Pfad

d-m6m22ctl = Bereich

d-rck5ywq4 = Art

d-7anyihpj = Zeilen hinzugefügt

d-kvrcmb3v = Zeilen entfernt

d-7dddbyyj = Binär

d-zzjs6njo = Unsere

d-ej3kja37 = Ihre

d-tlea3vto = Sprache

d-hr5ogyyj = Eine Spalte

d-edfwguh5 = Links

d-2kp7i4ab = Rechts

d-h6xzvamk = Hunks

d-533ctb76-filter-area = Bereich

d-533ctb76-filter-kind = Art

d-533ctb76-filter-additions = Zeilen hinzugefügt

d-533ctb76-filter-deletions = Zeilen entfernt

d-533ctb76-filter-binary = Binär

d-533ctb76-filter-ours = Unsere

d-533ctb76-filter-theirs = Ihre

d-eqy5lhxf = Übertragener Commit
    .noun = { $count ->
        [one] übertragener Commit
       *[other] übertragene Commits
    }

d-ktz2fyrf = SHA

d-ugphvrb2 = Zusammenfassung

d-hg67fbg2 = Text

d-cjxgeuao = Name des Autors

d-ypiqlu3b = E-Mail des Autors

d-3mge2zfz = Zeit

d-uyueykhv = Alter

d-o4wtnyc7 = Datum

d-o5fnmdlp = Vorgänger

d-oz6i3qfk = Verweise

d-firuj3kf = Spalte

d-eqy5lhxf-filter-time = Zeit

d-eqy5lhxf-filter-age = Alter

d-eqy5lhxf-filter-column = Spalte

d-pwhnois3 = Übertragener Verweis
    .noun = { $count ->
        [one] übertragener Verweis
       *[other] übertragene Verweise
    }

d-cgcdv4xh = Name

d-wlrzlclq = Art

d-pwhnois3-filter-kind = Art

d-jizhu6al = Übertragener Zweig
    .noun = { $count ->
        [one] übertragener Zweig
       *[other] übertragene Zweige
    }

d-iwhrqigk = Name

d-lnt6kiuo = Ist Head

d-i4erkio3 = Ist Remote

d-ej7g53n3 = Upstream

d-vupfshru = Voraus

d-koq4jc7k = Zurück

d-mfbh6kyh = Spitze

d-jizhu6al-filter-is_head = Ist Head

d-jizhu6al-filter-is_remote = Ist Remote

d-jizhu6al-filter-ahead = Voraus

d-jizhu6al-filter-behind = Zurück

d-7kdb2p7c = Übertragenes Tag
    .noun = { $count ->
        [one] übertragenes Tag
       *[other] übertragene Tags
    }

d-nsgs47is = Name

d-yn67zzls = Ziel

d-uju3pds5 = Nachricht

d-hjphbbmv = Übertragener Stash
    .noun = { $count ->
        [one] übertragener Stash
       *[other] übertragene Stashes
    }

d-zyrzfh5t = Index

d-bd32e2q6 = Nachricht

d-f3lmbvrn = Zweig

d-hjphbbmv-filter-index = Index

d-3dq47wrt = Übertragenes Remote
    .noun = { $count ->
        [one] übertragenes Remote
       *[other] übertragene Remotes
    }

d-d577aqii = Name

d-ihlupgbv = Url

d-pvxnlfqr = Übertragenes Arbeitsverzeichnis
    .noun = { $count ->
        [one] übertragenes Arbeitsverzeichnis
       *[other] übertragene Arbeitsverzeichnisse
    }

d-htbitpma = Name

d-f3a3kvnf = Pfad

d-cqc2kufj = Head

d-dvbe2xkw = Aktuell

d-pvxnlfqr-filter-current = Aktuell

d-yzovkqfv = Übertragene Datei
    .noun = { $count ->
        [one] übertragene Datei
       *[other] übertragene Dateien
    }

d-44l2jnvk = Pfad

d-fr7rebu2 = Alter Pfad

d-qggzylu7 = Art

d-b4js3rjp = Zeilen hinzugefügt

d-ivsbutw6 = Zeilen entfernt

d-f4hq5otl = Binär

d-bjqxfixo = Sprache

d-o6jj727v = Eine Spalte

d-oc5lrtao = Links

d-vmcjzzz5 = Rechts

d-yzovkqfv-filter-kind = Art

d-yzovkqfv-filter-additions = Zeilen hinzugefügt

d-yzovkqfv-filter-deletions = Zeilen entfernt

d-yzovkqfv-filter-binary = Binär

d-adg4z6qi = Pfadanfrage
    .noun = { $count ->
        [one] Pfadanfrage
       *[other] Pfadanfragen
    }

d-aqyg5aef = Pfad

d-dioumhuy = Leerraum
    .doc = Ob ein Diff den Leerraum am Ende seiner Zeilen zeigt.

d-adg4z6qi-filter-whitespace = Leerraum

d-7wb3dvw6 = Dateianfrage
    .noun = { $count ->
        [one] Dateianfrage
       *[other] Dateianfragen
    }

d-qahiprkz = Pfad

d-vywcliib = Datei

d-lcmux77m = Hunk-Anfrage
    .noun = { $count ->
        [one] Hunk-Anfrage
       *[other] Hunk-Anfragen
    }

d-nh7yqayy = Pfad

d-vfjaobn2 = Datei

d-tprahdo6 = Hunk

d-7pgxgdw3 = Zurücknehmen

d-lcmux77m-filter-hunk = Hunk

d-lcmux77m-filter-unstage = Zurücknehmen

d-ewgh6vvx = Commit-Anfrage
    .noun = { $count ->
        [one] Commit-Anfrage
       *[other] Commit-Anfragen
    }

d-ofstus7y = Pfad

d-bxihpw7v = Nachricht

d-oypyxpzg = Zweig-Anfrage
    .noun = { $count ->
        [one] Zweig-Anfrage
       *[other] Zweig-Anfragen
    }

d-fi4zi42q = Pfad

d-c4fbjyom = Name

d-l7meu2gh = Auschecken

d-oypyxpzg-filter-checkout = Auschecken

d-ha25kgjn = Checkout-Anfrage
    .noun = { $count ->
        [one] Checkout-Anfrage
       *[other] Checkout-Anfragen
    }

d-klf3gt64 = Pfad

d-rpchrkvk = Name

d-cuml3cz6 = Stash

d-ha25kgjn-filter-stash = Stash

d-vumkq7wu = Commit-Dateianfrage
    .noun = { $count ->
        [one] Commit-Dateianfrage
       *[other] Commit-Dateianfragen
    }

d-3w5wb2ew = Pfad

d-vbqr55tj = Commit

d-nrx2nxsc = Arbeitsverzeichnis-Anfrage
    .noun = { $count ->
        [one] Arbeitsverzeichnis-Anfrage
       *[other] Arbeitsverzeichnis-Anfragen
    }

d-ukwgoqln = Pfad

d-py6thmfz = Ziel

d-rwocyieo = Zweig

d-ibmiafbe = Start

d-obk6vca4 = Stash-Ablegen-Anfrage
    .noun = { $count ->
        [one] Stash-Ablegen-Anfrage
       *[other] Stash-Ablegen-Anfragen
    }

d-yeg5b74s = Pfad

d-igkwc6uc = Nachricht

d-h2kfi5de = Stash-Anwenden-Anfrage
    .noun = { $count ->
        [one] Stash-Anwenden-Anfrage
       *[other] Stash-Anwenden-Anfragen
    }

d-y3nby6jr = Pfad

d-eoe3gfz5 = Index

d-h2kfi5de-filter-index = Index

d-7oa4a4o4 = Lösungsanfrage
    .noun = { $count ->
        [one] Lösungsanfrage
       *[other] Lösungsanfragen
    }

d-yltk3frx = Pfad

d-o2dbxhal = Datei

d-qvivrkzl = Unsere

d-7oa4a4o4-filter-ours = Unsere

d-3lwjrl2s = Remote-Anfrage
    .noun = { $count ->
        [one] Remote-Anfrage
       *[other] Remote-Anfragen
    }

d-gifph57t = Pfad

d-tz3p3qeb = Remote

d-uzzqrnrx = Änderungsbereich
    .doc = Wo eine Änderung steht: im nächsten Commit, noch im Arbeitsverzeichnis oder noch unverfolgt.

d-37k7idbc = Bereitgestellt

d-w2lkndlx = Nicht bereitgestellt

d-6fdncnb6 = Unverfolgt

d-4uryee3o = Änderungsart
    .doc = Was aus einer Datei wurde.

d-y36zq2bw = Hinzugefügt

d-4keolvna = Geändert

d-lwiydt7j = Gelöscht

d-66cy3vrg = Umbenannt

d-hwlxq3oi = Unverfolgt

d-gxg2bwkg = Im Konflikt

d-leotiz2h = Verweisart
    .doc = Was ein Verweis auf einem Commit ist.

d-lqphpv2e = Head

d-axu5fcnu = Lokaler Zweig

d-7er7phvj = Remote-Zweig

d-tjfal3xx = Tag

d-drarr3v2 = Diff-Ansicht
    .doc = Wie ein Diff gelesen wird.

d-fowmhd76 = Eine Spalte

d-27umt2ce = Nebeneinander

d-sevk4ejg = Ankommen
    .doc = Beim Öffnen der App bekommt die Person ihr Fenster darauf, auch wenn sie noch kein Repository öffnet.

d-noccxekz = Aktualisieren
    .doc = Liest das Repository auf der Platte neu in die Aufzeichnungen der App, für dieses Fenster.

d-hftffvs5 = Repository öffnen
    .doc = Öffnet das Repository unter `path` auf diesem Gerät.

d-hftffvs5-path = Pfad

d-537xcsny = Repository wählen
    .doc = Beginnt, den Ordner eines Repositories zu wählen, und listet den Heimatordner dieses Geräts.

d-2eebbcq4 = Ordner lesen
    .doc = Listet `folder` in der Repository-Auswahl.

d-2eebbcq4-folder = Ordner

d-cdmdeeny = Geöffneten Ordner öffnen
    .doc = Öffnet den Ordner, den die Auswahl zeigt, als Repository.

d-nyioin33 = Auswahl schließen
    .doc = Blendet die Auswahl aus.

d-2tto3qfj = Datei bereitstellen
    .doc = Stellt eine geänderte Datei bereit.

d-2tto3qfj-c = Ä

d-by2ln3ko = Datei zurücknehmen
    .doc = Nimmt eine bereitgestellte Datei aus dem nächsten Commit zurück.

d-by2ln3ko-c = Ä

d-xleo3or5 = Dateiänderungen verwerfen
    .doc = Verwirft die Bearbeitungen an einer Datei im Arbeitsverzeichnis oder löscht sie, wenn sie nie verfolgt war.

d-xleo3or5-c = Ä

d-r3j7bakj = Alles bereitstellen
    .doc = Stellt jede Änderung bereit.

d-ajzbhhzn = Alles zurücknehmen
    .doc = Nimmt jede bereitgestellte Änderung aus dem nächsten Commit zurück.

d-t6hvr46v = Hunk bereitstellen
    .doc = Stellt einen Hunk einer Datei bereit.

d-t6hvr46v-c = Ä

d-t6hvr46v-hunk = Hunk

d-rliycbbj = Hunk zurücknehmen
    .doc = Nimmt einen bereitgestellten Hunk aus dem nächsten Commit zurück.

d-rliycbbj-c = Ä

d-rliycbbj-hunk = Hunk

d-p3tvpthq = Diese Änderung verwerfen
    .doc = Verwirft einen Hunk der Bearbeitungen einer Datei im Arbeitsverzeichnis.

d-p3tvpthq-c = Ä

d-p3tvpthq-hunk = Hunk

d-24f7tx6t = Bereitgestellte Änderungen festschreiben
    .doc = Schreibt fest, was bereitgestellt ist.
    .w-a-commit-has-a-message = A commit has a message
    .w-there-are-no-staged-changes-to = There are no staged changes to commit

d-24f7tx6t-message = Nachricht

d-33dk6utz = Eine Seite behalten
    .doc = Behält eine Seite einer Konfliktdatei und stellt sie als gelöst bereit.

d-33dk6utz-c = Ä

d-33dk6utz-ours = Unsere

d-6jfbtamv = Taste für eine Datei
    .doc = Die auf einer Dateizeile gedrückte Taste: `s` stellt sie bereit oder nimmt sie aus dem nächsten Commit zurück, und bei einer Konfliktdatei behält `o` oder `t` unsere oder ihre Seite.

d-6jfbtamv-c = C

d-6jfbtamv-key = Taste

d-pzeglzgm = Zweig anlegen
    .doc = Legt einen Zweig an und wechselt zu ihm.
    .w-a-branch-has-a-name = A branch has a name

d-pzeglzgm-name = Name

d-ipza5mm5 = Zweig auschecken
    .doc = Wechselt zu einem Zweig und fragt, was aus nicht festgeschriebener Arbeit wird, wenn es welche gibt.

d-ipza5mm5-name = Name

d-mjiefmgr = Ablegen und wechseln
    .doc = Beantwortet die Wechselfrage, indem nicht festgeschriebene Arbeit zuerst im Stash abgelegt wird.

d-uvji2mjq = Mitnehmen und wechseln
    .doc = Beantwortet die Wechselfrage, indem nicht festgeschriebene Arbeit mitgenommen wird.

d-h2e25sqj = Wechsel abbrechen
    .doc = Blendet die Wechselfrage aus.

d-svlikb4k = Wechseln zu
    .doc = Wechselt zu `name` und legt nicht festgeschriebene Arbeit ab, wenn `stash` es sagt.

d-svlikb4k-name = Name

d-svlikb4k-stash = Stash

d-vvjj7n6v = Zweig löschen
    .doc = Löscht einen lokalen Zweig.

d-vvjj7n6v-name = Name

d-js4difv7 = Taste für einen Zweig
    .doc = Die auf einer Zweigzeile gedrückte Taste: `Enter` wechselt zu ihm, und `Delete` löscht ihn, wenn er ein lokaler Zweig ist, der nicht ausgecheckt ist.

d-js4difv7-name = Name

d-js4difv7-is_remote = Ist Remote

d-js4difv7-key = Taste

d-mu2sgzjp = Änderungen ablegen
    .doc = Legt die Änderungen des Arbeitsverzeichnisses mit einer Nachricht im Stash ab.

d-mu2sgzjp-message = Nachricht

d-wboirbaa = Stash anwenden
    .doc = Wendet einen Stash an und lässt ihn in der Liste.

d-wboirbaa-index = Index

d-kn6abrns = Arbeitsverzeichnis anlegen
    .doc = Legt einen neuen Zweig bei `start` und ein eigenes Arbeitsverzeichnis an.
    .w-a-worktree-branch-has-a-name = A worktree branch has a name
    .w-a-worktree-has-a-destination-path = A worktree has a destination path
    .w-a-worktree-starts-somewhere = A worktree starts somewhere

d-kn6abrns-path = Pfad

d-kn6abrns-branch = Zweig

d-kn6abrns-start = Start

d-7lpwxrhi = Vom Remote abrufen
    .doc = Ruft von einem Remote ab.

d-7lpwxrhi-remote = Remote

d-c2uabswh = Vom Remote ziehen
    .doc = Ruft ab und spult den aktuellen Zweig auf seinen Upstream vor.

d-c2uabswh-remote = Remote

d-aqzo32et = Zum Remote pushen
    .doc = Pusht den aktuellen Zweig auf einen gleichnamigen Zweig eines Remotes.

d-aqzo32et-remote = Remote

d-vbt5h6db = Commit lesen
    .doc = Liest die geänderten Dateien eines Commits und ihre Diffs beim Öffnen.

d-vbt5h6db-id = Id

d-7xbbdu3w = Taste für einen Commit
    .doc = Die auf einer Commit-Zeile gedrückte Taste: `Enter` öffnet seine geänderten Dateien, und `Backspace` oder `Escape` geht aus ihnen zurück und lässt die Commit-Liste, wie sie war.

d-7xbbdu3w-id = Id

d-7xbbdu3w-key = Taste

d-umhtkuee = Diff-Ansicht
    .doc = Ändert, wie diese Person Diffs liest: eine Spalte oder zwei.

d-umhtkuee-s = S

d-umhtkuee-layout = Ansicht

d-idxn62yl = Syntaxhervorhebung
    .doc = Schaltet die Syntaxfärbung für diese Person ein oder aus.

d-idxn62yl-s = S

d-idxn62yl-on = An

d-fcrm23io = Leerraum zeigen
    .doc = Zeigt oder verbirgt den Leerraum am Ende der Diff-Zeilen für diese Person.

d-fcrm23io-s = S

d-fcrm23io-on = An

d-su4j4r5w = Lange Zeilen umbrechen
    .doc = Bricht lange Diff-Zeilen um oder lässt sie breiter als die Ansicht laufen, für diese Person.

d-su4j4r5w-s = S

d-su4j4r5w-wrap = Umbruch

d-247oj4wo = Fehler ausblenden
    .doc = Blendet die Fehlermeldung aus.

d-eqtvrcxw = Hinweis ausblenden
    .doc = Blendet den Hinweis aus.

d-pfjepyyc = Fragt
    .doc = Führt `op` mit `ask` aus, was das Repository auf dem Gerät ändert, und behält die Antwort: das Repository, wie es jetzt steht, und das Arbeitsverzeichnis, wie das Gerät es gelesen hat. Rückgängig nimmt eine Änderung auf dem Gerät nicht zurück.

d-pfjepyyc-s = F

d-pfjepyyc-op = Aufruf

d-pfjepyyc-ask = Anfrage

d-db3bvtrd = Liest
    .doc = Liest das Repository auf dem Gerät mit `ask`, ohne dort etwas zu ändern, und behält die Antwort.

d-db3bvtrd-s = F

d-db3bvtrd-ask = Anfrage

d-73f5j5d6 = Behält
    .doc = Behält, was das Gerät geantwortet hat: das Repository, wie es jetzt steht, und das Arbeitsverzeichnis, wie das Gerät es gelesen hat.

d-73f5j5d6-s = F

d-73f5j5d6-text = Text

d-ipe3wuqn = Dauer als Text
    .w-ms = { $ms } ms
    .w-0-s = { $value },0{ $part } s
    .w-s = { $value },{ $part } s

d-7w3h372c = Prüfungstitel
    .w-passed = { $check_id } bestanden
    .w-failed-with-exit-code = { $check_id } fehlgeschlagen mit Exit-Code { $code }
    .w-ended-on-a-signal = { $check_id } wurde durch ein Signal beendet
    .w-ran-out-of-time = { $check_id } lief aus der Zeit
    .w-printed-more-than-the-host-keeps = { $check_id } gab mehr aus, als der Host behält
    .w-was-stopped-by-the-host = { $check_id } wurde vom Host gestoppt
    .w-the-host-could-not-run = Der Host konnte { $check_id } nicht ausführen

d-bgsmx5zg = Kennzeichen
    .w-a = A
    .w-m = M
    .w-d = D
    .w-r = R
    .w-u = U
    .w-c = C

d-xtveqfbk = Bereichswort
    .w-staged = Index
    .w-not-staged = Arbeitsverzeichnis
    .w-untracked = Unverfolgt

d-bcal6hl7 = Repository-Titel
    .w-git = Git

d-fw76d6i7 = Verfolgung
    .w-up-to-date-with = Auf dem Stand von { $up }
    .w-ahead-of = { $ahead } voraus gegenüber { $up }
    .w-behind = { $behind } zurück gegenüber { $up }
    .w-ahead-behind = { $ahead } voraus, { $behind } zurück gegenüber { $up }
    .w-no-upstream-branch = Kein Upstream-Zweig

d-khr57jl2 = Vor gesagt
    .w-just-now = gerade eben
    .w-min-ago = vor { $value } Min.
    .w-h-ago = vor { $value } Std.
    .w-d-ago = vor { $value } T.

d-lre4z2mt = Festschreiben

d-qmat4wnw = Anlegen

d-u3txosuf = Anlegen

d-464a2hax = Änderungen
    .w-s = s
    .w-o = o
    .w-t = t
    .w-changes = Changes
    .w-git = Git
    .w-refresh = Refresh
    .w-fetch = Fetch
    .w-pull = Pull
    .w-push = Push
    .w-diff-layout = Diff layout
    .w-syntax-highlighting = Syntax highlighting
    .w-show-whitespace = Show whitespace
    .w-wrap-long-lines = Wrap long lines
    .w-stash-changes = Stash changes
    .w-run-approved-project-check = Freigegebene Projektprüfung ausführen
    .w-stage-all = Stage all
    .w-unstage-all = Unstage all
    .w-commit-message = Commit message
    .w-commit = Commit
    .w-commit-to = Commit to { $head }
    .w-your-working-tree-is-clean = Your working tree is clean
    .w-unstage-file = Unstage file
    .w-stage-file = Stage file
    .w-discard-file-changes = Discard file changes
    .w-select-a-file-to-see-its = Select a file to see its changes
    .w-switch-to = Switch to { $pending_switch }?
    .w-you-have-uncommitted-changes-stash-them = You have uncommitted changes. Stash them and switch, or keep them; switching fails when a change would be overwritten.
    .w-stash-and-switch = Stash and switch
    .w-cancel = Cancel
    .w-keep-my-changes = Keep my changes

d-sbvmzuej = Änderungszeile
    .w-unstage = Unstage
    .w-unstage-2 = Unstage { $path }
    .w-stage = Stage
    .w-stage-2 = Stage { $path }

d-f5rw6soc = Änderungsdetail
    .w-renamed-from = Renamed from { $old }
    .w-unstage-file = Unstage file
    .w-stage-file = Stage file
    .w-discard-file = Discard file
    .w-both-sides-changed-this-file = Both sides changed this file
    .w-keeping-ours-drops-lines-of-theirs = Keeping ours drops { $theirs } lines of theirs; keeping theirs drops { $ours } lines of yours.
    .w-take-ours = Take ours
    .w-take-theirs = Take theirs
    .w-unstage-hunk = Unstage hunk
    .w-stage-hunk = Stage hunk
    .w-discard-hunk = Discard hunk

d-ci5fxsvq = Änderungscode
    .w-binary-file = Binary file
    .w-there-is-no-text-to-compare = There is no text to compare.
    .w-no-changes-to-show = No changes to show
    .w-only-the-file-s-permissions-changed = Only the file's permissions changed.

d-x4ftmy7x = Code-Spalte

d-37sjstuv = Öffnungsseite
    .w-open-a-repository = Open a repository
    .w-enter-the-path-of-a-git = Enter the path of a git repository on this computer.
    .w-home-you-project = /home/you/project
    .w-open = Open
    .w-choose = Choose…

d-hqkjgisz = Auswahl
    .w-open-this-folder = Open this folder
    .w-cancel = Cancel

d-m3qefblf = Verlauf
    .w-history = History
    .w-git = Git
    .w-no-commits-yet = No commits yet
    .w-enter = Enter
    .w-backspace = Backspace
    .w-escape = Escape

d-eu2tlikq = Commit-Zeile

d-i4z2l7f7 = Commit-Seite
    .w-commit = Commit
    .w-date = Date
    .w-parents = Parents
    .w-branches-and-tags = Branches and tags
    .w-changed-files = Changed files

d-fxxvyq63 = Geänderte Datei
    .w-binary-file = Binary file
    .w-there-is-no-text-to-compare = There is no text to compare.
    .w-no-changes-to-show = No changes to show
    .w-only-the-file-s-permissions-changed = Only the file's permissions changed.

d-m4t7437c = Zweige
    .w-enter = Enter
    .w-delete = Delete
    .w-branches = Branches
    .w-git = Git
    .w-new-branch-name = New branch name
    .w-create-branch = Create branch
    .w-remote-branches = Remote branches
    .w-working-directories = Working directories
    .w-new-worktree-branch = New worktree branch
    .w-absolute-destination-path = Absolute destination path
    .w-start-point-branch-tag-or-commit = Start point (branch, tag or commit)
    .w-create-worktree = Create worktree
    .w-tags = Tags
    .w-stashes = Stashes
    .w-remotes = Remotes
    .w-fetch = Fetch
    .w-pull = Pull
    .w-push = Push

d-kz5j2n5t = Zweigzeile
    .w-delete = Delete
    .w-delete-2 = Delete { $name }

d-6lsikkhk = Prüfung
    .doc = Die letzte Prüfung, die der Host des Projekts dafür ausgeführt hat; jedes Mitglied sieht sie.

d-3peaa3l5-filter-check = Prüfung

d-vnshbilp = Freigegebene Prüfung
    .doc = Eine Prüfung, die der Host des Projekts freigegeben hat, wie sie zuletzt hier lief: was der Host meldet, während sie läuft, und was er beobachtet hat, als sie endete.
    .noun = { $count ->
        [one] freigegebene Prüfung
       *[other] freigegebene Prüfungen
    }

d-rmw3ex57 = Prüfungs-ID

d-nsdsyb5s = Auftrag
    .doc = Der Auftrag des Geräts, solange sie läuft; sonst leer.

d-ui5llo7z = Stand

d-d5rqal3v = Phase
    .doc = Die neueste Phase, die der Host gemeldet hat, und wie viel davon erledigt ist.

d-poxyr3cm = Erledigt

d-eezoev2x = Gesamt

d-u3i4t7tf = Ergebnis
    .doc = Wie die Befehle endeten, wie der Host es beobachtet hat: `exited`, `cancelled`, `timed_out`, `output_limit` oder `failed`.

d-dl6kn2wc = Exit-Code

d-pllfspge = Dauer in ms

d-vmj5rkri = Ausgabe
    .doc = Der Anfang dessen, was die Befehle ausgegeben haben, und dessen, was sie als Fehler ausgegeben haben.

d-qmhx4rnw = Fehlerausgabe

d-rclt3d5w = Ablehnung
    .doc = Warum sie nicht lief, oder warum der Host sie nicht überwachen konnte.

d-vnshbilp-filter-state = Stand

d-vnshbilp-filter-completed = Erledigt

d-vnshbilp-filter-total = Gesamt

d-vnshbilp-filter-exit_code = Exit-Code

d-vnshbilp-filter-elapsed_ms = Dauer in ms

d-j6kggm43 = Prüfungsstand
    .doc = Wie eine Prüfung steht, wie das Gerät `engine.status` beantwortet: sobald sie fertig ist, was der Host von ihren Befehlen beobachtet hat.
    .noun = { $count ->
        [one] Prüfungsstand
       *[other] Prüfungsstände
    }

d-uygk57xo = Stand

d-55lyz5up = Phase

d-pl44hgyv = Erledigt

d-5qgxxmf2 = Gesamt

d-jdbite5b = Fehler

d-lseynsql = Ergebnis

d-unt7o3nt = Exit-Code

d-oo44f2s2 = Dauer in ms

d-phjhwilm = Ausgabe

d-hmbphto7 = Fehlerausgabe

d-z5xx44zw = Störung

d-j6kggm43-filter-completed = Erledigt

d-j6kggm43-filter-total = Gesamt

d-j6kggm43-filter-exit_code = Exit-Code

d-j6kggm43-filter-elapsed_ms = Dauer in ms

d-ulbv3hii = Prüfungsstand
    .doc = Wo die letzte freigegebene Prüfung steht: noch keine lief (oder sie wurde weggelegt), der Host führt sie gerade aus, der Host hat beobachtet, wie ihre Befehle endeten, sie wurde angehalten, oder der Arbeitsbereich oder der Host hat sie abgelehnt.

d-gayd4ikj = Freigegebene Projektprüfung ausführen
    .doc = Führt eine Prüfung aus, die der Host dieses Projekts freigegeben hat, anhand ihrer ID: Der Host wählt Befehle, Quellen und Grenzen, meldet, wie weit er ist, und sagt, was er beobachtet hat, sobald sie endet.
    .w-a-check-is-already-running = Es läuft bereits eine Prüfung

d-gayd4ikj-check_id = Prüfungs-ID, die genau dieser Projekt-Host freigegeben hat

d-4m6s256h = Prüfung abfragen
    .doc = Fragt das Gerät, wie die laufende Prüfung vorankommt, und behält, was der Host gemeldet hat.

d-ci2kg6kc = Prüfung anhalten
    .doc = Hält die laufende Prüfung an; der Host beendet ihre Befehle.
    .w-no-check-is-running = Es läuft keine Prüfung

d-jehaiy7d = Prüfung wegräumen
    .doc = Legt das Ergebnis der letzten Prüfung weg.

d-suwn2nn2 = Freigegebene Prüfung ausführen~present

d-3eb4vxiy = Prüfung anhalten~present

d-kqs6l2ft = Prüfungskarte
    .w-running = { $check_id } läuft
    .w-waiting-for-the-project-s-host = Warte auf den Host des Projekts
    .w-stop = Anhalten
    .w-check-under-way = Prüfung läuft
    .w-took = Dauerte { $took }
    .w-what-the-check-printed = Was die Prüfung ausgegeben hat
    .w-was-stopped = { $check_id } wurde angehalten
    .w-no-project-s-approved-check-ran = Keine freigegebene Projektprüfung lief

d-7pbhnztg = Bereit

d-jciacddk = Läuft

d-kkgzl4i7 = Beobachtet

d-yj3fdk54 = Angehalten

d-kjqu7kvq = Abgelehnt
