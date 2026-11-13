# Shell auf Deutsch. Keys are the declarations' stable ids (app.ids); see locales/en-US.ftl.

d-so-tilo-shell = Shell

d-4jk2lkpt = Schreibtisch
    .doc = Der Schreibtisch dieses Geräts: die Zeit im Stil der Person, wer sie ist, ihre Benachrichtigungen, die Arbeitsbereiche auf diesem Gerät, die für sie laufenden Aufgaben und die Oberfläche, die der Schreibtisch über seinen Fenstern zeigt.
    .noun = { $count ->
        [one] Schreibtisch
       *[other] Schreibtische
    }

d-kx4rprro = Person

d-6unuw4qc = Hier
    .doc = Die Oberfläche, in der die Person ist, etwa die Übersicht oder die Benachrichtigungen; leer in Ruhe.

d-wey3uqyo = Wechsler gezeigt
    .doc = Ob das Panel des Wechslers zeigt: ein kurzes Alt+Tab wechselt Fenster und zeichnet nichts.

d-5mipdtxu = Bereich des Wechslers
    .doc = Welche Fenster der Weg des Wechslers kreuzt: `workspace` für jedes Fenster, `app` für die Fenster der App vorn (Alt+`).

d-foayl4ai = Suche
    .doc = Was die Person in die Suche der Shell getippt hat, die den Launcher dort öffnet, wo sie aufgehört hat.

d-wco4hiud = Banner
    .doc = Der Hinweis, der jetzt als Banner über dem Schreibtisch steht, nach seiner Kennung.

d-7bs5q4ht = Uhr

d-rlrql4ir = Timer
    .doc = Der Timer oder die Stoppuhr, die Clocks laufen lässt, nach ihrem Namen: ihrer Beschriftung, oder `Timer` bzw. `Stoppuhr`, wenn die App keine nannte; leer, wenn keiner schwimmt.

d-m4heyzat = Timer übrig
    .doc = Was der Timer anzeigt: was ihm bleibt oder was die Stoppuhr gelaufen ist.

d-pql2t5r3 = Timer pausiert

d-qb4cqhjk = Timer Stoppuhr

d-zxfieu2y = Frage
    .doc = Die Frage, die der Person über sie selbst gestellt wird; leer, wenn keine wartet.

d-w2fyvufg = Detail der Frage
    .doc = Was das Beantworten der Frage bringt.

d-zqu6ku2o = Antwort auf die Frage
    .doc = Die Antwort auf die Frage, wie ihr Knopf sie sagt.

d-eewsi53q = Frage offen
    .doc = Ob die Frage auf dem Avatar der Person offen steht.

d-gi7d6wct = Wochentag

d-5cs3vm3f = Datum

d-hj6eqjwk = Ruhe

d-c5k5ua2t = Arbeitsbereich

d-xpcwi5mg = Leiste

d-vbyis266 = Arbeitsbereiche

d-xu2jijql = Personen

d-g4t5njqq = Leiste erreicht
    .doc = Der Zeiger an der oberen Kante und ein Fenster, das steht, wo die Leiste fährt: was eine Leiste befolgt, die nur manchmal zeigt.

d-mvfeip6u = Leiste verdeckt

d-pjnfirs3 = Dock erreicht
    .doc = Der Zeiger an der unteren Kante und ein Fenster, das steht, wo das Dock fährt.

d-prsypg4e = Dock verdeckt

d-twcpnt5t = Agenten

d-sh62fec4 = Arbeitsbereiche

d-3vvnnztw = Hinweise

d-lz6qs7zm = Aktionen der Hinweise

d-5kokd6gv = Fenster

d-pdaqdidq = Infobereich
    .doc = Die Einträge laufender Anwendungen im Infobereich, in der Reihenfolge, in der sie sich registriert haben.

d-n2fzxj5u = Stile
    .doc = Die Schreibtischstile, die die installierten Apps anbieten: jeder eine Ansicht der Anordnung, aufgeführt nach seinen eigenen Worten.

d-4jk2lkpt-filter-switcher_revealed = Wechsler gezeigt

d-4jk2lkpt-filter-timer_paused = Timer pausiert

d-4jk2lkpt-filter-timer_stopwatch = Timer Stoppuhr

d-4jk2lkpt-filter-ask_open = Frage offen

d-4jk2lkpt-filter-quiet = Ruhe

d-4jk2lkpt-filter-bar_reached = Leiste erreicht

d-4jk2lkpt-filter-bar_covered = Leiste verdeckt

d-4jk2lkpt-filter-dock_reached = Dock erreicht

d-4jk2lkpt-filter-dock_covered = Dock verdeckt

d-zxwtaxaw = Schreibtischstil
    .doc = Ein Schreibtischstil: eine Ansicht der Anordnung, die eine installierte App deklariert.
    .noun = { $count ->
        [one] Schreibtischstil
       *[other] Schreibtischstile
    }

d-ly2aksp3 = Schlüssel
    .doc = Die Ansichtsreferenz des Stils, nach der der Datensatz ihn hält.

d-t6ojc3kt = Stil
    .doc = Die Ansichtsreferenz: ein Paket und eine Ansicht, was `Viewing.style` nennt.

d-yxdj6rhs = Paket
    .doc = Das Paket, das ihn deklariert.

d-uew43mty = Name
    .doc = Der eigene Name der Ansicht, wie ihre Deklaration ihn schreibt.

d-rhx3oobw = Zeile
    .doc = Die Dok-Zeile der Ansicht, in den Worten ihres Autors: wonach der Umschalter sie auflistet.

d-ifgxec27 = Lädt
    .doc = Ob der Stil lädt: falsch für einen, dessen App nicht hier ist oder nicht mit diesem Schreibtisch zusammenarbeitet, oder dessen Ansicht beim Laufen scheiterte, damit der Umschalter es sagen kann und der Schreibtisch die eingebauten Leisten zeigt, bis es behoben ist.

d-eihyiyeb = Grund
    .doc = Warum der Stil nicht lädt, in den Worten, mit denen er scheiterte, wenn er es nicht tut; leer, wenn er lädt.

d-zxwtaxaw-filter-loads = Lädt

d-bynckgvt = Arbeitsbereich
    .doc = Ein Arbeitsbereich auf diesem Gerät.
    .noun = { $count ->
        [one] Arbeitsbereich
       *[other] Arbeitsbereiche
    }

d-pmjwp2sk = Schlüssel

d-nze6cia2 = Name

d-ce523tqy = Aktuell

d-bynckgvt-filter-current = Aktuell

d-tlbg2fms = Eintrag im Infobereich
    .doc = Der Eintrag einer laufenden Anwendung im Infobereich: was die Leiste zeigt und was ein Druck darauf erreicht.
    .noun = { $count ->
        [one] Eintrag im Infobereich
       *[other] Einträge im Infobereich
    }

d-o3wuotce = Schlüssel

d-e5qbz5cz = Titel

d-lsd5bmss = Menü

d-tlbg2fms-filter-menu = Menü

d-bqittbss = Person
    .doc = Jemand anderes hier im Arbeitsbereich vorn.
    .noun = { $count ->
        [one] Person
       *[other] Personen
    }

d-vfhh3oax = Schlüssel

d-lmoug2df = Name

d-r7dcmwzl = Fenster
    .doc = Ein Fenster des Schreibtischs: ein Tab einer Leiste im Arbeitsbereich vorn.
    .noun = { $count ->
        [one] Fenster
       *[other] Fenster
    }

d-dfgqyodj = Schlüssel

d-by5gisol = Titel

d-v5xdn5a6 = Ursprungs-App

d-zscvpgbm = Schlüssel der App
    .doc = Die App, die es zeigt, geteilt von zwei Fenstern einer App: der App-Bereich des Wechslers kreuzt die Fenster, die sie halten.

d-drnqlrp5 = Symbol

d-mjnpzf6r = Leiste

d-g3ddlvlc = Schlüssel der Leiste
    .doc = Die Kennung seiner Leiste, wie das Verschieben eines Fensters dorthin die Leiste nennt.

d-hnoh7dgq = Arbeitsbereich

d-c7yq7qkl = Schlüssel des Arbeitsbereichs
    .doc = Die Kennung seines Arbeitsbereichs, wie das Wechseln dorthin den Arbeitsbereich nennt.

d-bard6ca3 = Schlüssel der Gruppe
    .doc = Das Fenster, dessen Tab es ist: die Tabs einer Gruppe sind ein Fenster auf dem Schreibtisch, und ihre anderen Tabs stehen dahinter.

d-lbxt5665 = Index des Tabs
    .doc = Sein Platz unter den Tabs seiner Gruppe: 0 für den Tab vorn, dann die Tabs dahinter in ihrer Reihenfolge.

d-kiwhpw3z = Anteil
    .doc = Sein Anteil an der Breite seiner Leiste, 0..1: die Breite, die das Fenster auf dem Schreibtisch behält.

d-hm2x5otu = Lokal
    .doc = Ob es auf diesem Gerät läuft: falsch für ein Fenster auf einem anderen Gerät der Person oder auf dem eines Peers.

d-wxi227u2 = Gerät
    .doc = Das Gerät, das es laufen lässt, wenn es nicht dieses ist: sein Schlüssel, leer, wenn er nicht bekannt ist.

d-aadqdflh = Name des Geräts
    .doc = Der Name dieses Geräts, wie sein Zertifikat ihn beschriftet.

d-ctnewsrr = Platz
    .doc = Wo es im Bild seines Arbeitsbereichs steht: 1 für den ersten Tab der ersten Gruppe der ersten Leiste, weiter an ihnen entlang, Leisten von oben nach unten.

d-z3pjamxl = Zuletzt benutzt

d-a5fwclo5 = Im Fokus

d-tb2jierp = Vorschau

d-4agha6k5 = Antwortet nicht
    .doc = Ob seine App aufgehört hat zu antworten: seine Karte sagt es und bietet an, weiter auf die App zu warten oder sie zu beenden.

d-r7dcmwzl-filter-tab_index = Index des Tabs

d-r7dcmwzl-filter-share = Anteil

d-r7dcmwzl-filter-local = Lokal

d-r7dcmwzl-filter-place = Platz

d-r7dcmwzl-filter-recency = Zuletzt benutzt

d-r7dcmwzl-filter-focused = Im Fokus

d-r7dcmwzl-filter-not_responding = Antwortet nicht

d-labby3mp = Agent
    .doc = Ein Agent, der für die Person hier ist oder für sie gehandelt hat.
    .noun = { $count ->
        [one] Agent
       *[other] Agenten
    }

d-njbk3cds = Schlüssel

d-4nhbpuss = Name

d-6zpjzgvl = Zustand

d-zcdb3jtl = Tätigkeit

d-ty5fvze2 = Zeichen

d-gbehvj6x = Ungesehen

d-p6gfsj72 = Sitz
    .doc = Ob er gerade an der Agententür der Person sitzt, ein Client, den sie zugelassen hat, statt der eigene Agent eines Laufs.

d-labby3mp-filter-unseen = Ungesehen

d-labby3mp-filter-seat = Sitz

d-jlqw5py5 = Arbeitsbereich
    .doc = Ein Arbeitsbereich, in dem die Person ist, wie ihn die Übersicht der Arbeitsbereiche nennt.
    .noun = { $count ->
        [one] Arbeitsbereich
       *[other] Arbeitsbereiche
    }

d-wvbsg6yl = Schlüssel

d-wofgn4vi = Name

d-5p6whtgp = Aktuell

d-huv4cci3 = Mitglieder

d-l4d25hj6 = Anwesend

d-no5yi6m7 = Aktivität

d-jlqw5py5-filter-current = Aktuell

d-jlqw5py5-filter-present = Anwesend

d-zk7pzrt2 = Hinweis
    .doc = Etwas, das ein Absender der Person hier gesagt hat: eine App oder die Aufgabe, die es tat.
    .noun = { $count ->
        [one] Hinweis
       *[other] Hinweise
    }

d-jq4onnp4 = Schlüssel

d-7snyky6u = Titel

d-wauytevp = Text

d-nxunr2dc = Ursprungs-App

d-bsp6shbm = Art

d-z67wqj3i = Gesehen

d-zk7pzrt2-filter-seen = Gesehen

d-7rjfrgeo = Hinweisaktion
    .doc = Eine Aktion, die ein Hinweis anbietet, wie ihr Absender sie nannte.
    .noun = { $count ->
        [one] Hinweisaktion
       *[other] Hinweisaktionen
    }

d-ohcc5fba = Hinweis

d-mfyylqsl = Schlüssel

d-6ehy2qwa = Beschriftung

d-qhf2pyze = Anordnung
    .doc = Die Anordnung der Person: was offen ist und wo.
    .noun = { $count ->
        [one] Anordnung
       *[other] Anordnungen
    }

d-rknsknpw = Arbeitsbereiche

d-2aititgj = Zeilen

d-x2vx3spj = Leisten

d-id3ec54i = Plätze
    .doc = Jeder Platz, jeder nennt die Leiste, in der er ist, damit die Ansichten nicht durch den Baum laufen müssen.

d-wydxt4zu = Arbeitsbereich der Anordnung
    .doc = Ein Arbeitsbereich, den der Schreibtisch der Person zeigt.
    .noun = { $count ->
        [one] Arbeitsbereich der Anordnung
       *[other] Arbeitsbereiche der Anordnung
    }

d-6m3fzjng = Schlüssel

d-tljqo2yo = Name

d-3uxxhan3 = Aktuell
    .doc = Ob es der Arbeitsbereich vorn ist, den der Schreibtisch zeigt.

d-wydxt4zu-filter-current = Aktuell

d-k6bv5cxt = Zeile der Anordnung
    .doc = Eine Zeile des Schreibtischs: ein Band über die Breite eines Arbeitsbereichs, das Leisten hält.
    .noun = { $count ->
        [one] Zeile der Anordnung
       *[other] Zeilen der Anordnung
    }

d-idcxp7jb = Schlüssel

d-qassqvic = Arbeitsbereich

d-yppyohd5 = Name

d-awgwtm6j = Shell
    .doc = Die Shell, die sie angelegt hat; leer liest sich als die dieser Shell.

d-vn23musu = Leiste der Anordnung
    .doc = Eine Leiste: eine Spalte aus Plätzen, mit einer Breite, in einer Zeile eines Arbeitsbereichs.
    .noun = { $count ->
        [one] Leiste der Anordnung
       *[other] Leisten der Anordnung
    }

d-heuzka3o = Schlüssel

d-dqpmay7u = Arbeitsbereich

d-r22fgcaa = Zeile

d-pbep7iq6 = Breite
    .doc = Ihre Breite in Promille der nutzbaren Breite ihrer Zeile (333 schmal, 667 normal, 1000 breit).

d-gpviayl3 = Maximiert

d-opotzabz = Aktiv
    .doc = Der Platz vorn, nach seiner Kennung; leer für keinen.

d-54qldnir = Platziert
    .doc = Ob er einen Platz auf der Fläche seines Arbeitsbereichs hat.

d-mnpd23ty = Fläche x

d-yzbtjovv = Fläche y

d-m67g5cso = Breite der Fläche

d-4n7jst3f = Höhe der Fläche

d-n2pmgnow = Shell

d-vn23musu-filter-width = Breite

d-vn23musu-filter-maximized = Maximiert

d-vn23musu-filter-placed = Platziert

d-vn23musu-filter-canvas_x = Fläche x

d-vn23musu-filter-canvas_y = Fläche y

d-vn23musu-filter-canvas_width = Breite der Fläche

d-vn23musu-filter-canvas_height = Höhe der Fläche

d-gpmk4o3r = Platz der Anordnung
    .doc = Ein Platz: ein Fenster, das eine Ressource durch eine Ansicht zeigt.
    .noun = { $count ->
        [one] Platz der Anordnung
       *[other] Plätze der Anordnung
    }

d-ez2plmfg = Schlüssel

d-rsnsojnc = Leiste
    .doc = Die Leiste, in der er ist.

d-uxd54qen = Zeigt
    .doc = Die App, die die Ressource zeigt, nach ihrer kanonischen Kennung (`so.tilo.terminal`).

d-jecpdz4d = Gezeigt als
    .doc = Die Ansicht, durch die sie die Ressource zeigt; leer für die Vorgabe der App.

d-yesjoqbj = Datensatz
    .doc = Der Datensatz, den er zeigt, wenn er einen zeigt.

d-iu3wwk2u = Titel
    .doc = Der Titel, unter dem er gezeigt wird.

d-joj2ey7d = Name
    .doc = Wie dieser Platz heißt, auf jeder Oberfläche: der Titel seines Fensters und der eigene Name seiner App (`Groceries` in `Notes`, den die Person gewählt hat), oder der Name der App allein, wenn das Fenster keinen Titel hat.

d-aonjenrr = Angeheftet
    .doc = Ob die Person ihn angeheftet hat: die Anordnung eines Agenten lässt ihn in Ruhe.

d-rqrtd6vq = Shell
    .doc = Die Shell, die ihn angelegt hat; leer liest sich als die dieser Shell.

d-gpmk4o3r-filter-pinned = Angeheftet

d-nkjtmr33 = Sicht
    .doc = Was die Person von ihrer Anordnung zeigt und welche Leiste vorn steht: die eigene Sicht der Shell darauf, in ihrem eigenen Datensatz (`records: .own`, `root viewing: Viewing`).
    .noun = { $count ->
        [one] Sicht
       *[other] Sichten
    }

d-kna5sco4 = Stil
    .doc = Der Schreibtischstil, den die Person zeigt, als Ansichtsreferenz: ein Paket und eine Ansicht (`so.tilo.shell:strips`, `me.styles:timeline`).

d-nl27ikjq = Zeigt
    .doc = Die Vorgabe, die die Person zeigte, bevor Stile Ansichten waren.

d-6b2coqll = Im Fokus
    .doc = Die Leiste vorn, nach ihrem Schlüssel; leer für keine.

d-nkjtmr33-filter-showing = Zeigt

d-cejnvssb = Shell
    .doc = Der eigene Shell-Datensatz der Person, erreichbar als `shell` (`root shell: Shell`).
    .noun = { $count ->
        [one] Shell
       *[other] Shells
    }

d-qsrjrjkd = Vorgabe
    .doc = Die Vorgabe, aus der die Einstellungen zuletzt gemacht wurden; `tuned` sagt, ob die Person seither eine Einstellung geändert hat.

d-2se66uzq = Basiert auf
    .doc = Die Vorgabe, aus der die Einstellungen zuletzt gemacht wurden, behalten, während die Person eine Sache einstellt, damit die Auswahl „Eigene, basierend auf …“ sagt, statt zu verlieren, wo sie angefangen hat.

d-nt6y5bk3 = Eingestellt
    .doc = Ob die Person seit dem Wählen einer Vorgabe eine Einstellung geändert hat.

d-kqnswwzm = Leiste gezeigt
    .doc = Die obere Leiste.

d-mi3yni2m = Kante der Leiste

d-la4yrobd = Sitz der Leiste

d-6ujrnpej = Ausführung der Leiste

d-b2fcmpwg = Zeigen der Leiste

d-773tn644 = Größe der Leiste

d-voqg5seo = Leistenteile

d-b7grsgnv = Dock gezeigt
    .doc = Das Dock.

d-o6tdqxq3 = Kante des Docks

d-dq6kzv2v = Sitz des Docks

d-y5rlj3v3 = Ausführung des Docks

d-nz74n3iu = Zeigen des Docks

d-wx25jsef = Größe des Docks

d-hx5alotd = Dock-Einträge

d-ffa2ohzm = Laufende im Dock
    .doc = Das Dock zeigt die laufenden Apps auch, wenn niemand sie angeheftet hat.

d-h5oo7js2 = Launcher im Dock
    .doc = Das Dock hat einen Launcher-Eintrag.

d-5n4s55tz = Dock-Zoom
    .doc = Ein Klick auf die Symbole des Docks vergrößert den Eintrag unter dem Zeiger, wie es das Dock von macOS tut.

d-cejnvssb-filter-preset = Vorgabe

d-cejnvssb-filter-based_on = Basiert auf

d-cejnvssb-filter-tuned = Eingestellt

d-cejnvssb-filter-bar_shown = Leiste gezeigt

d-cejnvssb-filter-bar_edge = Kante der Leiste

d-cejnvssb-filter-bar_fit = Sitz der Leiste

d-cejnvssb-filter-bar_finish = Ausführung der Leiste

d-cejnvssb-filter-bar_showing = Zeigen der Leiste

d-cejnvssb-filter-bar_size = Größe der Leiste

d-cejnvssb-filter-dock_shown = Dock gezeigt

d-cejnvssb-filter-dock_edge = Kante des Docks

d-cejnvssb-filter-dock_fit = Sitz des Docks

d-cejnvssb-filter-dock_finish = Ausführung des Docks

d-cejnvssb-filter-dock_showing = Zeigen des Docks

d-cejnvssb-filter-dock_size = Größe des Docks

d-cejnvssb-filter-dock_running = Laufende im Dock

d-cejnvssb-filter-dock_launcher = Launcher im Dock

d-cejnvssb-filter-dock_zoom = Dock-Zoom

d-sfhd6wko = Leistenteil
    .doc = Ein Teil der oberen Leiste, in der Reihenfolge der Person.
    .noun = { $count ->
        [one] Leistenteil
       *[other] Leistenteile
    }

d-fjzbw5un = Slot
    .doc = Was das Teil ist; die Leiste zeichnet es daraus, sodass ein verschobenes Teil dasselbe Teil ist.

d-v3iwpjov = Titel
    .doc = Sein Name auf der Leiste und in den Einstellungen.

d-pbly7ymy = Gezeigt
    .doc = Ob es auf der Leiste zeigt; ein ausgeschaltetes Teil behält seinen Platz im Datensatz.

d-sfhd6wko-filter-slot = Slot

d-sfhd6wko-filter-shown = Gezeigt

d-bnfk7mx5 = Dock-Eintrag
    .doc = Eine Sache auf dem Dock: eine angeheftete App oder Ressource, der Launcher oder eine gerade laufende App.
    .noun = { $count ->
        [one] Dock-Eintrag
       *[other] Dock-Einträge
    }

d-rfsefuzv = Schlüssel
    .doc = Wofür der Eintrag steht: eine App-Kennung oder die Referenz einer Ressource als Text.

d-qsnrl2gk = Titel
    .doc = Sein Name auf dem Dock und was assistive Technologie vorliest.

d-6gyb7csc = Symbol
    .doc = Das Zeichen, das er zeigt, nach dem Namen seines Symbols.

d-k376lylv = Angeheftet
    .doc = Ob die Person ihn angeheftet hat; eine laufende App, die nicht angeheftet ist, ist da, solange sie läuft.

d-5vpgvzqb = Laufend
    .doc = Ob er jetzt läuft.

d-mgcmvxjx = Abzeichen
    .doc = Wie viele Dinge neu sind, wie sein Abzeichen zeigt.

d-e7ft4mly = Aufmerksamkeit
    .doc = Ob er die Person will: ein pulsierendes Abzeichen und ein Aufmerksamkeitszeichen.

d-bnfk7mx5-filter-pinned = Angeheftet

d-bnfk7mx5-filter-running = Laufend

d-bnfk7mx5-filter-badge = Abzeichen

d-bnfk7mx5-filter-attention = Aufmerksamkeit

d-clhqzrkb = Breite
    .doc = Wie breit eine Leiste in ihrer Zeile steht, in Promille der Zeilenbreite.

d-hrjwxtwf = Drei Zehntel der Zeile: eine Liste, eine Reihe von Optionen
    .doc = Drei Zehntel der Zeile: eine Liste, eine Reihe von Optionen.

d-ipingyqi = Zwei Drittel der Zeile: eine Notiz, eine Unterhaltung
    .doc = Zwei Drittel der Zeile: eine Notiz, eine Unterhaltung.

d-nfufnx7j = Die ganze nutzbare Breite der Zeile
    .doc = Die ganze nutzbare Breite der Zeile.

d-vvznzcd4 = Vorgabe
    .doc = Eine Art, dieselben Ressourcen und Aktionen anzuordnen, die eine Person gewählt hat, bevor der Schreibtisch Stile hatte: `Viewing.showing` hielt eine.

d-25lm42aq = Spalten aus Plätzen, waagerecht gescrollt, mit Leisten gestapelt und senkrecht gescrollt
    .doc = Spalten aus Plätzen, waagerecht gescrollt, mit Leisten gestapelt und senkrecht gescrollt.

d-3ha7yyn2 = Plätze an Punkten auf einer verschiebbaren, zoombaren Fläche, jeder in einem Rahmen gezeichnet
    .doc = Plätze an Punkten auf einer verschiebbaren, zoombaren Fläche, jeder in einem Rahmen gezeichnet.

d-whadpf2h = Plätze an Punkten, jeder als eigene Ansicht gezeichnet: Ressourcen stehen als Inhalt auf einer Fläche, gruppiert danach, wo sie sind, statt nach einem Rahmen um sie
    .doc = Plätze an Punkten, jeder als eigene Ansicht gezeichnet: Ressourcen stehen als Inhalt auf einer Fläche, gruppiert danach, wo sie sind, statt nach einem Rahmen um sie.

d-6uzzl3rd = Der traditionelle Schreibtisch: Rahmen auf einem schwebenden Schreibtisch, die eigenen Befehle des fokussierten Fensters in der oberen Leiste und ein schwebendes Dock am Fuß, die Form, die Menschen von macOS und GNOME kennen
    .doc = Der traditionelle Schreibtisch: Rahmen auf einem schwebenden Schreibtisch, die eigenen Befehle des fokussierten Fensters in der oberen Leiste und ein schwebendes Dock am Fuß, die Form, die Menschen von macOS und GNOME kennen.

d-nxjnwtsd = Kante
    .doc = Die Shell der Person: welche Teile die obere Leiste und das Dock haben, in welcher Reihenfolge, an welcher Kante, wie sie aussehen und wann sie zeigen.

d-rbzbofjd = Oben

d-e4zlwr65 = Unten

d-tzvst7nv = Links

d-qykb6xix = Rechts

d-frrucget = Sitz
    .doc = Wie eine Leiste oder ein Dock in seiner Kante sitzt: eingesetzt mit runden Ecken, schwebend über dem, was es deckt, oder von Kante zu Kante und eckig.

d-kheriztb = Schwebend

d-ztqim6ly = Voll

d-luthoy6b = Ausführung
    .doc = Woraus eine Leiste oder ein Dock ist: das durchscheinende Material des Themas über dem, was dahinter ist, oder sein deckender Leistenton.

d-ipplqs3o = Durchscheinend

d-bc2qov6r = Deckend

d-rfgelicn = Zeigen
    .doc = Wann eine Leiste oder ein Dock zeigt.

d-p4owlmpg = Immer

d-sscc6gs3 = Gezeigt, solange kein Fenster sie überlappt; sie verbirgt sich, wenn ein Fenster sie decken würde
    .doc = Gezeigt, solange kein Fenster sie überlappt; sie verbirgt sich, wenn ein Fenster sie decken würde.

d-v2eyzsgr = Verborgen, bis der Zeiger an die Bildschirmkante stößt, an der sie fährt
    .doc = Verborgen, bis der Zeiger an die Bildschirmkante stößt, an der sie fährt.

d-62fazfo4 = Verborgen, wenn der Zeiger eine Weile nicht darauf war, gezeigt, wenn er zurückkommt
    .doc = Verborgen, wenn der Zeiger eine Weile nicht darauf war, gezeigt, wenn er zurückkommt.

d-v3z2zasp = Größe
    .doc = Wie groß die Teile sind.

d-2yxm7xfn = Klein

d-uv33v3c2 = Normal

d-5czgy2f6 = Groß

d-vtiohdqj = Slot
    .doc = Ein Teil, das eine Person auf die obere Leiste legen kann, in ihrer Reihenfolge.

d-2k6zzh2i = Das Menü des Systems: Tilo und die eigenen Befehle des Geräts
    .doc = Das Menü des Systems: Tilo und die eigenen Befehle des Geräts.

d-ujo3oxvs = Der Arbeitsbereich vorn, der die Arbeitsbereiche zum Wechseln öffnet
    .doc = Der Arbeitsbereich vorn, der die Arbeitsbereiche zum Wechseln öffnet.

d-e43glabw = Die Menüs der fokussierten App: was diese App jetzt mit ihrer Ressource tun kann
    .doc = Die Menüs der fokussierten App: was diese App jetzt mit ihrer Ressource tun kann.

d-bvj3ji7x = Die Leiste vorn
    .doc = Die Leiste vorn.

d-tqo2gugp = Die für die Person laufenden Aufgaben
    .doc = Die für die Person laufenden Aufgaben.

d-j2kj3zvb = Die Zeit
    .doc = Die Zeit.

d-ntj3ee4g = Wer sonst hier ist, die Person und die Liste aller
    .doc = Wer sonst hier ist, die Person und die Liste aller.

d-bd6dyvh6 = Der Zustand des Geräts (der Ton, die Verbindung und der Akku) und die Benachrichtigungen
    .doc = Der Zustand des Geräts (der Ton, die Verbindung und der Akku) und die Benachrichtigungen.

d-z4jepckl = Der Akku allein, zurückgezogen: das Statusteil zeigt ihn jetzt
    .doc = Der Akku allein, zurückgezogen: das Statusteil zeigt ihn jetzt.

d-ezboaery = Der eigene Anzeiger des Agenten: eine Aufgabe, die für die Person arbeitet
    .doc = Der eigene Anzeiger des Agenten: eine Aufgabe, die für die Person arbeitet.

d-fqaqauwe = Eine flexible Lücke: die Teile der Leiste davor führen, die danach folgen
    .doc = Eine flexible Lücke: die Teile der Leiste davor führen, die danach folgen.

d-oomdh5mi = Layout
    .doc = Ein benannter Satz von Einstellungen, den die Person in einer Aktion anwendet: eine Vorgabe für den Rahmen.

d-gdzkj7tc = Das Dock unten mit der Leiste oben: Tilos eigene
    .doc = Das Dock unten mit der Leiste oben: Tilos eigene.

d-b6bz7ze6 = Ein schwebendes, rundes, durchscheinendes Dock unten, das sich verbirgt, solange ein Fenster es deckt, und eine obere Leiste mit den Menüs der fokussierten App nach denen des Systems
    .doc = Ein schwebendes, rundes, durchscheinendes Dock unten, das sich verbirgt, solange ein Fenster es deckt, und eine obere Leiste mit den Menüs der fokussierten App nach denen des Systems.

d-k4de63ud = Eine Taskleiste über die volle Breite unten, immer gezeigt
    .doc = Eine Taskleiste über die volle Breite unten, immer gezeigt.

d-cqbe6g3k = Die obere Leiste allein; die Übersicht auf Abruf
    .doc = Die obere Leiste allein; die Übersicht auf Abruf.

d-hyr6xrfo = Kein Rahmen in Ruhe; die Kanten zeigen, was die Person braucht
    .doc = Kein Rahmen in Ruhe; die Kanten zeigen, was die Person braucht.

d-poagvor7 = Der traditionelle Schreibtisch: Rahmen auf einem schwebenden Schreibtisch, die eigenen Befehle des fokussierten Fensters in der oberen Leiste und ein schwebendes Dock am Fuß, die Form, die Menschen von macOS und GNOME kennen
    .doc = Der traditionelle Schreibtisch: Rahmen auf einem schwebenden Schreibtisch, die eigenen Befehle des fokussierten Fensters in der oberen Leiste und ein schwebendes Dock am Fuß, die Form, die Menschen von macOS und GNOME kennen.

d-q7rxwd72 = Platz der Leiste
    .doc = Wie viel Platz die obere Leiste hat: für das Menü des fokussierten Fensters und für ihre Teile.

d-w3dorpbd = Am meisten: das Menü nach dem Namen seines Fensters, auch die Aufgaben und wer hier ist
    .doc = Am meisten: das Menü nach dem Namen seines Fensters, auch die Aufgaben und wer hier ist.

d-qqg4smi5 = Das Menü nach dem Namen seines Fensters, aber nicht die Aufgaben und wer hier ist
    .doc = Das Menü nach dem Namen seines Fensters, aber nicht die Aufgaben und wer hier ist.

d-7caakb5b = Am wenigsten: das Menü steht als Symbol des Fensters, und nur, was ein Telefon behält, bleibt
    .doc = Am wenigsten: das Menü steht als Symbol des Fensters, und nur, was ein Telefon behält, bleibt.

d-p4bqyuzt = Stil benennen
    .doc = Ein Schreibtischstil, gewählt, bevor Stile Ansichten waren: der Name der Vorgabe, gewendet in die Ansicht dieses Pakets davon (`strips` zu `so.tilo.shell:strips`), damit die Wahl der Person bleibt.

d-7j7tzynq = Zeigen als
    .doc = Zeigt den Schreibtisch durch den Stil `style`: ein Paket und eine Ansicht (`so.tilo.shell:strips`).
    .w-it-is-shown-that-way-already = Er wird schon so gezeigt

d-7j7tzynq-style = Stil

d-dkylvhw4 = Nach vorn
    .doc = Macht die Leiste `strip` zur vorderen: was ein Druck auf die Kopfzeile einer Spalte tut.

d-dkylvhw4-strip = Leiste

d-zoqbhs7o = Einrichten
    .doc = Eine Shell, die nie angeordnet wurde: die Teile der Leiste und des Docks, in der Reihenfolge, in der die voreingestellte Shell sie zeigt.

d-vglzmkiy = Das Ein/Aus-Teil entfernen
    .doc = Eine Shell, die das zurückgezogene Teil `power` neben dem Status behielt, verliert es.

d-g7yuomid = Das Aufgaben-Teil entfernen
    .doc = Eine Shell, die das zurückgezogene Teil `tasks` behielt, verliert es: was ein Agent tut, ist der Zug einer Unterhaltung, den Chat zeigt, also hat die Leiste keinen Haufen laufender Aufgaben.

d-eanbdi6l = Dieses Layout verwenden
    .doc = Verwendet die Vorgabe `layout`: jede Einstellung, die sie nennt, auf einmal.

d-eanbdi6l-layout = Layout

d-rpbpwrcy = Die Menüs der App zeigen
    .doc = Legt die Menüs der fokussierten App auf die Leiste, nach denen des Systems: was die App vorn jetzt mit ihrer Ressource tun kann.
    .w-the-app-s-menus-are-on = Die Menüs der App sind schon auf der Leiste

d-7jggsdfm = Die Menüs der App verbergen
    .doc = Nimmt die Menüs der fokussierten App von der Leiste.

d-2k74oe7b = Zurück zur Vorgabe
    .doc = Bringt die Shell auf die Vorgabe zurück, auf der sie basierte, und lässt die Einstellungen der Person fallen.
    .w-nothing-was-changed-from-the-preset = Es wurde nichts gegenüber der Vorgabe geändert

d-jqwkinjd = Leiste an
    .doc = Setzt die Leiste an `edge`.

d-jqwkinjd-edge = Kante

d-u6k2x6mp = Sitz der Leiste setzen

d-u6k2x6mp-fit = Sitz

d-i2mddhin = Ausführung der Leiste setzen

d-i2mddhin-finish = Ausführung

d-w4d7htty = Zeigen der Leiste setzen

d-w4d7htty-showing = Zeigen

d-fdnwo2gt = Größe der Leiste setzen

d-fdnwo2gt-size = Größe

d-fcxzqnum = Die obere Leiste zeigen
    .doc = Zeigt oder verbirgt die obere Leiste: das eine Bedienelement, das sie ein- und ausschaltet, sodass eine Person sieht, ob sie an ist, wo sie drückt.

d-fcxzqnum-on = Auf dem Schreibtisch

d-5l5rmk47 = Dock an
    .doc = Verschiebt das Dock an `edge` und behält, wie es gebaut ist.

d-5l5rmk47-edge = Kante

d-jllttdkc = Sitz des Docks setzen

d-jllttdkc-fit = Sitz

d-uw5xrdst = Ausführung des Docks setzen

d-uw5xrdst-finish = Ausführung

d-iyws3u4w = Zeigen des Docks setzen

d-iyws3u4w-showing = Zeigen

d-quwdhi4n = Größe des Docks setzen

d-quwdhi4n-size = Größe

d-23faedwo = Das Dock zeigen
    .doc = Zeigt oder verbirgt das Dock: das eine Bedienelement, das es ein- und ausschaltet, sodass eine Person sieht, ob es an ist, wo sie drückt.

d-23faedwo-on = Auf dem Schreibtisch

d-exyxrzml = Laufende Apps
    .doc = Ob das Dock die laufenden Apps zeigt, die niemand angeheftet hat.

d-exyxrzml-on = Im Dock

d-vlnaqn4u = Launcher-Eintrag
    .doc = Ob das Dock einen Launcher-Eintrag hat.

d-vlnaqn4u-on = Im Dock

d-z2w4wlev = Das Dock vergrößern
    .doc = Ob das Dock den Eintrag unter dem Zeiger vergrößert.

d-z2w4wlev-on = Unter dem Zeiger

d-vurzkuat = Verschieben
    .doc = Setzt ein Leistenteil zu dem, vor das es kommt, wo das Teil jetzt auch steht: die eine Aktion, die ein Ziehen abschickt, und dieselbe Änderung aus den Einstellungen, einem Tastenkürzel oder von einem Agenten.
    .w-a-part-cannot-come-before-itself = Ein Teil kann nicht vor sich selbst stehen

d-vurzkuat-part = Teil

d-vurzkuat-before = Vor

d-4ljvsals = Verschieben
    .doc = Setzt ein Leistenteil an die Position `position`, von der führenden Kante gezählt: was ein Ziehen gibt, das einen Platz nennt.

d-4ljvsals-part = Teil

d-4ljvsals-position = Position

d-5esgamkb = Auf der Leiste gezeigt
    .doc = Zeigt oder verbirgt ein Teil der Leiste.

d-5esgamkb-part = Teil

d-5esgamkb-on = Auf der Leiste

d-hphul6cv = Verschieben
    .doc = Setzt einen Dock-Eintrag zu dem, vor den er kommt: die eine Aktion, die ein Ziehen abschickt.
    .w-an-item-cannot-come-before-itself = Ein Eintrag kann nicht vor sich selbst stehen

d-hphul6cv-item = Eintrag

d-hphul6cv-before = Vor

d-tv7bsor6 = Verschieben
    .doc = Setzt einen Dock-Eintrag an die Position `position`, von der führenden Kante gezählt: was ein Ziehen gibt, das einen Platz nennt.

d-tv7bsor6-item = Eintrag

d-tv7bsor6-position = Position

d-utk4ewgf = Ans Dock anheften
    .doc = Heftet eine Ressource oder eine App an das Dock, vor `next` (oder am Ende, wenn kein Nachbar genannt ist): was das Ziehen einer Sache auf das Dock tut und was die Person aus einem Menü tut.
    .w-is-on-the-dock-already = { $title } ist schon auf dem Dock

d-utk4ewgf-key = App

d-utk4ewgf-title = Titel

d-utk4ewgf-icon = Symbol

d-utk4ewgf-next = Vor

d-5yz5iwzs = Vom Dock entfernen
    .doc = Nimmt einen Eintrag vom Dock.

d-5yz5iwzs-item = Eintrag

d-aitkuflo = Eintrag öffnen
    .doc = Was ein Druck auf einen Dock-Eintrag tut, nach der App, für die der Eintrag steht: ein Fenster dieser App auf diesem Schreibtisch kommt nach vorn, sodass der Schreibtisch sagt, in welchem Fenster die Person ist, und die Befehle der oberen Leiste seine eigenen sind; das Fenster ist das, für das der Eintrag gezeichnet wurde, sodass ein Druck nie eine zweite Kopie öffnet.

d-aitkuflo-desk = Schreibtisch

d-aitkuflo-key = App

d-vjnksgzm = Wartendes anlegen
    .doc = Zwei Hinweise, die ein Absender hinterließ und die Person nicht gesehen hat: was die Shell auf einem Schreibtisch bereitstellt, den sie zeigt, bevor die Person aufgeholt hat, damit die Glocke der Leiste etwas zu zählen hat.

d-j4ax4otm = Hinweise anlegen
    .doc = Die Hinweise des Schreibtischs, von denen ein Szenario ausgeht: was Apps und ein Agent heute gesagt haben.

d-zm35wbjg = Ein Arbeitsbereich
    .doc = Fügt `a` einen Arbeitsbereich mit einer Zeile hinzu und gibt den Schlüssel der Zeile zurück.

d-zm35wbjg-name = Name

d-ztflhbbf = Eine Leiste
    .doc = Fügt `row` eine Leiste namens `name` hinzu, stehend bei `width`, und gibt ihren Schlüssel zurück.

d-ztflhbbf-row = Zeile

d-ztflhbbf-name = Name

d-ztflhbbf-width = Breite

d-chvuqgur = Ein Notizplatz
    .doc = Fügt `strip` einen Platz hinzu, der die Notiz an `index` des Notizbuchs zeigt, und gibt seinen Schlüssel zurück.

d-chvuqgur-strip = Leiste

d-chvuqgur-index = Index

d-tnecawb6 = Ein fremder Platz
    .doc = Ein Platz einer Art, für die dieser Stand keinen Datensatz hat: er steht, wie er ist, so wie ein Platz aussieht, den eine andere Shell gemacht hat.

d-tnecawb6-strip = Leiste

d-tnecawb6-index = Index

d-tnecawb6-title = Titel

d-b3rfswle = Platz zeigen
    .doc = Zeigt den Platz `place` und holt seine Leiste mit nach vorn: was das Wählen eines Tabs tut und dieselbe Anordnungsaktion, die ein Skript oder ein Agent ausführt.

d-b3rfswle-strip = Leiste

d-b3rfswle-place = Platz

d-i6ll2n4j = Schreibtischansicht

d-72tgr4px = Uhr
    .w-paused = Pausiert

d-3ezewqgy = Leiste
    .w-top-bar = Obere Leiste

d-pvl6wwx6 = Leistenteil
    .w-tilo = Tilo

d-vidcax4k = Arbeitsbereichsmenü
    .w-workspaces = Arbeitsbereiche
    .w-new-workspace = Neuer Arbeitsbereich
    .w-join-a-workspace = Einem Arbeitsbereich beitreten
    .w-workspace-settings = Einstellungen des Arbeitsbereichs
    .w-overview = Übersicht

d-hac3p23j = Lautstärke
    .w-volume-muted = Lautstärke, stumm
    .w-volume = Lautstärke, { $percent } %

d-iwv7cd5i = Netzwerk
    .w-wi-fi = WLAN, { $called }
    .w-wi-fi-2 = WLAN
    .w-wired = Kabel, { $called }
    .w-wired-2 = Kabel
    .w-offline = Offline

d-agni63m4 = Akku
    .w-charging = { $percentage } %, wird geladen

d-moh3hzne = Glocke
    .w-notifications-silenced = Benachrichtigungen stumm
    .w-notifications = Benachrichtigungen
    .w-1-unread = { $name }, 1 ungelesen
    .w-unread = { $name }, { $waiting } ungelesen

d-fniigjed = Einstellungen der Sicht
    .w-desk = Schreibtisch
    .w-switching-a-style-keeps-every-window = Ein Stilwechsel lässt jedes Fenster, wo es ist.

d-gapwwnqc = Stil zeigen~present

d-s7yqm7cb = Fläche
    .w-find-something-or-ask = Finde etwas oder frag

d-t3cux3yd = Gerahmter Platz

d-s6b5stgz = Inhalt der Fläche

d-ewgdlyyr = App-Menüs zeigen~present

d-npxnqobp = Zurücksetzen~present

d-vpc43pes = Leiste zeigen~present

d-nn5zjt6d = Dock zeigen~present

d-vedyud4d = Teil verschieben~present

d-gkca5uj2 = Eintrag verschieben~present

d-i4odzov3 = Anheften~present

d-gctjtdv7 = Schreibtisch

d-zzd3yipu = Leisten

d-tj4z2obq = Rahmen
    .w-canvas = Fläche

d-tqbdxbtn = Flächen
    .w-spaces = Flächen

d-pbhumuny = Desktop

d-gb3g4lte = Dockuntergrund
    .w-dock = Dock

d-afjfzjl6 = Leiste an der Kante
    .w-top-bar = Obere Leiste

d-su7ogm3c = Obere Leiste

d-37bjhdfb = In Ruhe

d-6a3axsap = Oberfläche

d-ojscef66 = Dock

d-lwre5gd7 = Dock-Lauf

d-fr6w4hgp = Kacheln laufender Apps

d-lwt7iwsj = Dock-Kachel
    .w-new = { $title }, { $badge } neu

d-dpqvjh2f = Launcher
    .w-search-apps-windows-actions-and-files = Suche Apps, Fenster, Aktionen und Dateien
    .w-windows = Fenster
    .w-recent-windows = Letzte Fenster
    .w-nothing-here-matches = Nichts hier passt
    .w-search-everywhere-for-it = Überall danach suchen
    .w-open-as-a-tab = Als Tab öffnen
    .w-search-everywhere-for = Überall nach { $query } suchen
    .w-apps = Apps
    .w-all-apps = Alle Apps

d-tst43bqy = Fensterzeile

d-hlv2uw7o = Türen des Schreibtischs
    .w-the-desk = Der Schreibtisch
    .w-overview = Übersicht
    .w-workspaces = Arbeitsbereiche
    .w-notifications = Benachrichtigungen

d-outvdzmi = Layouts
    .w-layout = Layout

d-5q3okayl = Layoutkarte
    .w-in-use = In Gebrauch

d-mwph24zz = Vorschau

d-cpowx55e = Vorschauleiste
    .w-file = Datei
    .w-edit = Bearbeiten
    .w-view = Ansicht

d-ebrt7kdg = Vorschaudock

d-grqdyggg = Einstellungen der Shell
    .w-top-bar = Obere Leiste
    .w-show-the-top-bar = Die obere Leiste zeigen
    .w-dock = Dock
    .w-show-the-dock = Das Dock zeigen

d-p7ismolp = Leistenteile
    .w-parts-in-order = Teile, in Reihenfolge

d-6phinc3w = Teilzeile
    .w-shown = Gezeigt
    .w-hidden = Verborgen

d-e25dashr = Dockeinstellungen
    .w-running-apps = Laufende Apps
    .w-launcher-entry = Launcher-Eintrag
    .w-magnify-the-item-under-the-pointer = Den Eintrag unter dem Zeiger vergrößern

d-clgbrhsn = Hinweise
    .w-notifications = Benachrichtigungen
    .w-clear-all = Alle löschen
    .w-nothing-to-catch-up-on = Nichts aufzuholen
    .w-search-notifications = Benachrichtigungen durchsuchen
    .w-nothing-matches-that = Nichts passt dazu
    .w-clear-the-search-to-see-every = Lösche die Suche, um jeden Hinweis zu sehen

d-wud3t5ro = Hinweisgruppe

d-t3ywtwqj = Hinweiszeile
    .w-dismiss = Verwerfen

d-yyk2izra = Banner

d-gke2gvrj = Banner ganz
    .w-open = Öffnen

d-wh7n4fhq = Bannerzeile
    .w-dismiss = Verwerfen

d-rcjzvfxp = Übersicht
    .w-overview = Übersicht
    .w-new-workspace = Neuer Arbeitsbereich
    .w-join-a-workspace = Einem Arbeitsbereich beitreten
    .w-search-windows = Fenster durchsuchen
    .w-no-windows = Keine Fenster
    .w-open-something-to-see-its-window = Öffne etwas, um sein Fenster zu sehen

d-f5tl6n63 = Arbeitsbereiche hier
    .w-here = { $len } hier

d-7nxhynyj = Bild

d-nxpbvfox = Fensterplatz

d-6l3wldtm = Fenstermenü
    .w-this-window = Dieses Fenster
    .w-go-there = Dorthin
    .w-close = Schließen

d-6nn35yb7 = Gefunden
    .w-nothing-matches = Nichts passt
    .w-windows = Fenster

d-kxdzjhoo = Fensterzeile
    .w-this-window = Dieses Fenster
    .w-go-there = Dorthin
    .w-move-here = Hierher verschieben

d-luuijvwf = Tastenhinweise Launcher
    .w-up = Hoch
    .w-down = Runter
    .w-choose = Wählen
    .w-enter = Eingabe
    .w-open = Öffnen
    .w-esc = Esc
    .w-close = Schließen

d-z4hw2mcw = Tastenhinweise Wechsler
    .w-alt = Alt
    .w-tab = Tab
    .w-walk = Weiter
    .w-enter = Eingabe
    .w-go-there = Dorthin
    .w-esc = Esc
    .w-stay = Bleiben

d-fehlcpo5 = Hängendes Fenster
    .w-not-responding = Antwortet nicht
    .w-wait = Warten
    .w-force-quit = Zum Beenden zwingen

d-uhzjvuan = Personenknopf
    .w-people = Personen
    .w-is-here = { $name } ist hier
    .w-and-are-here = { $name } und { $name2 } sind hier
    .w-and-others-are-here = { $name } und { $others } weitere sind hier

d-uvwnwa4d = Agenten-Chip
    .w-stop = Stopp

d-teccuwja = Schnelleinstellungen
    .w-quick-settings = Schnelleinstellungen

d-yxl6yq2r = WLAN-Kachel
    .w-wi-fi = WLAN

d-d5wzqn37 = Bluetooth-Kachel
    .w-bluetooth = Bluetooth

d-uzi5ah2q = Dunkel-Kachel
    .w-dark-mode = Dunkler Modus

d-s66oqjwe = Ruhe-Kachel
    .w-do-not-disturb = Nicht stören

d-w3fe7tjv = Pegel
    .w-volume = Lautstärke
    .w-brightness = Helligkeit

d-lcjohgdk = Akkulinie

d-ycck5awo = Ein/Aus-Zeile
    .w-lock = Sperren
    .w-power = Ein/Aus
    .w-sleep = Ruhezustand
    .w-log-out = Abmelden
    .w-restart = Neustart
    .w-shut-down = Ausschalten

d-wdjxaekq = Arbeitsbereiche
    .w-workspaces = Arbeitsbereiche
    .w-name-a-new-workspace = Benenne einen neuen Arbeitsbereich
    .w-create = Erstellen
    .w-join-a-workspace = Einem Arbeitsbereich beitreten

d-wcb3o55u = Arbeitsbereichskarte

d-xzb2gybw = Stilumschalter

d-kssxjs7v = Stilmenü
    .w-showing = Zeigt { $style_title }
    .w-settings-for = Einstellungen für { $style_title }

d-qrahog47 = Leistenansicht
    .w-no-window-here-yet = Noch kein Fenster hier

d-4qeo26jr = Zeilen der Arbeitsbereiche

d-7qrrtifg = Zeilen des Schreibtischs
    .w-no-window-here-yet = Noch kein Fenster hier

d-7sxqbtn2 = Zeilenfenster

d-2fofsa7f = Leistenfenster
    .w-nothing-here = Nichts hier

d-p5wzoqr5 = Fensterbedienelemente
    .w-narrow = Schmal
    .w-normal = Normal
    .w-wide = Breit
    .w-bring-to-front = Nach vorn holen
    .w-unpin-window = Fenster lösen
    .w-pin-window = Fenster anheften
    .w-restore = Wiederherstellen
    .w-maximize = Maximieren
    .w-close = Schließen
    .w-this-window = Dieses Fenster

d-bmd7xmln = Ein Platz für eine Suche
    .doc = Fügt `strip` einen Platz hinzu, der eine Suche nach `text` zeigt, mit den beiden Treffern, die die Notizen-App geantwortet hat, und gibt den Schlüssel des Platzes zurück: der Zustand, in dem ein Platz steht, bis die Person eine der gefundenen Ressourcen wählt.

d-bmd7xmln-strip = Spalte

d-bmd7xmln-text = Text

d-yd725hjw = Die installierten Apps
    .doc = Die auf diesem Gerät installierten Anwendungen, wie der Launcher sie auflistet: die, die die Aufnahmen und die Layouts des Schreibtischs zeigen.

d-gncm64rz = Stileinstellungen als Blatt
    .w-settings-for = Einstellungen für { $style_title }
    .w-close = Schließen

d-mmqndkio = Einstellungen eines Stils
    .w-comes-from-and-its-settings-are = { $style_label } kommt aus { $package }, und seine Einstellungen gehören seinem Paket.

d-5o2gn6hf = Einstellungen eines Stils
    .doc = Ein Schreibtisch mit einer Notiz im genannten Stil, dieser Platz vorn, und die eigenen Einstellungen des Stils darüber geöffnet: was eine Person sieht, die sie aus dem Stilmenü der Leiste öffnet.

d-5o2gn6hf-style = Stil

d-z4vc77e6 = Bedienelemente des Platzes
    .w-this-window = Dieses Fenster
    .w-pin-place = Platz anheften
    .w-close = Schließen

d-oyjbbzbk = Inhalt des Platzes

d-43am22do = Ansicht des Platzes

d-fziuz57n = Wechsler
    .w-of = { $at } von { $count }
    .w-no-windows = Keine Fenster
    .w-open-something-to-switch-to-it = Öffne etwas, um dazu zu wechseln

d-dswgb57x = Fensterkarte

d-jzxijptr = Fensternamen

d-3bxeb2i2 = Fensterkopf

d-sk6mvhah = Fensterbild

d-4hp326dv = Traditioneller Schreibtisch
    .w-desktop = Desktop

d-uekksuqv = Vorderes Fenster

d-6izhhsie = Desktop-Rahmen

d-fabwkpdf = Rahmen klassisch

d-efc4bnyi = Rahmenknöpfe
    .w-this-window = Dieses Fenster
    .w-close = Schließen

d-u55srubm = Aktionsleiste

d-cijhlzxf = Fensterbefehle
    .w-close = Schließen

d-yx7doxgr = Traditionelle Leiste
    .w-tilo = Tilo

d-s7dh3s7h = Apps
    .doc = Die auf diesem Gerät installierten Anwendungen: was der Starter auflistet und öffnet.

d-dkwmlyh7 = App
    .doc = Eine auf diesem Gerät installierte Anwendung, wie der Starter sie auflistet: ihre Paket-ID, ihr Name in der Sprache der Person, das Symbol, das sie zeigt, und wie sie sich öffnet.
    .noun = { $count ->
        [one] App
       *[other] Apps
    }

d-wabxc4xe = Schlüssel
    .doc = Ihre Paket-ID, unter der `open_app` sie nennt.

d-dhizmikp = Name
    .doc = Ihr Name in der Oberflächensprache der Person.

d-bkmyu6to = Symbol
    .doc = Das Symbol, das sie zeigt, nach dem Kebab-Case-Namen des Plattformsymbols.

d-fxhx35ly = Kategorie
    .doc = Ihre Kategorie (`work`, `media`, `play`, `system`), leer, wenn sie keine nennt.

d-vebrnbn2 = Beschreibung
    .doc = Wofür sie da ist, in wenigen Worten; leer, wenn sie keine anbietet.

d-if7lamna = Anfragen
    .doc = Die Anfragen, die die Arbeitsfläche aufstellt, während eine App sie beantwortet, nach dem Platz, der jede zeigt: was die Person eingegeben hat, was sie gefunden hat oder warum nichts (shell.md §8).

d-bkx6ffql = Verweise
    .doc = Die Ressourcen, die diese Anfragen gefunden haben, jede mit der Anfrage, zu der sie gehört.

d-7ync2hvz = Anordnungsanfrage
    .doc = Eine Anfrage, die ein Platz zeigt, während ihre App sie beantwortet: was die Person eingegeben hat, in welcher App und was daraus wurde.
    .noun = { $count ->
        [one] Anordnungsanfrage
       *[other] Anordnungsanfragen
    }

d-l6u2kjnd = Schlüssel
    .doc = Ihre ID: der Platz, der sie zeigt.

d-fs44wwaw = Text
    .doc = Was die Person eingegeben hat.

d-tilkofni = In der App
    .doc = Die gefragte App, nach ihrer kanonischen ID.

d-hpdpdjlh = Art
    .doc = Die Art von Ressource, die die Anfrage nennt, wie ihre App sie veröffentlicht (`tube.channel`).

d-34fpixhk = Zustand
    .doc = Wie die Suche steht: `searching`, `one`, `several`, `none` oder `failed`.

d-wz5iufcx = Grund
    .doc = Warum sie fehlgeschlagen ist, in den Worten der App; leer, solange es besser läuft.

d-m3of3jpo = Anordnungsverweis
    .doc = Eine Ressource, die eine Anfrage gefunden hat: ihre Adresse, der Name, unter dem sie läuft, und ihre Art – dieselben drei Angaben, mit denen der Datensatz eines Platzes benannt ist.
    .noun = { $count ->
        [one] Anordnungsverweis
       *[other] Anordnungsverweise
    }

d-c4hrnixt = Schlüssel

d-7bz4aejw = Anfrage
    .doc = Die Anfrage, zu der er gehört, nach ihrem Schlüssel.

d-m6dvgoo5 = Adresse

d-e6vecsii = Titel

d-xhyuztvt = Art

d-4zknrelh = Einstellungen offen
    .doc = Ob die eigenen Einstellungen des gewählten Stils über dem Schreibtisch stehen (`style_settings`), geöffnet aus dem Stilmenü der Leiste: der eine Ort, an dem eine Person den gezeigten Stil ohne Agent ändert, wie es `shell.md` §6, „Zwei Wege, einen Schreibtisch zu ändern“, sagt.

d-dwct4abs = Reihen-Tabs
    .doc = Reihen: ob eine Reihe mit mehreren Plätzen ihre Tableiste zeigt.

d-gfmzi3qr = Rahmen einrasten
    .doc = Rahmen: ob ein auf der Arbeitsfläche losgelassener Platz auf dem Raster des Stils landet, damit Rahmen beim Platzieren in einer Linie stehen.

d-cejnvssb-filter-settings_open = Einstellungen offen

d-cejnvssb-filter-strips_tabs = Reihen-Tabs

d-cejnvssb-filter-frames_snap = Rahmen einrasten

d-nljxfsom = Hierher verschieben
    .doc = Legt eine auf die Arbeitsfläche gezogene Ressource dort ab, wo sie losgelassen wurde.

d-nljxfsom-target = Ziel

d-nljxfsom-at = Bei

d-5giytccr = Erstes öffnen
    .doc = Was Enter bei einer Anfrage tut: Die erste App, die der Text findet, öffnet sich, sodass „no“ und Enter Notizen öffnen; eine Anfrage, die keine App nennt, beantwortet die Suche überall, wie Enter es schon immer für das tat, was die eigenen Zeilen des Schreibtischs nicht enthalten.

d-5giytccr-query = Anfrage

d-u6kdedav = Anfrage zeigen
    .doc = Zeigt `found` im Platz `place`, solange er eine Anfrage zeigt: derselbe Platz, jetzt die gefundene Ressource (shell.md §8).

d-u6kdedav-place = Platz

d-u6kdedav-found = Gefunden

d-4vtnv5px = Anfrage wiederholen
    .doc = Sucht erneut nach dem, was die Suche des Platzes `place` nennt.

d-4vtnv5px-place = Platz

d-mtt6d3af = Hier als Tab
    .doc = Macht den Platz, dessen Fensterleiste auf die Reihe `s` gezogen wurde, zu einem Tab dieser Reihe – so zieht eine Person die Leiste eines Fensters auf die eines anderen, um Tabs zu bilden, und dieselbe Anordnungsaktion, die „In … als Tab verschieben“ im Fenstermenü ausführt.

d-mtt6d3af-v = V

d-mtt6d3af-s = S

d-mtt6d3af-target = Ziel

d-n2ez6yop = Umsortieren
    .doc = Stellt den Platz `place` an die Stelle `at` unter den Tabs seiner Reihe: was das Ziehen einer Pille innerhalb der Fensterleiste tut, als das eigene `move` des Schreibtischs mit dem Platz, vor dem er steht.

d-n2ez6yop-s = S

d-n2ez6yop-place = Platz

d-n2ez6yop-at = Bei

d-d64rg4sw = Eigenes Fenster
    .doc = Gibt dem Platz `place` ein eigenes Fenster: was das Herausziehen eines Tabs aus seiner Fensterleiste tut, als das eigene `split-right` des Schreibtischs für das vordere Fenster.

d-d64rg4sw-s = S

d-d64rg4sw-place = Platz

d-jipcl6u3 = Einstellungen des Schreibtischstils
    .doc = Die eigenen Einstellungen des Schreibtischstils und die zwei Wege, einen Schreibtisch zu ändern (`docs/designs/app-language/ shell.md` §6): ohne Agent, an dem einen Ort, an dem die Einstellungen einer Person liegen, und mit Agent, über dieselben Aktionen, die die Bedienelemente der Person ausführen.

d-wxxxvca4 = Schließen
    .doc = Schließt sie.

d-33wxlq7h = Einstellungen des Schreibtischstils
    .doc = Zeigt oder verbirgt sie: ein Bedienelement für beides, damit eine Person den Zustand dort sieht, wo sie drückt.

d-33wxlq7h-on = An

d-zitymayc = Reihen-Tabs
    .doc = Ob die Reihe mit mehreren Plätzen ihre Tableiste zeigt: die eigene Einstellung des Reihenstils.

d-zitymayc-on = An

d-lfa3rl6s = Plätze am Raster einrasten
    .doc = Ob ein auf der gerahmten Arbeitsfläche losgelassener Platz auf dem Raster landet: die eigene Einstellung des Rahmenstils.

d-lfa3rl6s-on = An

d-7womqcp3 = App-Zeile

d-6t5hb2z7 = Anfrageansicht
    .w-retry = Erneut versuchen
    .w-nothing-matched = Nichts passt zu { $text }

d-pjkvdmwk = Stileinstellungen zeigen~present

d-vseev2rl = Stilbereich
    .w-settings-for = Einstellungen für { $style_title }
    .w-close = Schließen

d-g476xvac = Einstellungen des Desktopstils
    .w-show-the-top-bar = Obere Leiste zeigen
    .w-show-the-dock = Dock zeigen
    .w-dock-edge = Dock-Kante
    .w-dock-size = Dock-Größe
    .w-dock-when-it-shows = Dock, wenn er zeigt

d-e3tczbcu = Einstellungen des Reihenstils
    .w-show-the-tabs-of-a-strip = Tabs einer Reihe zeigen

d-mnbjennf = Einstellungen des Rahmenstils
    .w-snap-places-to-a-grid = Plätze am Raster einrasten

d-xwhxaimf = Einstellungen des Raumstils
    .w-spaces-shows-each-place-as-its = Räume zeigt jeden Platz als eigene Ansicht und hat nichts einzustellen.
