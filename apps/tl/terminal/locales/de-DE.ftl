# Terminal auf Deutsch. Keys sind die stabilen ids der Deklarationen (app.ids); die Wörter der Quelle stehen in den Quellen des Pakets.

d-so-tilo-terminal = Terminal

d-so-tilo-terminal-keywords = terminal, shell, konsole, kommandozeile, eingabeaufforderung, befehlszeile, terminalemulator, terminalfenster, befehle, bash, zsh, fish, ssh, gnome terminal, iterm, windows terminal, xterm, cli, tty, cmd, powershell

d-i3xij4bu = Befehlseintrag
    .doc = Etwas, das die Parser eines Laufs in seiner Ausgabe gefunden haben: eine Meldung, eine Stelle, ein Link, ein Test, eine geänderte Datei.
    .noun = { $count ->
        [one] Befehlseintrag
       *[other] Befehlseinträge
    }

d-3acjc3vs = Art

d-dmweg2lv = Text
    .doc = Was die Ausgabe darüber sagt.

d-cyt46k4w = Pfad
    .doc = Die Datei, in der es steht, oder die Adresse, die ein Link öffnet.

d-wfgzpulj = Zeile

d-p7w5m2xe = Spalte

d-in7by3ic = Code
    .doc = Der Code einer Meldung, der Name eines Tests, der Status einer Änderung.

d-i3xij4bu-filter-kind = Art

d-i3xij4bu-filter-line = Zeile

d-i3xij4bu-filter-column = Spalte

d-tv6nvvs2 = Befehl
    .doc = Ein Befehl, den eine Sitzung ausgeführt hat: was getippt wurde, wo er lief und was er geantwortet hat.
    .noun = { $count ->
        [one] Befehl
       *[other] Befehle
    }

d-hhohupva = Befehl
    .doc = Die Befehlszeile, wie die Shell sie markiert hat.

d-yuimyd26 = Ordner
    .doc = Der Ordner, in dem er lief.

d-63rybmrj = Umgebung
    .doc = Wo er lief: das kodierte Ziel der Sitzung.

d-qpdn55zn = Wer
    .doc = Wer ihn ausgeführt hat: die Person an der Tastatur oder wer ihn mit `run` angefragt hat.

d-u5dzvgkn = Begonnen (ms)

d-mk4tmed6 = Beendet (ms)

d-uekxnfuo = Exit-Status
    .doc = Sein Exit-Status.

d-acfa2avq = Ausgabe
    .doc = Was er ausgegeben hat, wie die Shell der Sitzung es markiert hat: das Ende, wenn er mehr ausgegeben hat.

d-4i2bpiv7 = Gekürzt
    .doc = Er hat mehr ausgegeben, als die Ausgabe aufbewahrt.

d-44426wfp = Werkzeug
    .doc = Das Werkzeug, dessen Parser ihn gelesen hat.

d-kr4ew23e = Rückgängig machbar
    .doc = Er hat etwas geändert, das eine spätere Aktion zurücksetzen kann.

d-iydnwa7n = Einträge
    .doc = Was seine Parser gefunden haben.

d-tv6nvvs2-filter-actor = Wer

d-tv6nvvs2-filter-started_ms = Begonnen (ms)

d-tv6nvvs2-filter-ended_ms = Beendet (ms)

d-tv6nvvs2-filter-exit = Exit-Status

d-tv6nvvs2-filter-truncated = Gekürzt

d-tv6nvvs2-filter-undoable = Rückgängig machbar

d-42uvr5o4 = Gemerkter Befehl
    .doc = Eine Befehlszeile, die ein Terminal ausgeführt hat, im Verlauf des Arbeitsbereichs aufbewahrt, damit Pfeil nach oben sie zurückholt.
    .noun = { $count ->
        [one] gemerkter Befehl
       *[other] gemerkte Befehle
    }

d-xirmgxkc = Befehl

d-q2r65yuo = Ordner

d-qvj4xyes = Umgebung
    .doc = Wo er lief: das kodierte Ziel der Sitzung.

d-nq24rldv = Exit-Status

d-kxqyvf3x = Beendet (ms)

d-i43gac55 = Wer

d-24wxlrux = Sitzung
    .doc = Die Sitzung, die ihn ausgeführt hat, und sein Lauf dort.

d-eckllbcl = Lauf

d-42uvr5o4-filter-exit = Exit-Status

d-42uvr5o4-filter-ended_ms = Beendet (ms)

d-42uvr5o4-filter-actor = Wer

d-vcaayzrt = Terminal
    .doc = Das Terminal des Arbeitsbereichs: jede Befehlszeile, die seine Terminals ausgeführt haben, die Pfeil nach oben zurückholt.
    .noun = { $count ->
        [one] Terminal
       *[other] Terminals
    }
    .untitled = Terminalverlauf

d-ofrwkmpt = Name

d-2zxod2mc = Verlauf
    .doc = Jede in einem Terminal getippte Befehlszeile, älteste zuerst.

d-kz2mzthh = Maschinengeheimnis
    .doc = Ein Tresor-Element, das ein Programm in der Maschine des Arbeitsbereichs bekommt, über die Variable, in der es ihm übergeben wird.
    .noun = { $count ->
        [one] Maschinengeheimnis
       *[other] Maschinengeheimnisse
    }

d-stvocxff = Variable

d-w36d6q3i = Element

d-gffvgtpd = Maschinengeheimnisse
    .doc = Die Tresor-Elemente, die die Maschine des Arbeitsbereichs den Programmen übergibt, die sie startet.
    .noun = { $count ->
        [one] Maschinengeheimnis
       *[other] Maschinengeheimnisse
    }

d-6xrw6sg7 = Geheimnisse

d-thtnx4h6 = Hier
    .doc = Was dieses Gerät darüber weiß, wo seine Terminals laufen, und die Umgebung, die seine Person zuletzt benutzt hat, in der sich ein neues Fenster öffnet.
    .noun = { $count ->
        [one] Gerätestand
       *[other] Gerätestände
    }

d-ijsbkn65 = Verfügbar
    .doc = Das Gerät führt überhaupt Umgebungen aus.

d-wjfwiddm = Zuletzt
    .doc = Die Umgebung, in der sich ein neues Fenster öffnet, kodiert; leer für die Standardumgebung.

d-ninu7wy3 = Gerät
    .doc = Dieses Gerät, wie die Sitzungen es nennen, die es hält: einmal erzeugt, als es zum ersten Mal eine geöffnet hat.

d-thtnx4h6-filter-available = Verfügbar

d-vgkud5dv = Sitzung
    .doc = Ein Terminalfenster: wo seine Shell läuft, die Shell, die es zeigt, und die Befehle, die es ausgeführt hat.
    .noun = { $count ->
        [one] Sitzung
       *[other] Sitzungen
    }
    .untitled = Terminal

d-6ikkeypg = Titel
    .doc = Der Titel des Fensters, wie das Programm ihn gesetzt hat; leer zeigt den der App.

d-fzio5css = Ziel
    .doc = Wo die Sitzung läuft, kodiert: `host`, `environment`, `environment:<record>` oder `linux-vm:<vm>`.

d-ghiasjsf = Relay
    .doc = Die Sitzung auf einem Relay, mit der sich diese wieder verbindet, wenn sie mitgenommen wurde.

d-e32flokn = Bereich
    .doc = Der Bereich, auf den sich die Anwesenheit der Sitzung bezieht.

d-2cbvs73e = Gerät
    .doc = Das Gerät, dessen Fenster die Shell der Sitzung hält.

d-gt55ngxh = Inhaber
    .doc = Wer die Sitzung dort eingerichtet hat.

d-c2b6tu23 = Shell
    .doc = Die laufende Shell auf dem Gerät, unter der ID, die ihre Engine ihr gegeben hat; leer, bis eine geöffnet wurde.

d-7u3sva7z = Shell-Name
    .doc = Das Programm an der Eingabeaufforderung, wie die Shell sich selbst genannt hat.

d-wfh3tsr4 = Zustand
    .doc = Wie es um die Shell steht.

d-xsftvwsn = Problem
    .doc = Warum die Shell nicht starten konnte oder warum ihre Verbindung verloren ging.

d-u3x3ponz = Angehalten
    .doc = Ein Befehl, den der Host der Sitzung anhält, bis jemand handelt, mit dem Grund, den er genannt hat.

d-w4lywnqg = Befehle
    .doc = Die Befehle, die die Sitzung ausgeführt hat, älteste zuerst.

d-vgkud5dv-filter-holder = Inhaber

d-vgkud5dv-filter-state = Zustand

d-42z4xgzf = Öffnungsanfrage
    .noun = { $count ->
        [one] Öffnungsanfrage
       *[other] Öffnungsanfragen
    }

d-fslt6efz = Ziel

d-yud4bccy = Spalten

d-nk6wa4qf = Zeilen

d-jicmwy5t = Kontext

d-h52qdimi = Eintrag

d-ea2cdahy = Verbinden

d-wzf7qzfd = Agent

d-42z4xgzf-filter-columns = Spalten

d-42z4xgzf-filter-rows = Zeilen

d-42z4xgzf-filter-agent = Agent

d-7lfr5p7s = Öffnungsantwort
    .noun = { $count ->
        [one] Öffnungsantwort
       *[other] Öffnungsantworten
    }

d-kfrnmcfl = Shell

d-argszor3 = Shell-Name

d-64xoslvc = Ziel

d-6sjhgtoh = Relay-Sitzung

d-feq55yex = Shell-Anfrage
    .noun = { $count ->
        [one] Shell-Anfrage
       *[other] Shell-Anfragen
    }

d-nflwgdja = Shell

d-v4evdc4j = Text

d-4ukgob2k = Agent

d-feq55yex-filter-agent = Agent

d-dqam7gyi = Editor-Anfrage
    .noun = { $count ->
        [one] Editor-Anfrage
       *[other] Editor-Anfragen
    }

d-b6qxwqfx = Shell

d-nycl2vap = Lease

d-q3pgqijn = Text

d-zm6tpr52 = Prüfantwort
    .noun = { $count ->
        [one] Prüfantwort
       *[other] Prüfantworten
    }

d-sx26ovx5 = Shell

d-br2uknjd = Shell-Name

d-uwsxhnkq = Relay-Sitzung

d-wywmktkr = Beendet

d-zm6tpr52-filter-ended = Beendet

d-yy5prvyc = Bildschirmantwort
    .noun = { $count ->
        [one] Bildschirmantwort
       *[other] Bildschirmantworten
    }

d-wqk5lwvs = Text

d-f752arhx = Gekürzt

d-24ouncbp = Links

d-yy5prvyc-filter-truncated = Gekürzt

d-am6r4jyz = Bildschirmlink
    .noun = { $count ->
        [one] Bildschirmlink
       *[other] Bildschirmlinks
    }

d-in26jp4t = ID

d-vmhd6mh2 = URL

d-am6r4jyz-filter-id = ID

d-43wcjrgj = Beendet
    .doc = Ein Lauf, wie seine Shell ihn markiert hat, so wie die Engine und das Raster des Fensters ihn melden.
    .noun = { $count ->
        [one] beendeter Lauf
       *[other] beendete Läufe
    }

d-4yy4zpjt = Befehl

d-omq4sdo5 = Ordner

d-vdjaeemi = Begonnen (ms)

d-o6udeeg3 = Beendet (ms)

d-wnhsu3fp = Exit-Status

d-fetjzhua = Ausgabe

d-mxlz3peo = Gekürzt

d-ftevx6sn = Werkzeug

d-2nmjhjm6 = Rückgängig machbar

d-e2i4qhpg = Einträge

d-igi43hde = Hintergrundauftrag
    .doc = Der Lauf eines Hintergrundauftrags: niemand hat ihn namentlich angefragt.

d-43wcjrgj-filter-started_ms = Begonnen (ms)

d-43wcjrgj-filter-ended_ms = Beendet (ms)

d-43wcjrgj-filter-exit = Exit-Status

d-43wcjrgj-filter-truncated = Gekürzt

d-43wcjrgj-filter-undoable = Rückgängig machbar

d-43wcjrgj-filter-job = Hintergrundauftrag

d-toaebyny = Ergebniseintrag
    .noun = { $count ->
        [one] Ergebniseintrag
       *[other] Ergebniseinträge
    }

d-tnhfcuru = Art

d-t6c5z2ob = Text

d-67wdeazk = Pfad

d-ua4kljp2 = Zeile

d-rhdzo7gy = Spalte

d-v7vu5okp = Code

d-toaebyny-filter-line = Zeile

d-toaebyny-filter-column = Spalte

d-lubynk22 = Laufdetail
    .doc = Was der Auftrag eines Laufs bei seinem Ende meldet.
    .noun = { $count ->
        [one] Laufdetail
       *[other] Laufdetails
    }

d-634jnzsu = Lauf

d-lubynk22-filter-run = Lauf

d-hlztbqvn = Erneut ausführen
    .noun = { $count ->
        [one] erneuter Lauf
       *[other] erneute Läufe
    }

d-47mkduos = Befehl

d-7vflp2mp = Test

d-jjdvwuu3 = Auflistung
    .noun = { $count ->
        [one] Auflistung
       *[other] Auflistungen
    }

d-aajcuxpo = Verfügbar

d-eioo2vkc = Selbst gehostet

d-jjdvwuu3-filter-available = Verfügbar

d-jjdvwuu3-filter-self_hosted = Selbst gehostet

d-bevorhei = Verlaufsanfrage
    .doc = Was die Shell von ihrem Verlauf verlangt hat, wie das Raster es meldet.
    .noun = { $count ->
        [one] Verlaufsanfrage
       *[other] Verlaufsanfragen
    }

d-3a7cqi35 = Schlüssel

d-bck7dys6 = Überspringen

d-ceicvfy2 = Grenze

d-zvalm6sa = Passend

d-bevorhei-filter-skip = Überspringen

d-bevorhei-filter-limit = Grenze

d-ofifkd7x = Verlaufsantwort
    .noun = { $count ->
        [one] Verlaufsantwort
       *[other] Verlaufsantworten
    }

d-phbmj3ft = Shell

d-ylinc6vw = Schlüssel

d-ju63vxd2 = Überspringen

d-4qotyu56 = Grenze

d-srkjue6c = Passend

d-ynkulvgt = Zeilen
    .doc = Die passenden Zeilen, neueste zuerst; die Engine lässt Wiederholungen weg.

d-ofifkd7x-filter-skip = Überspringen

d-ofifkd7x-filter-limit = Grenze

d-4a4cnd7t = Angehalten
    .doc = Ein Befehl, den der Host einer Sitzung anhält, mit dem Grund, den seine Shell genannt hat.
    .noun = { $count ->
        [one] angehaltener Befehl
       *[other] angehaltene Befehle
    }

d-2x5qp6eh = Titel

d-rpsxhmru = Nachricht

d-7lxnsok4 = URL

d-5wa4qzsc = Agent

d-4a4cnd7t-filter-agent = Agent

d-7eri3fx4 = Sitzungszustand
    .doc = Wie es um die Shell eines Fensters steht.

d-7ihet2d3 = Die Shell wird geöffnet
    .doc = Die Shell wird geöffnet.

d-kcycznbq = Die Shell läuft; was sie ausgibt, steht in ihrem Raster
    .doc = Die Shell läuft; was sie ausgibt, steht in ihrem Raster.

d-j7ky2n2a = Die Sitzung läuft auf einem anderen Gerät oder für ein anderes Mitglied: das Fenster zeigt, was sie
    .doc = Die Sitzung läuft auf einem anderen Gerät oder für ein anderes Mitglied: das Fenster zeigt, was sie aufgezeichnet hat, und startet keine eigene Shell, bis die Person darum bittet.

d-g7qzlbcp = Die Shell konnte nicht starten
    .doc = Die Shell konnte nicht starten.

d-fbdjbrkh = Die Verbindung zur Shell ging verloren; das Fenster öffnet sie erneut
    .doc = Die Verbindung zur Shell ging verloren; das Fenster öffnet sie erneut.

d-bt4a7mpf = Die Shell wurde beendet
    .doc = Die Shell wurde beendet.

d-lqoaodsn = Eintragsart
    .doc = Was etwas ist, das die Parser eines Laufs in seiner Ausgabe gefunden haben.

d-cis7bjwe = Fehler

d-auaak6d7 = Warnung

d-5imnlujc = Stelle

d-4qqfmjsq = URL

d-fmsylk7n = Test bestanden

d-luvlgi7s = Test fehlgeschlagen

d-qv7wbrqs = Test übersprungen

d-6z7tkwg4 = Änderung

d-gcvgmpqb = Verbinden
    .doc = Ein Fenster zu öffnen verbindet es wieder mit seiner Shell: der, die dieses Gerät noch dafür ausführt, oder einer neuen dort, wo die Sitzung läuft.

d-jrwezw47 = Shell öffnen
    .doc = Öffnet eine Shell dort, wo die Sitzung laut eigener Angabe läuft, und zeigt sie im Fenster.
    .w-an-agent-never-opens-the-device = ein Agent öffnet nie die eigene Shell des Geräts

d-ix22dnq4 = Fehlgeschlagen

d-ix22dnq4-why = Grund

d-wuo5e4l4 = Verlief
    .doc = Wie es der Shell des Fensters erging, und warum.

d-wuo5e4l4-state = Zustand

d-wuo5e4l4-why = Grund

d-iksf5dyf = Shell neu starten
    .doc = Startet die Shell im Fenster erneut, dort wo sie läuft: nachdem sie beendet wurde, wenn ihre Verbindung verloren ging, oder um eine Sitzung zu übernehmen, die ein anderes Gerät hält.
    .keywords = neu starten, neustart, zurücksetzen, neu laden, neue shell

d-5a6j3qoo = Shell schließen
    .doc = Schließt die Shell dieser Sitzung auf diesem Gerät.

d-4myrrcmj = Öffnen in…
    .doc = Führt die Sitzung in einer anderen Umgebung aus und öffnet neue Fenster ab jetzt dort.
    .keywords = umgebung, maschine, vm, ziel, verschieben
    .w-choose-where-the-session-runs = wähle, wo die Sitzung läuft
    .w-a-session-runs-on-this-device = eine Sitzung läuft in der eigenen Shell dieses Geräts, in einer Umgebung oder auf einer Linux-VM
    .w-an-agent-never-opens-the-device = ein Agent öffnet nie die eigene Shell des Geräts

d-4myrrcmj-target = Ziel

d-qfeut2wt = Umgebungen auflisten
    .doc = Erfährt, ob dieses Gerät überhaupt Umgebungen ausführt.

d-wlterudp = Ziel aufzeichnen
    .doc = Zeichnet auf, wo die Sitzung läuft, und das Gerät des Fensters, in dem sie eingerichtet wird.
    .w-session-target-is-invalid = das Ziel der Sitzung ist ungültig

d-wlterudp-target = Ziel

d-wlterudp-device = Gerät

d-mzc5e26t = Bereich übernehmen
    .doc = Zeichnet den Bereich auf, auf den sich die Anwesenheit der Sitzung bezieht.
    .w-pane-id-is-invalid = die ID des Bereichs ist ungültig

d-mzc5e26t-pane = Bereich

d-mzc5e26t-device = Gerät

d-vsxray7q = Relay-Sitzung aufzeichnen
    .doc = Zeichnet die Relay-Sitzung auf, mit der sich diese wieder verbindet, oder dass sie sich mit keiner verbindet.
    .w-a-relay-session-s-id-is = die ID einer Relay-Sitzung hat 32 Hex-Zeichen

d-vsxray7q-session = Sitzung

d-amqrsyv5 = Befehl ausführen
    .doc = Führt eine Befehlszeile in der Shell der Sitzung an ihrer Eingabeaufforderung aus und antwortet, sobald ihre Shell sie als beendet markiert hat.
    .keywords = ausführen, befehl, tippen, shell
    .w-a-command-line-of-1-to = eine Befehlszeile von 1 bis 4096 Bytes
    .w-one-command-line = eine einzige Befehlszeile
    .w-commands-are-not-run-in-the = in der eigenen Shell des Geräts werden für niemanden Befehle ausgeführt

d-amqrsyv5-command = Befehl

d-wenubu3w = In der Shell ausführen
    .doc = Führt eine Befehlszeile in der offenen Shell der Sitzung aus.
    .w-the-session-s-shell-did-not = die Shell der Sitzung hat sich nicht geöffnet: { $trouble }
    .w-the-session-s-shell-runs-on = die Shell der Sitzung läuft auf dem Gerät, das sie hält

d-wenubu3w-command = Befehl

d-r5gk4tnw = Gelaufen
    .doc = Der Lauf, der aus einer Befehlszeile wurde, aufgezeichnet, sobald ihre Shell ihn als beendet markiert hat.

d-r5gk4tnw-j = Auftrag

d-qcpeddxu = Erneut ausführen
    .doc = Führt den Befehl eines früheren Laufs erneut aus.
    .keywords = wiederholen, erneut, nochmal

d-qcpeddxu-prior = Früherer Lauf

d-tqdlijuq = Diesen Test erneut ausführen
    .doc = Führt einen Test eines früheren Laufs erneut aus, so wie sein Werkzeug einen einzelnen Test ausführt.
    .keywords = test, wiederholen, fehlgeschlagen, einzelner test
    .w-the-command-reported-no-such-test = der Befehl hat keinen solchen Test gemeldet

d-tqdlijuq-prior = Früherer Lauf

d-tqdlijuq-test = Test

d-oct52uah = Befehl stoppen
    .doc = Stoppt das Programm im Vordergrund, wie Strg+C.
    .keywords = abbrechen, unterbrechen, strg+c, beenden, kill
    .w-the-session-s-shell-runs-on = die Shell der Sitzung läuft auf dem Gerät, das sie hält

d-6adfyuih = Text tippen
    .doc = Tippt Text an der Eingabeaufforderung, ohne Enter zu drücken.
    .w-1-to-8192-bytes-of-text = 1 bis 8192 Bytes Text
    .w-text-without-enter = Text ohne Enter
    .w-this-window-s-shell-is-not = die Shell dieses Fensters läuft nicht
    .w-an-agent-never-types-into-the = ein Agent tippt nie in die eigene Shell des Geräts

d-6adfyuih-text = Text

d-uv3msar4 = Einfügen
    .doc = Fügt Text genau so in die Shell ein, wie er übergeben wurde. Jeder außer der Person am Fenster wird vorher gefragt: was an der Eingabeaufforderung landet, wird ausgeführt.
    .keywords = einfügen, zwischenablage
    .w-at-most-64-kib-at-once = höchstens 64 KiB auf einmal
    .w-this-window-s-shell-is-not = die Shell dieses Fensters läuft nicht
    .w-an-agent-never-types-into-the = ein Agent tippt nie in die eigene Shell des Geräts

d-uv3msar4-text = Text

d-3oo6gydn = Zwischenablage einfügen
    .doc = Fügt ein, was die Zwischenablage enthält: die Person am Fenster, also wird nichts vorher gefragt.

d-36zgyxqm = Bildschirm leeren
    .doc = Leert den Bildschirm und seinen Verlauf.
    .keywords = leeren, löschen, zurücksetzen, cls, verlauf
    .w-this-window-s-shell-is-not = die Shell dieses Fensters läuft nicht

d-kfsbvjbt = Sichtbare Ausgabe kopieren
    .doc = Kopiert, was das Fenster zeigt, ohne Escape-Sequenzen.
    .keywords = kopieren, ausgabe, bildschirm, text
    .w-nothing-to-copy = Nichts zu kopieren
    .w-copied = Kopiert

d-77jih4ro = Auswahl kopieren
    .doc = Kopiert die Wörter, die eine Auswahl umfasst, so wie das Raster sie übergibt.
    .keywords = kopieren, auswahl
    .w-select-text-on-the-grid-first = Wähle zuerst Text im Raster aus
    .w-copied = Kopiert

d-77jih4ro-text = Text

d-mt6yoax4 = Gesamte Ausgabe auswählen
    .doc = Wählt alles aus, was die Sitzung ausgegeben hat, und kopiert es: die ganze Ausgabe des Rasters.
    .keywords = auswählen, alles, gesamt

d-3tvkwhr4 = Zum Anfang
    .doc = Die älteste Ausgabe, die die Sitzung aufbewahrt, und die neueste, die sie zeigt, als Text.
    .keywords = scrollen, anfang, beginn, älteste

d-ndrzvnm4 = Zur neuesten Ausgabe
    .keywords = scrollen, ende, unten, live

d-k2wrcabm = Ausgabe lesen
    .doc = Was das Fenster zeigt: seine neueste Ausgabe als Text und die öffentlichen Links darin.
    .keywords = lesen, bildschirm, ausgabe
    .w-this-window-s-shell-is-not = die Shell dieses Fensters läuft nicht

d-ovqs6tkm = Editor prüfen
    .doc = Der Editor, der in der Sitzung läuft und seinen Text anbietet, falls einer es tut: die Lease, unter der seine Auswahl ersetzt wird, und ob er rückgängig machen oder wiederholen kann.
    .w-this-window-s-shell-is-not = die Shell dieses Fensters läuft nicht

d-5xzfkdvy = Editorauswahl ersetzen
    .doc = Ersetzt die Auswahl des Editors, der in der Sitzung läuft, unter der Lease, die er vergeben hat, statt in der Shell zu tippen.
    .w-this-window-s-shell-is-not = die Shell dieses Fensters läuft nicht
    .w-an-agent-never-types-into-the = ein Agent tippt nie in die eigene Shell des Geräts

d-5xzfkdvy-lease = Lease

d-5xzfkdvy-text = Text

d-fqliglti = Sitzung prüfen
    .doc = Wie es um die Sitzung steht: wo sie läuft, ob ihre Shell lebt und warum nicht.

d-xenqzrnh = Link öffnen
    .doc = Öffnet eine öffentliche Webadresse, die die Sitzung ausgegeben hat, im Browser.
    .keywords = url, browser, web, adresse
    .w-only-a-plain-web-address-opens = nur eine einfache Webadresse öffnet sich

d-xenqzrnh-url = URL

d-c6b2lxuq = Öffnen
    .doc = Öffnet etwas, das die Parser eines Laufs gefunden haben: einen Link im Browser, eine Datei in der App, die sie öffnet.
    .w-no-app-opens = Keine App öffnet { $path }

d-c6b2lxuq-prior = Früherer Lauf

d-c6b2lxuq-position = Position

d-52n4fgbt = Beendet
    .doc = Ein Lauf, den die Shell als beendet markiert hat, wie das Raster ihn gemeldet hat: die Person hat ihn getippt.

d-52n4fgbt-said = Gemeldet

d-oxoopdwb = Aufzeichnen
    .doc = Zeichnet einen Lauf in der Sitzung auf und im Verlauf des Arbeitsbereichs, wenn eine Person ihn getippt hat.

d-oxoopdwb-run = Lauf

d-oxoopdwb-id = ID

d-i7oueaex = Befehl aufzeichnen
    .doc = Zeichnet einen Befehl auf, der in der Sitzung endete, wo ihre Shell für jemanden läuft, der ihn mit `run` angefragt hat (einen Agenten, ein Skript, ein anderes Gerät), unter der ID, die sein Aufrufer ihm gab.
    .w-a-command-line = eine Befehlszeile
    .w-a-command-takes-an-id-of = ein Befehl bekommt eine eigene ID

d-i7oueaex-command = Befehl

d-i7oueaex-cwd = Ordner

d-i7oueaex-started_ms = Begonnen (ms)

d-i7oueaex-ended_ms = Beendet (ms)

d-i7oueaex-exit = Exit-Status

d-i7oueaex-output = Ausgabe

d-i7oueaex-truncated = Gekürzt

d-i7oueaex-tool = Werkzeug

d-i7oueaex-undoable = Rückgängig machbar

d-i7oueaex-items = Einträge

d-i7oueaex-request = Anfrage

d-i7oueaex-id = ID

d-4pfe6ato = Titel gesetzt
    .doc = Das Programm hat den Titel des Fensters gesetzt.

d-4pfe6ato-title = Titel

d-howgwhk3 = Beendet
    .doc = Die Shell wurde beendet: Die Sitzung endet, und das Fenster, das sie zeigte, schließt sich.

d-howgwhk3-how = Wie

d-pblr2uei = Verbindung verloren
    .doc = Die Verbindung zur Shell ging verloren: Das Fenster sagt es und öffnet sie wieder, verbunden mit der Sitzung, wo sie noch läuft.

d-pblr2uei-why = Grund

d-ghzu2uci = Angehalten
    .doc = Der Host der Sitzung hält einen Befehl an, bis jemand handelt, und sagt warum; leerer Text sagt, dass er ihn losgelassen hat.

d-ghzu2uci-said = Gemeldet

d-wms53lje = Loslassen
    .doc = Die Person hat den angehaltenen Befehl losgelassen: er wird an der Eingabeaufforderung abgebrochen.

d-veiz3dom = Karte hinzufügen
    .doc = Öffnet die Seite, auf der die Person die Karte hinzufügt, auf die der angehaltene Befehl wartet.
    .w-the-card-is-added-on-a = die Karte wird auf einer sicheren Seite hinzugefügt

d-veiz3dom-url = URL

d-6275qknr = Zurückholen
    .doc = Die Shell hat nach den Zeilen gefragt, die sie bei Pfeil nach oben zurückholt: die neuesten aus dem Verlauf der Person, die das Getippte enthalten, jede einmal.

d-6275qknr-asked = Angefragt

d-5ov34juh = Merken
    .doc = Bewahrt eine Befehlszeile im Verlauf des Arbeitsbereichs auf, damit Pfeil nach oben sie zurückholt.
    .w-a-command-line = eine Befehlszeile

d-5ov34juh-command = Befehl

d-5ov34juh-cwd = Ordner

d-5ov34juh-exit = Exit-Status

d-5ov34juh-ended_ms = Beendet (ms)

d-5ov34juh-session = Sitzung

d-5ov34juh-run = Lauf

d-5ov34juh-environment = Umgebung

d-5ov34juh-actor = Wer

d-ktoqle45 = Lauf importieren
    .doc = Bewahrt einen Lauf, den ein anderes Gerät aufgezeichnet hat, einmal im Verlauf auf: ihn erneut zu importieren bewahrt nichts.

d-ktoqle45-command = Befehl

d-ktoqle45-cwd = Ordner

d-ktoqle45-exit = Exit-Status

d-ktoqle45-ended_ms = Beendet (ms)

d-ktoqle45-session = Sitzung

d-ktoqle45-run = Lauf

d-ktoqle45-environment = Umgebung

d-ktoqle45-actor = Wer

d-fcmeehdw = Geheimnis hinzufügen
    .doc = Übergibt jedem Programm, das die Maschine des Arbeitsbereichs startet, ein Tresor-Element in `variable`.
    .keywords = geheimnis, tresor, umgebungsvariable, token, schlüssel
    .w-a-variable-is-letters-digits-and = eine Variable besteht aus Buchstaben, Ziffern und _, beginnt nicht mit einer Ziffer und hat höchstens 128 Zeichen
    .w-the-machine-s-own-software-reads = die eigene Software der Maschine liest diese Variable
    .w-a-vault-item-is-named-by = ein Tresor-Element wird über seine Referenz benannt, vault.item:<id>

d-fcmeehdw-variable = Variable

d-fcmeehdw-item = Element

d-tc6qi4e3 = Geheimnis entfernen
    .doc = Übergibt den Programmen der Maschine eine Variable nicht mehr.
    .w-no-secret-is-handed-to-the = der Maschine wird in { $variable } kein Geheimnis übergeben

d-tc6qi4e3-variable = Variable

d-kl7nqibh = Umgebung erstellen
    .doc = Erstellt eine neue Umgebung des Arbeitsbereichs namens `name`.
    .keywords = umgebung, neu, erstellen, anlegen
    .w-name-the-new-environment = Gib der neuen Umgebung einen Namen

d-kl7nqibh-name = Name

d-e2gpzen6 = Umgebung umbenennen
    .doc = Benennt die Umgebung `record` in `name` um.
    .w-name-the-environment = Gib der Umgebung einen Namen

d-e2gpzen6-record = Eintrag

d-e2gpzen6-name = Name

d-a4ncrc6w = Einer Umgebung einen Ordner freigeben
    .doc = Lässt die Umgebung `record` den Ordner unter `path` auf diesem Gerät erreichen und, wenn `write`, seinen Inhalt ändern.
    .w-say-which-folder = Gib an, welcher Ordner

d-a4ncrc6w-record = Eintrag

d-a4ncrc6w-path = Pfad

d-a4ncrc6w-write = Schreiben

d-55uvowt4 = Befehl kopieren
    .doc = Kopiert eine Befehlszeile erneut.
    .w-copied = Kopiert

d-mmuhjvj5 = Zielname
    .w-your-shell-on-this-device = Deine Shell auf diesem Gerät
    .w-virtual-machine = Virtuelle Maschine { $vm }
    .w-default-environment = Standardumgebung

d-75k3ipz5 = Werkzeugzeile
    .w-no-tools-yet-run-pkg-install = Noch keine Werkzeuge. Führe pkg install in der Umgebung aus.
    .w-tools = Werkzeuge: { $join }

d-uzfmws7p = Ordnerzeile
    .w-no-folders-yet-use-grant-folder = Noch keine Ordner. Nutze „Ordner freigeben…“ oder führe perm trust-dir in der Umgebung aus.
    .w-folders = Ordner: { $join }

d-h4kbiesu = Öffnungszeile
    .w-opening-your-shell-on-this-device = Deine Shell auf diesem Gerät wird geöffnet…
    .w-opening-the-shell-where-this-workspace = Die Shell wird dort geöffnet, wo dieser Arbeitsbereich läuft…

d-nbkl6r5x = Befehlszeile
    .w-done = Fertig
    .w-exit = Exit { $exit }
    .w-output-cut = , Ausgabe gekürzt
    .w-in = { $status }{ $cut }, in { $cwd }

d-pcvdfhkg = Shell öffnen~present

d-arwxz2tt = Öffnen in~present

d-tqfbpxlu = Ziel aufzeichnen~present

d-i2foisay = Bereich übernehmen~present

d-roddim42 = Relay-Sitzung aufzeichnen~present

d-uci5hdxk = Ausführen~present

d-e5t66gme = In der Shell ausführen~present

d-umxbrwyj = Test wiederholen~present

d-c5ajagak = Stoppen~present

d-mxsl5gck = Senden~present

d-vpsbvb4p = Einfügen~present

d-2k5exx6m = Leeren~present

d-qenw4hw5 = Bildschirm lesen~present

d-m2eoannq = Editor prüfen~present

d-2v54vs45 = Editorauswahl ersetzen~present

d-x32t27ry = Link öffnen~present

d-xwyw3ibu = Befehl aufzeichnen~present

d-lmrwcgrd = Karte hinzufügen~present

d-dtbgc3fa = Merken~present

d-4rd4rvle = Geheimnis hinzufügen~present

d-h7xxzw6i = Geheimnis entfernen~present

d-rwjtt7ml = Umgebung erstellen~present

d-5pyfjkze = Umgebung umbenennen~present

d-6atj2i2j = Ordner freigeben~present

d-ulzedoij = Sitzungsfenster
    .w-terminal = Terminal

d-6istfnoc = Zielmenü
    .w-default-environment = Standardumgebung
    .w-your-shell-on-this-device = Deine Shell auf diesem Gerät
    .w-manage-environments = Umgebungen verwalten…
    .w-environments = Umgebungen
    .w-grant = Freigeben
    .w-rename = Umbenennen
    .w-create = Erstellen
    .w-close = Schließen
    .w-no-environments-yet = Noch keine Umgebungen
    .w-name-one-below-to-make-it = Gib unten einen Namen ein, um eine zu erstellen.
    .w-open-here = Hier öffnen
    .w-grant-folder = Ordner freigeben…
    .w-the-folder-s-path-on-this = Der Pfad des Ordners auf diesem Gerät
    .w-programs-may-change-what-is-in = Programme dürfen seinen Inhalt ändern
    .w-new-name = Neuer Name
    .w-name-of-a-new-environment = Name einer neuen Umgebung

d-szpmlk42 = Sitzungsinhalt
    .w-reconnecting = Verbindung wird wiederhergestellt…
    .w-restart-shell = Shell neu starten
    .w-this-session-runs-on-another-device = Diese Sitzung läuft auf einem anderen Gerät
    .w-its-commands-are-shown-here-restart = Die Befehle dieser Sitzung werden hier gezeigt. Starte die Shell neu, um sie auf diesem Gerät zu übernehmen.
    .w-the-shell-couldn-t-start = Die Shell konnte nicht starten
    .w-try-again = Erneut versuchen
    .w-the-shell-exited = Die Shell wurde beendet
    .w-copy = Kopieren
    .w-paste = Einfügen
    .w-select-all-output = Gesamte Ausgabe auswählen
    .w-clear-screen = Bildschirm leeren
    .w-terminal = Terminal, { $target_name }
    .w-copy-on-select = Beim Auswählen kopieren
    .w-flash-on-a-bell = Bei einem Klingelton aufblinken
    .w-let-programs-set-the-clipboard = Programme die Zwischenablage setzen lassen
    .w-no-shell-running = Keine Shell läuft

d-2ejm6m4e = Hinweis zum angehaltenen Befehl
    .w-this-needs-a-linux-machine = Dafür ist eine Linux-Maschine nötig.
    .w-add-a-card-to-continue = Füge eine Karte hinzu, um fortzufahren.
    .w-add-a-card = Karte hinzufügen
    .w-cancel = Abbrechen
    .w-this-command-waits = Dieser Befehl wartet

d-jrudigsy = Befehlsliste
    .w-no-commands-yet = Noch keine Befehle
    .w-commands-this-session-runs-show-here = Befehle, die diese Sitzung ausführt, erscheinen hier, mit dem, was jeder ausgegeben hat.

d-p23env37 = Befehlszeile
    .w-run-again = Erneut ausführen
    .w-copy-command = Befehl kopieren
    .w-run-this-test-again = Diesen Test erneut ausführen

d-thaaij7x = Terminal-Übersicht
    .w-no-commands-yet = Noch keine Befehle
    .w-commands-run-in-the-workspace-s = Befehle, die in den Terminals des Arbeitsbereichs laufen, erscheinen hier.
    .w-exit-in = Exit { $exit }, in { $cwd }

d-ucew7fdw = Geheimnisseite
    .w-no-secrets-for-the-machine = Keine Geheimnisse für die Maschine
    .w-a-vault-item-added-here-is = Ein hier hinzugefügtes Tresor-Element wird jedem Programm übergeben, das die Maschine des Arbeitsbereichs startet, in der Variable, die du nennst.

d-nbjnpcjn = Lauf
    .doc = Die Kennung, unter der sein Aufrufer ihn aufgezeichnet hat, wenn einer es tat: Ein erneut aufgezeichneter Lauf ist derselbe.

d-so-tilo-terminal-Preferences = Terminal-Einstellungen
    .doc = Was die Person für ihre Terminals festlegt, auf jedem ihrer Geräte.
    .noun = { $count ->
        [one] Terminal-Einstellung
       *[other] Terminal-Einstellungen
    }

d-g7naqad5 = Beim Auswählen kopieren
    .doc = Eine abgeschlossene Auswahl wird auch in die Zwischenablage kopiert, nicht nur in die primäre Auswahl.

d-hvjs5osb = Bei einem Klingelton aufblinken
    .doc = Das Raster blinkt auf, wenn ein Programm den Klingelton auslöst.

d-gmjhrfhb = Programme die Zwischenablage setzen lassen
    .doc = Ein Programm darf Text in die Zwischenablage legen (OSC 52). Aus, bis die Person es erlaubt: sonst könnte alles, was im Terminal ausgegeben wird, auch ein entfernter Rechner, ersetzen, was sie kopiert hat.

d-so-tilo-terminal-Preferences-filter-copy_on_select = Beim Auswählen kopieren

d-so-tilo-terminal-Preferences-filter-bell_flash = Bei einem Klingelton aufblinken

d-so-tilo-terminal-Preferences-filter-allow_osc52_clipboard = Programme die Zwischenablage setzen lassen
