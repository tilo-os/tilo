# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-chip-design = Chip-Entwurf

d-so-tilo-chip-design-keywords = Chip, Chip-Entwurf, Chip-Design, Operator, Operatorgraph, Hardware, Prüfung, Bericht, Nachweis, RTL, Netzliste, Silizium, chip-entwurf, halbleiter, nachweis

d-xa35avfl = Port
    .doc = Ein typisierter Port eines Operators.
    .noun = { $count ->
        [one] Port
       *[other] Ports
    }

d-esvx2n72 = Schlüssel
    .doc = Seine Identität an seinem Operator: `value`, `weights`, `result`, `left` oder `right`.

d-vjm4mdax = Datentyp
    .doc = Der Tensortyp, den er führt.

d-ixbog3pv = Operator
    .doc = Ein Operator der strukturellen Gestaltung.
    .noun = { $count ->
        [one] Operator
       *[other] Operatoren
    }

d-wxkbwcn5 = Knoten
    .doc = Seine feste Identität.

d-e6aj4t7l = Art

d-nbs52ikt = Form
    .doc = Die F32-Tensorform; ihre letzte Achse ist die Normalisierungsbreite.

d-w6rehfmk = Normalisierungsbreite

d-gonrdrls = Ausgangsbreite
    .doc = Die Ausgangsbreite der strukturellen Matrix, wenn der Operator eine Matrix hat.

d-u3ws6vjg = Hat Ausgangsbreite

d-nreyvv4r = Formtext
    .doc = Was eine Person für die Form eingetippt hat; `apply_shape` wendet es an.

d-sjbeclxu = Breitentext
    .doc = Was eine Person für die Matrix-Ausgangsbreite eingetippt hat; `apply_width` wendet es an.

d-s4l6n47t = X

d-pgjl7vwe = Y

d-neb6c6b5 = Eingänge

d-eb6j7t6x = Ausgänge

d-o4fzd6at = Verbindung
    .doc = Eine Verbindung zwischen zwei Ports von Operatoren.
    .noun = { $count ->
        [one] Verbindung
       *[other] Verbindungen
    }

d-sqqjxtyw = Von Knoten

d-hk3nk2x5 = Von Port

d-xgc3rbc2 = Zu Knoten

d-hxqnvwsp = Zu Port

d-3tc244az = Kennwert
    .doc = Die Inhaltsidentität der Verbindung.

d-o2gagtnb = Aktiv
    .doc = Ob die gespeicherte Gestaltung sie noch verbindet.

d-qkwwqrxp = Grund
    .doc = Warum nicht, wenn nicht.

d-o4fzd6at-filter-active = Aktiv

d-hayxmj5x = Annahmen
    .doc = Die Speicherannahmen, die ein Bericht angibt.
    .noun = { $count ->
        [one] Annahme
       *[other] Annahmen
    }

d-run6wd4l = Logische Parameter

d-smx2rwim = Gewichtsbits

d-kclrpitf = Cache-Bits

d-6r63anpq = Kontext-Token

d-hayxmj5x-filter-logical_parameters = Logische Parameter

d-hayxmj5x-filter-weight_bits = Gewichtsbits

d-hayxmj5x-filter-kv_bits = Cache-Bits

d-hayxmj5x-filter-context_tokens = Kontext-Token

d-mwnbuncs = Bericht
    .doc = Ein analytischer Speicherbericht, wie importiert und wie eine Person ihn liest.
    .noun = { $count ->
        [one] Bericht
       *[other] Berichte
    }

d-twijrbwn = Kennwert
    .doc = Die Inhaltsidentität der importierten Bytes.

d-7rep5j6b = Quelle
    .doc = Der genaue importierte Bericht, wie der Hersteller ihn schrieb.

d-hd2g3vdr = Modell
    .doc = Das Modell-Repository, um das der Bericht geht.

d-fxx5zcjz = Schemaversion

d-svry2b7t = Revision

d-xyjuenxt = Konfiguration SHA-256

d-vyv4me7w = Nachweis

d-b3g6lqvm = Batch-Größe

d-5qj7upc2 = Annahmen

d-42kdgwvs = Gepackte Gewichtsbytes

d-7jvkir3f = Cache

d-qw2v536a = Grund des Caches

d-ogp5wc76 = Aufmerksamkeitsschichten

d-he3itm4b = Cache-Elemente

d-biifjz5l = Cache-Bytes

d-n3tcruqh = Cache-Ausschlüsse

d-mrp773gq = Nicht geschätzt

d-oypgaywq = Grenzen

d-gdfhr7j4 = Prüfung
    .doc = Eine gemeldete Prüfung.
    .noun = { $count ->
        [one] Prüfung
       *[other] Prüfungen
    }

d-i2clfcou = Name

d-arunp5tg = Ergebnis

d-gdfhr7j4-filter-result = Ergebnis

d-wv7oxuhd = Werkzeug
    .doc = Eine Werkzeugidentität, die ein Beleg meldet.
    .noun = { $count ->
        [one] Werkzeug
       *[other] Werkzeuge
    }

d-6l36tmym = Name

d-ptuif53v = Version

d-6ffjwxk2 = Ausführbare Datei SHA-256

d-kkcqj2np = Artefakt
    .doc = Eine Artefaktidentität, die ein Beleg meldet.
    .noun = { $count ->
        [one] Artefakt
       *[other] Artefakte
    }

d-4d7avciq = Name

d-nwa3vepp = SHA-256

d-umlywiy3 = Kennwert
    .doc = Ein benannter Kennwert, den ein Beleg meldet.
    .noun = { $count ->
        [one] Kennwert
       *[other] Kennwerte
    }

d-r4lv3eu7 = Name

d-pn5tgrsn = SHA-256

d-w7znau2a = Zellenzahl
    .doc = Eine Zählung generischer Zellen, die ein Superblock-Beleg meldet.
    .noun = { $count ->
        [one] Zellenzahl
       *[other] Zellenzahlen
    }

d-r4l4iqx2 = Name

d-vxrpbdeb = Anzahl

d-w7znau2a-filter-count = Anzahl

d-hmpy65v2 = Beobachtung
    .doc = Eine numerische Beobachtung eines CPU-Operator-Belegs.
    .noun = { $count ->
        [one] Beobachtung
       *[other] Beobachtungen
    }

d-l3my4c7u = Bezeichnung
    .doc = Was die Beobachtung verglichen hat, in Worten.

d-boatafil = Art

d-jbgmt4ie = Vergleich
    .doc = Was der Vergleich behauptet.

d-2m7xr7oi = Verglichene Wörter

d-765n5dvn = Abweichende Wörter

d-r2sgywxz = Ausgabe SHA-256

d-2mvyzxqy = Ergebnis

d-jqiqarxm = Abgelehnte Eingaben

d-7g5ljuf2 = Zyklen

d-tkyyux4t = Übernommene Eingaben

d-byvm4brz = Übernommene Ausgaben

d-ysqpkcxy = Logikzellen

d-fmeekkru = Logische Speicherbits

d-fswivmpb = Bibliothek

d-e5eeqpa7 = Taktvorgabe (ns)

d-orrpic3k = Setup-Reserve (ns)

d-pbdqj47x = Hold-Reserve (ns)

d-j7qjxpgt = Verlegte Signalnetze

d-js7iwiwx = Router-Verstöße

d-bsogreds = Bibliotheksfläche (µm²)

d-62qim76q = Profil
    .doc = Das RMS-Profil, über das dieser Vergleich lief.

d-vdf52bkl = FP32-Vergleiche

d-qvzkneov = Eingaben insgesamt
    .doc = Die endlichen SiLU-Eingaben und ihre Abweichungen der Exponentialfunktion.

d-sofq7z7n = exp-Abweichungen

d-rgsgq4i3 = Höchstes exp-ULP

d-fwsjrjk3 = Köpfe
    .doc = Die Form des Gated-Delta und die verglichenen Wörter.

d-shn33gs4 = Token

d-fn6veddp = Breite

d-d3y6qjsu = Aufmerksamkeitswörter

d-rfak7s5n = Zustandswörter

d-3mbqqhmz = Name
    .doc = Die maskierten Softmax-Eingaben und ihre begrenzten Abweichungen.

d-hif4kqoj = Maskiert

d-3ijpmjgv = Höchstes ULP

d-hmpy65v2-filter-kind = Art

d-hmpy65v2-filter-comparison = Vergleich

d-hmpy65v2-filter-compared_words = Verglichene Wörter

d-hmpy65v2-filter-differing_words = Abweichende Wörter

d-hmpy65v2-filter-outcome = Ergebnis

d-hmpy65v2-filter-refused_inputs = Abgelehnte Eingaben

d-hmpy65v2-filter-cycles = Zyklen

d-hmpy65v2-filter-accepted_inputs = Übernommene Eingaben

d-hmpy65v2-filter-accepted_outputs = Übernommene Ausgaben

d-hmpy65v2-filter-logic_cells = Logikzellen

d-hmpy65v2-filter-logical_memory_bits = Logische Speicherbits

d-hmpy65v2-filter-routed_signal_nets = Verlegte Signalnetze

d-hmpy65v2-filter-router_violations = Router-Verstöße

d-hmpy65v2-filter-fp32_comparisons = FP32-Vergleiche

d-hmpy65v2-filter-total_inputs = Eingaben insgesamt

d-hmpy65v2-filter-exp_differences = exp-Abweichungen

d-hmpy65v2-filter-max_exp_ulp = Höchstes exp-ULP

d-hmpy65v2-filter-heads = Köpfe

d-hmpy65v2-filter-tokens = Token

d-hmpy65v2-filter-width = Breite

d-hmpy65v2-filter-attention_words = Aufmerksamkeitswörter

d-hmpy65v2-filter-state_words = Zustandswörter

d-hmpy65v2-filter-masked = Maskiert

d-hmpy65v2-filter-max_ulp = Höchstes ULP

d-yxlbi32g = Speicher
    .doc = Ein logischer Speicher, den eine gemeldete Synthese beobachtet hat.
    .noun = { $count ->
        [one] Speicher
       *[other] Speicher
    }

d-imhcxbpl = Name

d-eifw7p4n = Wortbreite (Bits)

d-474at3uz = Tiefe (Wörter)

d-o76j7m2f = Bits

d-2aqawzb3 = Leseports

d-he5exqe2 = Getaktete Leseports

d-6nk642cp = Schreibports

d-hovn72p4 = Vollständig initialisiert

d-yxlbi32g-filter-width_bits = Wortbreite (Bits)

d-yxlbi32g-filter-depth_words = Tiefe (Wörter)

d-yxlbi32g-filter-bits = Bits

d-yxlbi32g-filter-read_ports = Leseports

d-yxlbi32g-filter-clocked_read_ports = Getaktete Leseports

d-yxlbi32g-filter-write_ports = Schreibports

d-yxlbi32g-filter-fully_initialized = Vollständig initialisiert

d-bjljl2tc = Variante
    .doc = Eine Variante eines gemeldeten Architekturvergleichs.
    .noun = { $count ->
        [one] Variante
       *[other] Varianten
    }

d-zl5mvubo = Identität

d-4y2sweqe = Numerisch
    .doc = Der numerische Vergleich, wenn die Stufe einen meldet.

d-qe2ijn52 = Kaltzyklen der Programmierung

d-lvk3uaeb = Zyklen zum Laden der Vektoren

d-pm72baqp = Warme Zyklen

d-bctwv4iq = Übernommene Gewichtsbytes

d-sbhcuyji = Übernommene Deskriptor-Bytes

d-lrpqhjtv = Cache-Lesezugriffe

d-chkz2y4j = Quantisierer-Pakete

d-f7om2r4s = Vergleichswörter

d-5p4i7krn = Ausgabe SHA-256

d-vte7iosv = Synthese
    .doc = Die generische Synthese, die diese Variante beobachtet hat, wenn die Stufe eine meldet.

d-z3aiymm6 = Generische Logikzellen

d-wsvyc46d = Bits des Aktivierungs-Caches

d-ouotgooh = Programmierte Gewichtsbits

d-htrm4etw = Initialisierte feste Bits

d-s3urh6zt = Speicher

d-bjljl2tc-filter-numerical = Numerisch

d-bjljl2tc-filter-cold_program_cycles = Kaltzyklen der Programmierung

d-bjljl2tc-filter-vector_load_cycles = Zyklen zum Laden der Vektoren

d-bjljl2tc-filter-warm_cycles = Warme Zyklen

d-bjljl2tc-filter-accepted_weight_bytes = Übernommene Gewichtsbytes

d-bjljl2tc-filter-accepted_descriptor_bytes = Übernommene Deskriptor-Bytes

d-bjljl2tc-filter-cache_reads = Cache-Lesezugriffe

d-bjljl2tc-filter-quantizer_packets = Quantisierer-Pakete

d-bjljl2tc-filter-comparison_words = Vergleichswörter

d-bjljl2tc-filter-synthesis = Synthese

d-bjljl2tc-filter-generic_logic_cells = Generische Logikzellen

d-bjljl2tc-filter-activation_cache_bits = Bits des Aktivierungs-Caches

d-bjljl2tc-filter-programmed_weight_bits = Programmierte Gewichtsbits

d-bjljl2tc-filter-fixed_initialized_bits = Initialisierte feste Bits

d-urioql56 = Stufenverknüpfung
    .doc = Eine Stufenverknüpfung eines gemeldeten Architekturvergleichs.
    .noun = { $count ->
        [one] Stufenverknüpfung
       *[other] Stufenverknüpfungen
    }

d-2bpp44k7 = Name

d-cphzthhu = Beleg SHA-256

d-vnv3rjs3 = Hersteller SHA-256

d-u5cz3l65 = Eingabeidentität SHA-256

d-ceqqtftx = Stufe
    .doc = Eine Stufe eines gemeldeten Nachweisfusses.
    .noun = { $count ->
        [one] Stufe
       *[other] Stufen
    }

d-4qfzbkq5 = Schlüssel
    .doc = Die Identität, die der Hersteller der Stufe gibt.

d-htbbatar = Titel

d-wyge75zm = Status

d-uglh3san = Originalbeleg-Artefakt

d-46xvj5o3 = Hersteller SHA-256

d-775i2e3d = Eingabeidentität SHA-256

d-vxt23uic = Grenzen

d-zistzyfl = Beobachtet

d-ff56mklf = Fakten
    .doc = Was die Beobachtung gemeldet hat, wie der Produzent es angegeben hat.

d-4nxr5eoq = Beobachtung

d-vqhz7zzv = Prüfungen

d-znevuybg = Platziert
    .doc = Die gemeldete Makro-Platzierung, wenn die Stufe eine meldet.

d-6mwfx57p = Datenbank-Artefakt

d-t4u6picr = Beobachtungsartefakt

d-jquleaxm = Datenbankeinheiten je Mikrometer

d-abpu3jku = Die links

d-zgxyfzp2 = Die unten

d-cfo4rbhf = Die rechts

d-ozebffaf = Die oben

d-lk5ymnxh = Instanzen insgesamt

d-linro3fe = Fehlende Instanzen

d-l7433uoi = Makros

d-twlssaox = Makro
    .doc = Ein Makro, das eine gemeldete Platzierung hält.
    .noun = { $count ->
        [one] Makro
       *[other] Makros
    }

d-l2pfdak5 = Platz
    .doc = Die gemeldete Makroidentität.

d-shgem24v = Name

d-oo4l5j2f = Master

d-tpbo6p6i = Ausrichtung

d-mvnygfkc = Platzierung

d-sb4fvyic = Links

d-uwbrid6d = Unten

d-bdrtkvnd = Rechts

d-2f6kftyj = Oben

d-twlssaox-filter-orientation = Ausrichtung

d-twlssaox-filter-left = Links

d-twlssaox-filter-bottom = Unten

d-twlssaox-filter-right = Rechts

d-twlssaox-filter-top = Oben

d-654bphdp = Abhängigkeit
    .doc = Eine gemeldete Nachweisabhängigkeit.
    .noun = { $count ->
        [one] Abhängigkeit
       *[other] Abhängigkeiten
    }

d-lifw33iv = Von

d-lau4lbgx = Zu

d-vuoz6p3k = Art

d-flo3szj2 = Artefakt

d-654bphdp-filter-kind = Art

d-v72w2lcr = Unterfluss
    .doc = Ein gemeldeter Unterfluss, auf den ein Nachweisfuss verweist.
    .noun = { $count ->
        [one] Unterfluss
       *[other] Unterflüsse
    }

d-utp25rwu = Schlüssel
    .doc = Die Identität, die der Hersteller dem referenzierten Unterfluss gibt.

d-2hn4pjda = Titel

d-qhfawhwq = Zusammenfassungsartefakt

d-s6kcjx6i = Beleg
    .doc = Ein importierter Hardware-Prüfbeleg, wie eine Person ihn liest.
    .noun = { $count ->
        [one] Beleg
       *[other] Belege
    }

d-mhsaondd = Kennwert
    .doc = Die Identität des bereinigten Belegs, den dieses Projekt hält.

d-nw7f6brj = Quelle
    .doc = Der bereinigte Beleg, wie dieses Projekt ihn hält.

d-x37f53ms = Nachweis

d-nj3b4qpo = Status

d-jhyibxbz = Importierte Bytes SHA-256

d-3zyb62tt = Fehlschlag

d-zon6lior = Fehlschlagstufe

d-bp2hd5ou = Eingabeprüfung

d-sp7jdjll = Hat Eingabeprüfung

d-gcb6foq7 = Bahnen

d-ufj5tgp7 = Hat Bahnen

d-ntsw3ejx = Gruppengröße

d-t5yt4cgs = Hat Gruppengröße

d-ghx5dlez = Angegebenes Toolchain-Image

d-sofs7yem = Beispielmanifest SHA-256

d-tu4t57su = Prüfer SHA-256

d-qvghxafg = Artefaktprüfer SHA-256

d-snhxqhnq = Mapper SHA-256

d-7ifmtq3n = Bibliotheksrevision

d-pyl5qiwx = Abgebildete Zellen

d-psq5vxbm = Hat abgebildete Zellen

d-325si2ag = Gemeldete Bibliotheksfläche

d-4ldwfggs = ABC-Verzögerungsziel (ps)

d-sdqy2njd = Hat ABC-Ziel

d-jxgfvcs7 = Fertigbar

d-d6eyywsn = Hat Fertigbarkeit

d-hu5xloo5 = Synthetische Koeffizienten

d-rmrnw3gy = Hat synthetische Koeffizienten

d-gvdrc4h2 = Synthetische Aktivierungen

d-3uovqvrr = Hat synthetische Aktivierungen

d-6fqc7ll4 = Beispiel SHA-256

d-hsospyt4 = Beispielbeleg SHA-256

d-zenayb65 = RTL-Quelltext SHA-256

d-mbf2wgz7 = Echtes Protokoll

d-4ddzxjoe = C-Instrumentierung

d-cbi2ze7u = Externe Skalarvergleiche

d-277q5ghg = Hat externe Skalarvergleiche

d-bdd52yao = Generische Zellen insgesamt

d-bsobd2xf = Hat generische Zellenzahl

d-7bk6ofeo = Ausgeschlossene Metadatenzellen

d-3sohcddq = Hat ausgeschlossene Metadatenzellen

d-5rnyzfkw = Compiler-Flags

d-u44pzow2 = Prüfungen

d-zg5xxom6 = Werkzeuge

d-zxqh5fqc = Artefakte

d-4nv3clxq = Ausschlüsse

d-upp3twxb = Eingabekennwerte

d-eryiyu4m = Primärquellen

d-ljs2gw3t = Quellen

d-ev5xwwya = Generische Zellen

d-yd5juegt = Operator

d-ytpaenff = Originalbeleg SHA-256

d-67ramk2w = Revision der Primärquelle

d-l2hy4vng = Compilerversion

d-7mcamyri = Compiler-Datei SHA-256

d-gbjecqi4 = Compiler-Flags

d-s52ihrs2 = Primärquellen

d-el3wnlvr = Lokale Quellen

d-vayzuyd3 = Bibliotheken

d-ph5z4g57 = Beobachtungen

d-3tunzd7f = Studie

d-yu7n73kw = Studienstufe

d-o34vwu7q = Originaler Herstellerbeleg SHA-256

d-czwcogej = Ausgeführter Hersteller SHA-256

d-iehv2mu5 = Gemeinsame Eingabeidentität SHA-256

d-idk7oh4r = Angegebene Vergleichswörter

d-tietricm = Verknüpfungen

d-p4nm3blp = Varianten

d-4zxln2bs = Fluss

d-x4ibyx74 = Titel des Flusses

d-eti75aww = Eingabeidentität des Flusses SHA-256

d-dj67bcy5 = Stufen

d-6cz7yifm = Abhängigkeiten

d-ol3j2h7j = Unterflüsse

d-s6kcjx6i-filter-status = Status

d-bmeexqpt = Projekt
    .doc = Das Chip-Entwurfsprojekt eines Arbeitsbereichs: die importierten Nachweise und die strukturellen Operatoren, die eine Person daraus skizziert hat.
    .noun = { $count ->
        [one] project
       *[other] projects
    }
    .untitled = Chip-Entwurf

d-5vvat77h = Berichte

d-lst2bswm = Prüfbelege

d-qajzmqsz = Operatoren

d-ij6pw4z7 = Verbindungen

d-lmh6n2cu = Ausgewählt
    .doc = Was das Fenster zeigt: `all`, `graph`, `report` oder `receipt`.

d-iwadzlgx = Gegenstand
    .doc = Der Kennwert des Berichts oder Belegs, den das Fenster zeigt.

d-u7dhnpij = Knoten
    .doc = Der Operator, dessen eigene Fakten das Fenster zeigt.

d-mppe3c5h = Ausgangsknoten
    .doc = Der Ausgang, von dem aus eine Person verbindet.

d-ocvjm3oh = Ausgangsport

d-vrtmdnei = Zoom
    .doc = Wie weit der Graph gezoomt ist, in Prozent.

d-3oydn2gz = Stufe
    .doc = Die Nachweisstufe, die der gezeigte Beleg offen hält.

d-uuloznl2 = Makro
    .doc = Das Platzierungsmakro, das die gezeigte Stufe offen hält.

d-yl422q4s = Fehlschlag
    .doc = Warum das Letzte, was eine Person verlangte, abgelehnt wurde, in den Worten des Geräts.

d-2cb665aj = Hinweis
    .doc = Was zuletzt geschah, schlicht gesagt.

d-bmeexqpt-filter-zoom = Zoom

d-h6rtufxk = Importanfrage
    .noun = { $count ->
        [one] import ask
       *[other] import asks
    }

d-fmuu7ss2 = JSON

d-yeycxwyn = Knotenanfrage
    .noun = { $count ->
        [one] node ask
       *[other] node asks
    }

d-zcgjq6hw = Id

d-xj4chp77 = Art

d-qzhhc7sz = Form

d-gpmyx3jk = Ausgangsbreite
    .doc = Die Matrix-Ausgangsbreite; null, wenn der Operator keine Matrix hat.

d-lcejfaxh = X

d-sjxnosz6 = Y

d-yeycxwyn-filter-kind = Art

d-yeycxwyn-filter-output_width = Ausgangsbreite

d-yeycxwyn-filter-x = X

d-yeycxwyn-filter-y = Y

d-kycnmjh5 = Kantenanfrage
    .noun = { $count ->
        [one] edge ask
       *[other] edge asks
    }

d-cem5h5og = Von Knoten

d-og4slduv = Von Port

d-esecrerr = Zu Knoten

d-7ydvdfsk = Zu Port

d-azqyhdbh = Graphanfrage
    .noun = { $count ->
        [one] graph ask
       *[other] graph asks
    }

d-6ydpdgb2 = Knoten

d-5sfjy4uo = Verbindungen

d-fgztesqv = Ergebnis von
    .noun = { $count ->
        [one] outcome of
       *[other] outcome ofs
    }

d-bf2avrry = Kennwert

d-wfpvvvxh = Aktiv

d-7ezbtipq = Grund

d-fgztesqv-filter-active = Aktiv

d-fzjohe4t = Operatorart
    .doc = Die strukturellen Operatorarten, aus denen eine Gestaltung besteht.

d-rhhh2itq = Eingang

d-avno42l5 = Eingang des Gatters

d-l3pmoeaz = Eingang der Normalisierungsgewichte

d-qki72q5u = RMS-Normalisierung

d-6tlt753p = Normalisierungsgewichte anwenden

d-d5yprsar = SiLU

d-bdcmtayu = Multiplizieren

d-apt4dgg3 = Eingang der Projektion

d-acuu4z4l = Eingang der Matrixgewichte

d-w23be4x4 = Lineare Projektion

d-bjmzpc2n = Addieren

d-xtxvv5ey = Prüfungsergebnis
    .doc = Wie eine gemeldete Prüfung endete.

d-sv2spyhp = Bestanden

d-6w5sjnjw = Fehlgeschlagen

d-t2bem3bm = Laufergebnis
    .doc = Wie ein gemeldeter Lauf endete.

d-kmk626bt = Bestanden

d-556rg7nj = Fehlgeschlagen

d-newd543u = Abgebrochen

d-xccazk3u = Cacheart
    .doc = Ob ein Speicherbericht seinen Aufmerksamkeits-Cache begrenzt, oder es nicht sagen kann.

d-xtajlvle = Begrenzt

d-owtits4w = Nicht unterstützt

d-ntuzz6km = Beobachtungsart
    .doc = Was eine gemeldete Nachweisstufe beobachtet hat.

d-lam2dzpr = Keine

d-gl2vdyxy = CPU-Referenz

d-5nrx2cub = RTL-Simulation

d-xbq4pclg = Generische Synthese

d-e66d2bnp = Extraherte Bibliothek

d-mk4umznw = RMS-Normalisierung

d-n7efpvbz = SiLU

d-uyr5cv7o = Gated Delta

d-kbmdullp = Softmax

d-mp3q2dr6 = Vergleich
    .doc = Was ein numerischer Vergleich behauptet.

d-abrpzhtn = Exakt

d-q4gghfea = Charakterisierung

d-7rrv34yb = Ergebnis
    .doc = Wie eine gemeldete RTL-Simulation endete.

d-nbmoug3l = Abgeschlossen

d-ao76imbp = Erwartete Ablehnung

d-ynbig6oe = Abhängigkeitsart
    .doc = Was eine gemeldete Nachweisabhängigkeit führt.

d-mrubdlqf = Daten

d-7xcqjq4q = Bau

d-vdbwbp4i = Prüfung

d-6qnz2pt4 = Stufenart
    .doc = Welche Stufe eines Architekturvergleichs ein Beleg meldet.

d-tv2begou = Vorbereitung

d-2pnqkrjb = Numerisch

d-5ggzqkan = Fehler

d-ljmpko27 = Synthese

d-xdwch5pa = Verknüpft

d-jfmns64g = Ausrichtung
    .doc = Die Ausrichtung der gemeldeten Makro-Platzierungen.

d-4bx6xsie = R0

d-3x7sxkex = R90

d-b3l62otc = R180

d-ekckacui = R270

d-paas3xxn = MX

d-zpmeg4e2 = MY

d-plnfglts = MXR90

d-cywro6el = MYR90

d-lz7sfooc = Bericht importieren
    .doc = Importiert den technischen Nachweis einer JSON-Datei: einen analytischen Speicherbericht oder einen Hardware-Prüfbeleg.
    .w-the-report-exceeds-the-1-mib = Der Bericht überschreitet die Importgrenze von 1 MiB

d-lz7sfooc-file = Datei

d-2esuldj2 = Analysebericht importieren
    .doc = Importiert einen analytischen Speicherbericht aus seinem JSON.

d-2esuldj2-document = Dokument

d-bp4izx7i = Prüfbeleg importieren
    .doc = Importiert einen Hardware-Prüfbeleg aus seinem JSON.

d-bp4izx7i-document = Dokument

d-aojvundi = Hereinnehmen
    .doc = Nimmt ein importiertes Dokument herein, welches auch immer: das Gerät prüft es zuerst, und was es hält, erreicht das Projekt erst, wenn sein eigener Vertrag es annimmt.

d-aojvundi-document = Dokument

d-rsfeeosl = Bericht behalten
    .doc = Hält einen analytischen Speicherbericht und die Annahmen und den Umfang, die er angibt.

d-rsfeeosl-digest = Kennwert

d-rsfeeosl-source = Quelle

d-rsfeeosl-facts = Fakten

d-ptt4dxra = Beleg behalten
    .doc = Hält einen Hardware-Prüfbeleg: seinen Umfang, seine Prüfungen, die Identitäten, die er meldet, und welches der drei gemeldeten Protokolle er spricht.

d-ptt4dxra-digest = Kennwert

d-ptt4dxra-source = Quelle

d-ptt4dxra-facts = Fakten

d-5uqijyq2 = Bericht entfernen
    .doc = Entfernt einen Bericht und alles, was er angab.

d-5uqijyq2-r = R

d-zzpfsfn4 = Prüfbeleg entfernen
    .doc = Entfernt einen Prüfbeleg und alles, was er angab.

d-zzpfsfn4-r = R

d-rhugesq6 = Operatorgraph
    .doc = Öffnet den Operatorgraphen.

d-sbus2usu = Alle Berichte
    .doc = Listet jeden Bericht und Beleg, den das Projekt hält.

d-u7uj5e5v = Bericht prüfen
    .doc = Zeigt einen Bericht.

d-u7uj5e5v-r = R

d-5hgiywn5 = Prüfbeleg prüfen
    .doc = Zeigt einen Prüfbeleg.

d-5hgiywn5-r = R

d-lkubu4by = Nachweis prüfen
    .doc = Zeigt eine Stufe des Belegs, den das Fenster offen hält.

d-lkubu4by-r = R

d-lkubu4by-s = S

d-fk62azrb = Platzierung prüfen
    .doc = Zeigt ein Makro der Platzierung, die das Fenster offen hält.

d-fk62azrb-r = R

d-fk62azrb-m = M

d-bovpll5i = Unterfluss öffnen
    .doc = Öffnet den Beleg, den der Unterflussverweis eines Nachweisfusses nennt, wenn seine Zusammenfassung importiert ist.

d-bovpll5i-r = R

d-bovpll5i-s = S

d-xtme7y55 = Verwerfen
    .doc = Legt die Zeile über die letzte Ablehnung oder den letzten Erfolg ab.

d-vmhkm6oz = Graph zoomen
    .doc = Zoomt den Operatorgraphen näher, weiter weg, oder zurück auf seine ganze Ausdehnung.

d-vmhkm6oz-step = Schritt

d-xgsghcqx = Ausgang wählen
    .doc = Wählt den Ausgang, von dem aus eine Person verbindet, und nennt die Eingänge, die er speisen kann.

d-xgsghcqx-op = Operator

d-xgsghcqx-port = Port

d-mfra3i5f = Ausgang abwählen
    .doc = Verlässt den Graphen ohne gewählten Ausgang.

d-2nj2oywc = Operator prüfen
    .doc = Zeigt die eigenen Fakten eines Operators, oder schließt sie.

d-2nj2oywc-op = Operator

d-hhgkdr2m = Operator schließen

d-vlxtsdsp = Operator hinzufügen
    .doc = Fügt einen Operator der Art `kind` hinter den schon im Graphen hinzu, auf einem vier breiten Raster.

d-vlxtsdsp-name = Name

d-7woverrw = Operator mit Form hinzufügen
    .doc = Fügt einen Operator mit genau der Form, Matrixbreite und dem Platz hinzu, die ein Aufrufer angibt.

d-7woverrw-node = Knoten

d-7woverrw-kind = Art

d-7woverrw-shape = Form

d-7woverrw-output_width = Ausgangsbreite

d-7woverrw-x = X

d-7woverrw-y = Y

d-7ssomfsm = Eingabeform des Operators festlegen
    .doc = Ändert die Form eines Operators.

d-7ssomfsm-op = Operator

d-7ssomfsm-shape = Form

d-64uig2eg = Form anwenden
    .doc = Wendet die Form an, die eine Person für einen Operator eingetippt hat, wenn sie eine nennt.

d-64uig2eg-op = Operator

d-verclot2 = Ausgangsbreite anwenden
    .doc = Wendet die Matrix-Ausgangsbreite an, die eine Person für einen Operator eingetippt hat.

d-verclot2-op = Operator

d-qixmmh5c = Makro antippen
    .doc = Zeigt ein Platzierungsmakro der Stufe, die das Fenster offen hält.

d-qixmmh5c-s = S

d-qixmmh5c-index = Index

d-hafquklm = Matrix-Ausgangsbreite festlegen
    .doc = Legt die Matrix-Ausgangsbreite eines Operators fest.

d-hafquklm-op = Operator

d-hafquklm-width = Breite

d-ja53622w = Operator verschieben
    .doc = Verschiebt einen Operator an einen Platz auf dem Zeichenfeld.

d-ja53622w-op = Operator

d-ja53622w-x = X

d-ja53622w-y = Y

d-jhu3cqwg = Operator entfernen
    .doc = Entfernt einen Operator und jede Verbindung, die er hielt.

d-jhu3cqwg-op = Operator

d-yiq7sfzp = Operatoren verbinden
    .doc = Verbindet einen Ausgang mit einem Eingang.

d-yiq7sfzp-from = Von

d-yiq7sfzp-from_port = Von Port

d-yiq7sfzp-to = Zu

d-yiq7sfzp-to_port = Zu Port

d-nz7gzmnw = Eingang trennen
    .doc = Trennt jede Verbindung in einen Eingang.

d-nz7gzmnw-op = Operator

d-nz7gzmnw-port = Port

d-rcuv3d36 = Graph auflösen
    .doc = Löst die ganze Gestaltung erneut auf, sodass das Fenster zeigt, was die gespeicherte Gestaltung ergibt.

d-mtz3jwsi = Graph anwenden
    .doc = Hält die Ports und Verbindungsergebnisse, die das Gerät antwortete, sodass das Fenster nach der eigenen Auflösung der Gestaltung zeichnet und ablehnt.

d-mtz3jwsi-answer = Antwort

d-kuhuzr64 = Gesehenes Makro prüfen
    .doc = Zeigt ein Makro, das die Person aus der eigenen Liste der Platzierung gewählt hat.

d-kuhuzr64-r = R

d-kuhuzr64-s = S

d-kuhuzr64-m = M

d-2i4xepvu = Graph antippen
    .doc = Ein Druck auf das Zeichenfeld: der Titel eines Operators öffnet seine Fakten, ein Ausgang ist das, wovon der nächste Eingang verbindet, und ein Eingang verbindet oder öffnet seinen Operator.

d-2i4xepvu-index = Index

d-tw273e3b = Ablehnungswörter
    .w-an-endpoint-no-longer-exists = Ein Endpunkt existiert nicht mehr
    .w-a-port-no-longer-exists = Ein Port existiert nicht mehr
    .w-port-shapes-differ = Die Port-Formen unterscheiden sich
    .w-another-connection-occupies-this-input = Eine andere Verbindung belegt diesen Eingang
    .w-this-connection-would-create-a-cycle = Diese Verbindung erzeugte einen Zyklus
    .w-the-device-refused-this-connection = Das Gerät hat diese Verbindung abgelehnt

d-s6okr33x = Artwörter
    .w-input = Eingang
    .w-gate-input = Gate-Eingang
    .w-normalization-weights = Normalisierungsgewichte
    .w-rms-normalization = RMS-Normalisierung
    .w-apply-normalization-weights = Normalisierungsgewichte anwenden
    .w-silu-activation = SiLU-Aktivierung
    .w-multiply = Multiplizieren
    .w-sum = Summieren
    .w-projection-input = Projektionseingang
    .w-matrix-weights = Matrixgewichte
    .w-linear-projection = Lineare Projektion

d-wigjid4m = Portwörter
    .w-value = Wert
    .w-weights = Gewichte
    .w-result = Ergebnis
    .w-left-operand = Linker Operand
    .w-right-operand = Rechter Operand

d-pr4gzj3v = Statuswörter
    .w-reported-failed = Als fehlgeschlagen gemeldet
    .w-reported-cancelled = Als abgebrochen gemeldet
    .w-reported-passed = Als bestanden gemeldet

d-sijgqczt = Prüfungswörter
    .w-reported-failed = Als fehlgeschlagen gemeldet
    .w-reported-passed = Als bestanden gemeldet

d-mfu7hrb3 = Stufenwörter
    .w-numerical-verification = Numerische Prüfung
    .w-fault-controls = Fehlerkontrollen
    .w-generic-synthesis = Generische Synthese
    .w-linked-comparison = Verknüpfter Vergleich
    .w-preparation = Vorbereitung

d-iosocdmu = Abhängigkeitswörter
    .w-build-dependency = Build-Abhängigkeit
    .w-verification-dependency = Prüfabhängigkeit
    .w-data-dependency = Datenabhängigkeit

d-q7pbq4xx = Vergleichswörter
    .w-backend-characterization = Backend-Charakterisierung
    .w-exact-comparison = Exakter Vergleich

d-ehamip6c = Ergebniswörter
    .w-expected-refusal-no-completed-result = Erwartete Ablehnung; kein abgeschlossenes Ergebnis
    .w-completed-simulation = Abgeschlossene Simulation

d-3ndeu67e = Beobachtungswörter
    .w-cpu-reference = CPU-Referenz
    .w-rtl-simulation = RTL-Simulation
    .w-generic-synthesis = Generische Synthese
    .w-extracted-library-estimate = Schätzung aus extrahierter Bibliothek
    .w-no-numerical-observations-reported = Keine numerischen Beobachtungen gemeldet

d-sl6rfikz = Hinweis zum Beleg
    .w-reported-evidence-only-artifacts-are-not = Nur gemeldete Nachweise. Artefakte werden weder authentifiziert noch ausgeführt. Schätzungen der Bibliothek sind keine Siliziummessungen, Freigabe, Fertigungsgenehmigung oder Modellqualität.
    .w-producer-reported-architecture-comparison-stage-links = Vom Hersteller gemeldeter Architekturvergleich. Stufenverbindungen und Artefaktidentitäten sind hier nicht geprüft. Zyklen und generische Zellen belegen keine Taktfrequenz, physische Fläche, Energie oder Codequalität.
    .w-imported-reported-cpu-numerical-observations-artifact = Importierte gemeldete numerische CPU-Beobachtungen. Artefaktidentität und Echtheit sind hier nicht geprüft; das belegt keine Ausführung des vollständigen Modells, keine Modellqualität, kein RTL und keine physische PPA.
    .w-imported-reported-research-library-mapping-evidence = Importierte gemeldete Zuordnungsnachweise aus einer Forschungsbibliothek. Artefaktidentität und Echtheit sind hier nicht geprüft; das belegt kein fertigbares Layout, keinen Timing-Abschluss und keine Modellqualität.
    .w-imported-reported-integer-hardware-evidence-artifact = Importierte gemeldete Integer-Hardware-Nachweise. Artefaktidentität und Echtheit sind hier nicht geprüft; das belegt keine PPA des ganzen Chips und keine Modellqualität.

d-zofzh5ki = Beobachtung gesagt
    .w-words-compared-differing = { $compared_words } Wörter verglichen, { $differing_words } davon abweichend
    .w-accepted-inputs-outputs-cycles = { $accepted_inputs } übernommene Eingaben, { $accepted_outputs } Ausgaben, { $cycles } Zyklen
    .w-logic-cells-logical-memory-bits = { $logic_cells } Logikzellen, { $logical_memory_bits } logische Speicherbits
    .w-routed-nets-router-violations = { $library }: { $routed_signal_nets } verlegte Netze, { $router_violations } Verstöße des Routers

d-ojf6ymem = Fenstertitel
    .w-operator-graph = Operatorgraph
    .w-chip-design = Chip-Entwurf

d-h3ulwtvt = Bezeichnung der Prüfungen
    .w-producer-reported-stage-checks = Vom Hersteller gemeldete Stufenprüfungen
    .w-reported-numerical-checks = Gemeldete numerische Prüfungen
    .w-reported-integer-checks = Gemeldete Integer-Prüfungen

d-m3lopvjx = Ausrichtungswörter
    .w-r90 = R90
    .w-r180 = R180
    .w-r270 = R270
    .w-mx = MX
    .w-my = MY
    .w-mxr90 = MXR90
    .w-myr90 = MYR90
    .w-r0 = R0

d-a2o6nstv = Verbindungszeile
    .w-connecting-from-the-output-of = Verbindet vom Ausgang { $port_words } von { $from }

d-engplagq = Konfliktzeile
    .w-cannot-feed = { $from_node }.{ $port_words } kann { $to_node }.{ $port_words2 } nicht speisen: { $refusal_words }

d-b53kihl2 = Bericht importieren

d-frkvs7gt = Fenster
    .w-chip-design = Chip-Entwurf
    .w-operator-graph = Operatorgraph
    .w-import-report = Bericht importieren
    .w-all-reports = Alle Berichte
    .w-that-could-not-be-done = Das konnte nicht getan werden
    .w-dismiss = Verwerfen
    .w-nothing-in-this-project-yet = In diesem Projekt ist noch nichts
    .w-reported-output-artifact-identities = Gemeldete Identitäten der Ausgabeartefakte
    .w-not-reported = Nicht gemeldet
    .w-exclusions-and-limitations = Ausschlüsse und Grenzen
    .w-verification-exclusion = Ausschluss aus der Prüfung
    .w-reported-numerical-observations = Gemeldete numerische Beobachtungen
    .w-remove-verification-receipt = Prüfbeleg entfernen
    .w-imported-analytical-storage-report-source-authenticity = Analytischer Speicherbericht importiert. Echtheit der Quelle und Qualität der Quantisierung sind ungeprüft.
    .w-reported-provenance = Gemeldete Herkunft
    .w-model-repository = Modell-Repository
    .w-reported-model-revision = Gemeldete Modellrevision
    .w-reported-config-sha-256 = Gemeldete Konfiguration SHA-256
    .w-imported-bytes-blake3 = Importierte Bytes BLAKE3
    .w-storage-assumptions = Speicherannahmen
    .w-assumed-logical-parameters = Angenommene logische Parameter
    .w-assumed-weight-bits = Angenommene Gewichtsbits
    .w-assumed-cache-bits = Angenommene Cache-Bits
    .w-assumed-context-tokens = Angenommene Kontext-Token
    .w-batch-size = Batch-Größe
    .w-analytical-storage = Analytischer Speicher
    .w-packed-weight-bytes = Gepackte Gewichtsbytes
    .w-cache-storage-unverified = Cache-Speicher ungeprüft
    .w-reported-full-attention-layers = Gemeldete Schichten mit voller Aufmerksamkeit
    .w-reported-cache-elements = Gemeldete Cache-Elemente
    .w-partial-cache-bytes = Bytes des partiellen Caches
    .w-excluded-from-cache = Aus dem Cache ausgeschlossen
    .w-unestimated = Nicht geschätzt
    .w-report-limitation = Grenze des Berichts
    .w-verification-scope = Umfang der Prüfung
    .w-internal-arithmetic-only-the-pinned-config = Nur interne Arithmetik. Die festgelegte Konfiguration wird nicht rekonstruiert; Hardwareleistung und Codequalität werden nicht gemessen.
    .w-remove-report = Bericht entfernen
    .w-node-identity = Identität des Knotens
    .w-fp32-shape = FP32-Form
    .w-input-width = Eingangsbreite
    .w-normalization-width = Normalisierungsbreite
    .w-position = Position
    .w-in = Eingang
    .w-out = Ausgang
    .w-apply-shape = Form anwenden
    .w-apply-output-width = Ausgangsbreite anwenden
    .w-move-left = Nach links verschieben
    .w-move-right = Nach rechts verschieben
    .w-move-up = Nach oben verschieben
    .w-move-down = Nach unten verschieben
    .w-disconnect = { $port_words } trennen
    .w-remove-operator = Operator entfernen

d-i4fiqfpg = Orte
    .w-operator-graph = Operatorgraph
    .w-all-reports = Alle Berichte
    .w-reports = Berichte
    .w-verifications = Prüfbelege

d-rnmryklx = Nachweisliste
    .w-nothing-in-this-project-yet = In diesem Projekt ist noch nichts
    .w-sketch-the-operators-in-the-operator = Skizziere die Operatoren im Operatorgraphen oder importiere einen technischen Bericht (JSON), um die aufgezeichneten Nachweise zu lesen.
    .w-open-the-operator-graph = Den Operatorgraphen öffnen
    .w-reports = Berichte
    .w-verifications = Prüfbelege
    .w-operators = Operatoren

d-xfrgv6rb = Berichtsseite
    .w-imported-analytical-storage-report-source-authenticity = Analytischer Speicherbericht importiert. Echtheit der Quelle und Qualität der Quantisierung sind ungeprüft.
    .w-reported-provenance = Gemeldete Herkunft
    .w-model-repository = Modell-Repository
    .w-reported-model-revision = Gemeldete Modellrevision
    .w-reported-config-sha-256 = Gemeldete Konfiguration SHA-256
    .w-imported-bytes-blake3 = Importierte Bytes BLAKE3
    .w-storage-assumptions = Speicherannahmen
    .w-assumed-logical-parameters = Angenommene logische Parameter
    .w-assumed-weight-bits = Angenommene Gewichtsbits
    .w-assumed-cache-bits = Angenommene Cache-Bits
    .w-assumed-context-tokens = Angenommene Kontext-Token
    .w-batch-size = Batch-Größe
    .w-analytical-storage = Analytischer Speicher
    .w-packed-weight-bytes = Gepackte Gewichtsbytes
    .w-cache-storage-unverified = Cache-Speicher ungeprüft
    .w-not-reported = Nicht gemeldet
    .w-reported-full-attention-layers = Gemeldete Schichten mit voller Aufmerksamkeit
    .w-reported-cache-elements = Gemeldete Cache-Elemente
    .w-partial-cache-bytes = Bytes des partiellen Caches
    .w-excluded-from-cache = Aus dem Cache ausgeschlossen
    .w-exclusions-and-limitations = Ausschlüsse und Grenzen
    .w-unestimated = Nicht geschätzt
    .w-report-limitation = Grenze des Berichts
    .w-verification-scope = Umfang der Prüfung
    .w-internal-arithmetic-only-the-pinned-config = Nur interne Arithmetik. Die festgelegte Konfiguration wird nicht rekonstruiert; Hardwareleistung und Codequalität werden nicht gemessen.
    .w-remove-report = Bericht entfernen

d-znn6bjwe = Belegseite
    .w-nothing-in-this-project-yet = In diesem Projekt ist noch nichts
    .w-reported-output-artifact-identities = Gemeldete Identitäten der Ausgabeartefakte
    .w-not-reported = Nicht gemeldet
    .w-exclusions-and-limitations = Ausschlüsse und Grenzen
    .w-verification-exclusion = Ausschluss aus der Prüfung
    .w-reported-numerical-observations = Gemeldete numerische Beobachtungen
    .w-remove-verification-receipt = Prüfbeleg entfernen

d-gynnzyfj = Umfang des Belegs
    .w-reported-verification-scope = Gemeldeter Umfang der Prüfung
    .w-reported-run-result = Gemeldetes Laufergebnis
    .w-evidence-class = Nachweisklasse
    .w-failure-explanation = Erklärung des Fehlers
    .w-failure-stage = Fehlschlagstufe
    .w-reported-input-validation = Gemeldete Prüfung der Eingabe
    .w-output-lanes = Ausgabebahnen
    .w-activation-group-size = Gruppengröße der Aktivierungen
    .w-reported-mapped-cell-count = Gemeldete Anzahl abgebildeter Zellen
    .w-sum-of-research-library-cell-areas = Summe der Zellflächen der Forschungbibliothek
    .w-abc-optimization-target-ps-not-measured = Optimierungsziel von ABC (ps), keine gemessene Zeit
    .w-process-scope = Umfang des Prozesses
    .w-nonmanufacturable-research-library = Nicht fertigbare Forschungbibliothek
    .w-external-scalar-comparisons = Externe Skalarvergleiche
    .w-protocol-observation = Beobachtung des Protokolls
    .w-c-reference-instrumentation = C-Referenzinstrumentierung
    .w-scalar-compiler-flags = Compiler-Flags für Skalare
    .w-reported-generic-cell-count = Gemeldete Anzahl generischer Zellen
    .w-excluded-metadata-cells = Ausgeschlossene Metadatenzellen
    .w-reported-experiment-stage = Gemeldete Versuchsstufe
    .w-declared-complete-output-words-per-variant = Angegebene vollständige Ausgabewörter je Variante
    .w-reported-numerical-observation = Gemeldete numerische Beobachtung

d-dlvirkwn = Herkunft des Belegs
    .w-reported-provenance = Gemeldete Herkunft
    .w-original-imported-bytes-blake3 = Ursprünglich importierte Bytes BLAKE3
    .w-imported-bytes-sha-256 = Importierte Bytes SHA-256
    .w-not-reported = Nicht gemeldet
    .w-declared-immutable-toolchain-image = Angegebenes unveränderliches Toolchain-Image
    .w-reported-sample-manifest-sha-256 = Gemeldetes Beispielmanifest SHA-256
    .w-reported-verifier-sha-256 = Gemeldeter Prüfer SHA-256
    .w-reported-artifact-validator-sha-256 = Gemeldeter Artefaktprüfer SHA-256
    .w-reported-mapper-sha-256 = Gemeldeter Mapper SHA-256
    .w-reported-library-source-revision = Gemeldete Revision der Bibliotheksquelle
    .w-reported-sample-digest = Gemeldeter Beispielkennwert
    .w-reported-sample-receipt-digest = Gemeldeter Beleg des Beispiels
    .w-reported-rtl-source-digest = Gemeldeter RTL-Quelltext
    .w-original-receipt-sha-256 = Ursprünglicher Beleg SHA-256
    .w-reported-primary-source-revision = Gemeldete Revision der Primärquelle
    .w-reported-compiler-version = Gemeldete Compilerversion
    .w-reported-tool-executable-sha-256 = Gemeldete ausführbare Werkzeugdatei SHA-256
    .w-reported-compiler-flags = Gemeldete Compiler-Flags
    .w-reported-experiment-stage = Gemeldete Versuchsstufe
    .w-original-producer-receipt-sha-256 = Ursprünglicher Herstellerbeleg SHA-256
    .w-executed-producer-sha-256 = Ausgeführter Hersteller SHA-256
    .w-shared-input-identity-sha-256 = Gemeinsame Eingabeidentität SHA-256
    .w-tool-identities = Identitäten der Werkzeuge

d-ka2nnn5r = Beobachtungszeile
    .w-reported-numerical-observation = Gemeldete numerische Beobachtung
    .w-fp32-words-compared-exactly = { $profile }: { $fp32_comparisons } FP32-Wörter exakt verglichen
    .w-inputs-silu-compared-exactly-exp-has = { $total_inputs } Eingaben: SiLU exakt verglichen; exp hat { $exp_differences } Abweichungen, höchstens { $max_exp_ulp } ULP
    .w-heads-tokens-width-attention-and-state = { $heads } Köpfe, { $tokens } Token, Breite { $width }: { $attention_words } Aufmerksamkeits- und { $state_words } Zustandswörter exakt verglichen
    .w-words-masked-differences-maximum-ulp = { $name }: { $width } Wörter, { $masked } maskiert; { $differing_words } Abweichungen, höchstens { $max_ulp } ULP

d-beykirjy = Studienseite
    .w-stage-links = Stufenverknüpfungen
    .w-original-producer-receipt-sha-256 = Ursprünglicher Herstellerbeleg SHA-256
    .w-executed-producer-sha-256 = Ausgeführter Hersteller SHA-256
    .w-shared-input-identity-sha-256 = Gemeinsame Eingabeidentität SHA-256
    .w-cold-weight-programming-cycles = Kaltzyklen der Gewichtsprogrammierung
    .w-activation-load-cycles = Zyklen zum Laden der Aktivierungen
    .w-warm-execution-cycles = Warme Ausführungszyklen
    .w-accepted-packed-weight-bytes = Übernommene gepackte Gewichtsbytes
    .w-accepted-block-descriptor-bytes = Übernommene Block-Deskriptor-Bytes
    .w-activation-cache-reads = Lesezugriffe auf den Aktivierungs-Cache
    .w-quantizer-packets = Quantisierer-Pakete
    .w-compared-output-words = Verglichene Ausgabewörter
    .w-reported-output-sha-256 = Gemeldete Ausgabe SHA-256
    .w-generic-logic-cells = Generische Logikzellen
    .w-activation-cache-bits = Bits des Aktivierungs-Caches
    .w-programmed-weight-storage-bits = Programmierte Gewichtsspeicherbits
    .w-initialized-fixed-storage-bits = Initialisierte feste Speicherbits
    .w-of = { $name } von { $identity }
    .w-word-width-bits = Wortbreite (Bits)
    .w-depth-words = Tiefe (Wörter)
    .w-logical-storage-bits = Logischer Speicher (Bits)
    .w-read-ports = Leseports
    .w-clocked-read-ports = Getaktete Leseports
    .w-write-ports = Schreibports
    .w-fully-initialized = Vollständig initialisiert
    .w-yes = Ja
    .w-no = Nein

d-j65hkz24 = Flusskörper
    .w-reported-evidence-flow = Gemeldeter Nachweisfuss
    .w-reported-evidence-only = Nur gemeldete Nachweise
    .w-declared-input-identity-sha-256 = Angegebene Eingabeidentität SHA-256
    .w-version-executable = Version { $version }, ausführbare Datei { $executable_sha256 }
    .w-reported-evidence-only-artifacts-are-not = Nur gemeldete Nachweise. Artefakte werden weder authentifiziert noch ausgeführt. Schätzungen der Bibliothek sind keine Siliziummessungen, Freigabe, Fertigungsgenehmigung oder Modellqualität.
    .w-reported-stage-result = Gemeldetes Stufenergebnis
    .w-observation = Beobachtung
    .w-original-receipt-artifact = Originalbeleg-Artefakt
    .w-declared-artifact-sha-256 = Angegebenes Artefakt SHA-256
    .w-placement-database-artifact = Platzierungsdatenbank-Artefakt
    .w-placement-observation-artifact = Platzierungsbeobachtungs-Artefakt
    .w-declared-producer-sha-256 = Angegebener Hersteller SHA-256
    .w-scope-limitation = Grenze des Umfangs
    .w-declared-dependency = Angegebene Abhängigkeit
    .w-feeds-artifact = { $from } speist { $to }, { $dependency_words }, Artefakt { $artifact }
    .w-reported-macro-placement = Gemeldete Makro-Platzierung
    .w-macro-footprints-only-other-instances-are = Nur Makro-Fußabdrücke; andere Instanzen fehlen. Die gemeldete Geometrie belegt weder Layout-Authentizität noch Freigabe. Koordinaten sind in Mikrometern, mit dem physikalischen Ursprung unten links.
    .w-total-reported-instances = Gemeldete Instanzen insgesamt
    .w-instances-omitted-from-this-view = Instanzen, die in dieser Ansicht fehlen
    .w-die-bounds-m = Die-Grenzen (µm)
    .w-cell-master = Zell-Master
    .w-macro-bounds-m = Makro-Grenzen (µm)
    .w-orientation = Ausrichtung
    .w-placement-status = Platzierungsstatus
    .w-original-artifact-references = Referenzen auf die Originalartefakte

d-hus44a4k = Nachweisstufenkarte

d-sze4ff2m = Unterflusskarte
    .w-subflow-reference = Verweis auf einen Unterfluss

d-b6xciujw = Stufenfakten
    .w-reported-numerical-observation = Gemeldete numerische Beobachtung
    .w-comparison-contract = Vergleichsvertrag
    .w-compared-words = Verglichene Wörter
    .w-differing-words = Abweichende Wörter
    .w-reported-accepted-output-sha-256 = Gemeldete übernommene Ausgabe SHA-256
    .w-reported-simulation-outcome = Gemeldetes Ergebnis der Simulation
    .w-expected-refusals-observed = Beobachtete erwartete Ablehnungen
    .w-observed-rtl-simulation-cycles = Beobachtete Zyklen der RTL-Simulation
    .w-accepted-inputs = Übernommene Eingaben
    .w-accepted-outputs = Übernommene Ausgaben
    .w-generic-logic-cells = Generische Logikzellen
    .w-logical-memory-bits = Logische Speicherbits
    .w-declared-library = Angegebene Bibliothek
    .w-declared-clock-constraint-ns = Angegebene Taktvorgabe (ns)
    .w-estimated-setup-slack-ns = Geschätzte Setup-Reserve (ns)
    .w-estimated-hold-slack-ns = Geschätzte Hold-Reserve (ns)
    .w-routed-signal-nets = Verlegte Signalnetze
    .w-reported-router-violations = Gemeldete Verstöße des Routers
    .w-mapped-library-area-estimate-m = Geschätzte Fläche der abgebildeten Bibliothek (µm²)

d-n622kuhe = Stufenseite
    .w-reported-stage-result = Gemeldetes Stufenergebnis
    .w-observation = Beobachtung
    .w-original-receipt-artifact = Originalbeleg-Artefakt
    .w-declared-artifact-sha-256 = Angegebenes Artefakt SHA-256
    .w-placement-database-artifact = Platzierungsdatenbank-Artefakt
    .w-placement-observation-artifact = Platzierungsbeobachtungs-Artefakt
    .w-declared-producer-sha-256 = Angegebener Hersteller SHA-256
    .w-declared-input-identity-sha-256 = Angegebene Eingabeidentität SHA-256
    .w-scope-limitation = Grenze des Umfangs
    .w-declared-dependency = Angegebene Abhängigkeit
    .w-feeds-artifact = { $from } speist { $to }, { $dependency_words }, Artefakt { $artifact }
    .w-reported-macro-placement = Gemeldete Makro-Platzierung
    .w-macro-footprints-only-other-instances-are = Nur Makro-Fußabdrücke; andere Instanzen fehlen. Die gemeldete Geometrie belegt weder Layout-Authentizität noch Freigabe. Koordinaten sind in Mikrometern, mit dem physikalischen Ursprung unten links.
    .w-total-reported-instances = Gemeldete Instanzen insgesamt
    .w-instances-omitted-from-this-view = Instanzen, die in dieser Ansicht fehlen
    .w-die-bounds-m = Die-Grenzen (µm)
    .w-cell-master = Zell-Master
    .w-macro-bounds-m = Makro-Grenzen (µm)
    .w-orientation = Ausrichtung
    .w-placement-status = Platzierungsstatus

d-erao3yv4 = Makroseite
    .w-cell-master = Zell-Master
    .w-macro-bounds-m = Makro-Grenzen (µm)
    .w-orientation = Ausrichtung
    .w-placement-status = Platzierungsstatus

d-2jgrmrzq = Unterflussseite
    .w-subflow-reference = Verweis auf einen Unterfluss
    .w-relative-artifact-reference = Relativer Artefaktverweis
    .w-declared-artifact-sha-256 = Angegebenes Artefakt SHA-256
    .w-not-reported = Nicht gemeldet
    .w-open-imported-subflow = Importierten Unterfluss öffnen
    .w-referenced-summary-has-not-been-imported = Die referenzierte Zusammenfassung wurde nicht importiert. Für diesen Verweis wird keine Ausführung behauptet.

d-op24aqz3 = Operatorseite
    .w-node-identity = Identität des Knotens
    .w-fp32-shape = FP32-Form
    .w-input-width = Eingangsbreite
    .w-normalization-width = Normalisierungsbreite
    .w-position = Position
    .w-in = Eingang
    .w-out = Ausgang
    .w-apply-shape = Form anwenden
    .w-apply-output-width = Ausgangsbreite anwenden
    .w-move-left = Nach links verschieben
    .w-move-right = Nach rechts verschieben
    .w-move-up = Nach oben verschieben
    .w-move-down = Nach unten verschieben
    .w-disconnect = { $port_words } trennen
    .w-remove-operator = Operator entfernen

d-lcswi4co = Graphkörper
    .w-add-operator = Operator hinzufügen
    .w-operator = Operator
    .w-zoom-out = Verkleinern
    .w-fit-to-window = An das Fenster anpassen
    .w-zoom-in = Vergrößern
    .w-structural-design-only-this-graph-does = Nur strukturelle Gestaltung. Dieser Graph führt keine Hardware aus und qualifiziert keine.
    .w-cancel = Abbrechen
    .w-to-connect-two-operators-choose-an = Um zwei Operatoren zu verbinden, wähle einen Ausgang und dann einen Eingang, den er speisen kann.
    .w-no-operators-yet = Noch keine Operatoren
    .w-add-one-to-start-designing = Füge einen hinzu, um zu gestalten.
    .w-the-structural-operator-design = Die strukturelle Operatorgestaltung
