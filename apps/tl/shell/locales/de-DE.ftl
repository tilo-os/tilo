# Shell auf Deutsch. Keys are the declarations' stable ids (app.ids); the source's words are written from the package's sources.
d-so-tilo-shell = Shell

d-nkjtmr33 = Sicht
    .doc = Was die Person von ihrer Anordnung zeigt und welche Leiste vorn steht: die eigene Sicht der Shell darauf, in ihrem eigenen Datensatz (`root viewing: Viewing`).
    .noun = { $count ->
        [one] Sicht
       *[other] Sichten
    }

d-kna5sco4 = Stil
    .doc = Der Stil, den die Person zeigt, als Ansichtsreferenz: ein Paket und eine Ansicht (`so.tilo.shell:strips`, `me.styles:timeline`).

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

d-xf2a4hwd = Dock-Vergrößerung
    .doc = Wie viel größer ein Dock-Eintrag unter dem Zeiger ist, wenn vergrößert wird: in seiner Mitte gut anderthalbmal so groß, ein paar Einträge weiter wieder seine eigene Größe.

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

d-cejnvssb-filter-dock_zoom_amount = Dock-Vergrößerung

d-sfhd6wko = Leistenteil
    .doc = Ein Teil der oberen Leiste, in der Reihenfolge der Person.
    .noun = { $count ->
        [one] Leistenteil
       *[other] Leistenteile
    }

d-fjzbw5un = Art
    .doc = Was das Teil ist; die Leiste zeichnet es daraus, sodass ein verschobenes Teil dasselbe Teil ist.

d-pbly7ymy = Gezeigt
    .doc = Ob es auf der Leiste zeigt; ein ausgeschaltetes Teil behält seinen Platz im Datensatz.

d-sfhd6wko-filter-kind = Art

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

d-fnckswva = Ränder
    .doc = Was eine Anzeige von ihren eigenen Rändern sagt: ob der Zeiger an ihrem oberen oder unteren Rand ist und ob ein Fenster dort steht, wo eine Leiste oder ein Dock fahren würde. Das Gerät veröffentlicht für jede Anzeige eine Zeile (`device.displays.displays`), sodass eine Leiste, die sich bis zum Erreichen versteckt, auf der erreichten Anzeige erscheint und auf keiner anderen.
    .noun = { $count ->
        [one] Ränder
       *[other] Ränder
    }

d-eug3b6ab = Leiste erreicht

d-yiqwedz2 = Leiste verdeckt

d-vpvxtlm4 = Dock erreicht

d-divvrgkh = Dock verdeckt

d-fnckswva-filter-bar_reached = Leiste erreicht

d-fnckswva-filter-bar_covered = Leiste verdeckt

d-fnckswva-filter-dock_reached = Dock erreicht

d-fnckswva-filter-dock_covered = Dock verdeckt

d-rfgelicn = Zeigen
    .doc = Wann eine Leiste oder ein Dock zeigt.

d-p4owlmpg = Immer

d-sscc6gs3 = Solange kein Fenster sie deckt

d-v2eyzsgr = An der Bildschirmkante

d-62fazfo4 = Nach einer Weile

d-v3z2zasp = Größe
    .doc = Wie groß die Teile sind.

d-2yxm7xfn = Klein

d-uv33v3c2 = Normal

d-5czgy2f6 = Groß

d-vtiohdqj = Leistentyp
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
    .doc = Das Layout der Person: was offen ist und wo.
    .noun = { $count ->
        [one] Layout
       *[other] Layouts
    }

d-oomdh5mi-filter-waiting = Wartet

d-q7rxwd72 = Platz der Leiste
    .doc = Wie viel Platz die obere Leiste hat: für das Menü des fokussierten Fensters und für ihre Teile.

d-w3dorpbd = Am meisten: das Menü nach dem Namen seines Fensters, auch die Aufgaben und wer hier ist
    .doc = Am meisten: das Menü nach dem Namen seines Fensters, auch die Aufgaben und wer hier ist.

d-qqg4smi5 = Das Menü nach dem Namen seines Fensters, aber nicht die Aufgaben und wer hier ist
    .doc = Das Menü nach dem Namen seines Fensters, aber nicht die Aufgaben und wer hier ist.

d-7caakb5b = Am wenigsten: das Menü steht als Symbol des Fensters, und nur, was ein Telefon behält, bleibt
    .doc = Am wenigsten: das Menü steht als Symbol des Fensters, und nur, was ein Telefon behält, bleibt.

d-7j7tzynq = Zeigen als
    .doc = Zeigt die Shell durch den Stil `style`: ein Paket und eine Ansicht (`so.tilo.shell:strips`).
    .w-it-is-shown-that-way-already = Er wird schon so gezeigt

d-7j7tzynq-style = Stil

d-zoqbhs7o = Einrichten
    .doc = Womit eine neue Shell beginnt: die Teile der Leiste und des Docks, in der Reihenfolge, in der die voreingestellte Shell sie zeigt.

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

d-fcxzqnum-on = Gezeigt

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

d-23faedwo-on = Gezeigt

d-exyxrzml = Laufende Apps
    .doc = Ob das Dock die laufenden Apps zeigt, die niemand angeheftet hat.

d-exyxrzml-on = Im Dock

d-vlnaqn4u = Launcher-Eintrag
    .doc = Ob das Dock einen Launcher-Eintrag hat.

d-vlnaqn4u-on = Im Dock

d-z2w4wlev = Das Dock vergrößern
    .doc = Ob das Dock den Eintrag unter dem Zeiger vergrößert.

d-z2w4wlev-on = Unter dem Zeiger

d-pltwqaur = Wie sehr das Dock vergrößert
    .doc = Wie viel größer der Eintrag unter dem Zeiger steht: mehr ergibt ein kühneres Dock, und nichts lässt jeden Eintrag seine Größe behalten.

d-pltwqaur-amount = Vergrößerung

d-dvmvtbbz = Seitliches Dock

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
    .doc = Was ein Druck auf einen Dock-Eintrag tut, nach der App, für die der Eintrag steht: ein Fenster dieser App auf diesem Bildschirm kommt nach vorn, sodass die Shell sagt, in welchem Fenster die Person ist, und die Befehle der oberen Leiste seine eigenen sind; das Fenster ist das, für das der Eintrag gezeichnet wurde, sodass ein Druck nie eine zweite Kopie öffnet.

d-aitkuflo-key = App

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

d-b3rfswle-place = Platz

d-72tgr4px = Uhr

d-3ezewqgy = Leiste
    .w-top-bar = Obere Leiste

d-pvl6wwx6 = Leistenteil
    .w-tilo = Tilo
    .w-overview = Übersicht
    .w-undo-layout-change = Layoutänderung zurücknehmen
    .w-redo-layout-change = Layoutänderung wiederholen
    .w-settings = Einstellungen
    .w-about-this-computer = Über diesen Computer
    .w-open-your-card = { $ask }: deine Karte öffnen
    .w-open-your-card-2 = { $person }: deine Karte öffnen
    .w-you-open-your-card = Du: deine Karte öffnen
    .w-invite-someone = Jemanden einladen

d-vidcax4k = Arbeitsbereichsmenü
    .w-workspaces = Arbeitsbereiche
    .w-workspace-name = Name des Arbeitsbereichs
    .w-new-workspace = Neuer Arbeitsbereich
    .w-join-a-workspace = Einem Arbeitsbereich beitreten
    .w-invite-someone = Jemanden einladen
    .w-rename = { $name } umbenennen
    .w-delete = { $name } löschen
    .w-workspace-settings = Einstellungen des Arbeitsbereichs
    .w-overview = Übersicht
    .w-history = Verlauf

d-hac3p23j = Lautstärke
    .w-volume-muted = Lautstärke, stumm
    .w-volume = Lautstärke, { $percent } %

d-iwv7cd5i = Netzwerk
    .w-wi-fi = WLAN, { $called }
    .w-wi-fi-2 = WLAN
    .w-wired = Kabel, { $called }
    .w-wired-2 = Kabel
    .w-offline = Offline

d-4xei7g27 = Ordnerzeile
    .w-your-workspace = Dein Arbeitsbereich
    .w-the-server = den Server
    .w-is-paused = { $name } ist pausiert
    .w-hasn-t-reached-this-computer-yet = { $name } ist noch nicht auf diesem Computer
    .w-is-waiting-for = { $name } wartet auf { $server }
    .w-isn-t-syncing = { $name } wird nicht synchronisiert: { $stuck_reason }
    .w-sending-changes-in-to = Änderungen in { $name } werden an { $server } gesendet
    .w-some-files-in-aren-t-on = Einige Dateien in { $name } haben { $server } noch nicht erreicht
    .w-is-in-sync-with = Alle Dateien in { $name } haben { $server } erreicht

d-dywbbsoi = Grund des Wartens
    .w-hasn-t-said-which-files-it = { $server } hat noch nicht gesagt, welche Dateien es hat
    .w-hasn-t-answered-for-some-files = { $server } hat für einige Dateien noch nicht geantwortet
    .w-part-of-the-file-list-hasn = ein Teil der Dateiliste ist von { $server } noch nicht angekommen
    .w-some-changes-wait-for-others-to = einige Änderungen warten, bis andere { $server } erreicht haben
    .w-has-no-room-for-more-changes = { $server } hat noch keinen Platz für weitere Änderungen; sie bleiben hier, bis Platz ist
    .w-the-workspace-doesn-t-let-you = der Arbeitsbereich erlaubt dir einige dieser Änderungen nicht; frag eine der Personen, denen er gehört
    .w-isn-t-answering-changes-are-sent = { $server } antwortet nicht; die Änderungen werden gleich erneut gesendet
    .w-didn-t-take-some-changes-they = { $server } hat einige Änderungen nicht angenommen; sie werden gleich erneut gesendet
    .w-some-files-details-are-on-neither = zu einigen Dateien fehlen die Angaben, auf diesem Computer und auf { $server }
    .w-some-changes-haven-t-reached-yet = einige Änderungen haben { $server } noch nicht erreicht und werden erneut gesendet

d-jfdcwsgz = Kennzeichen-Text
    .w-yes = ja
    .w-no = nein

d-vipg3gla = Synchronisierung

d-agni63m4 = Akku
    .w-charging = { $percentage } %, wird geladen

d-moh3hzne = Glocke
    .w-notifications-silenced = Benachrichtigungen stumm
    .w-notifications = Benachrichtigungen
    .w-1-unread = { $name }, 1 ungelesen
    .w-unread = { $name }, { $waiting } ungelesen
    .w-1-question-waiting = { $name }, 1 Frage wartet
    .w-questions-waiting = { $name }, { $asked } Fragen warten

d-fniigjed = Einstellungen der Sicht
    .w-shell = Shell
    .w-switching-a-style-keeps-every-window = Ein Stilwechsel lässt jedes Fenster, wo es ist.

d-gapwwnqc = Stil zeigen~present

d-s7yqm7cb = Fläche
    .w-click-anywhere-and-say-what-you = Klicke irgendwo hin und sag, was du willst
    .w-caret = Schreibmarke
    .w-ask-or-find-anything = Frag oder finde etwas
    .w-send = Senden
    .w-type-what-you-want-first = Schreib zuerst, was du willst

d-kzhftbrb = Platz auf der Fläche

d-t3cux3yd = Gerahmter Platz

d-s6b5stgz = Rahmenloser Platz

d-ewgdlyyr = App-Menüs zeigen~present

d-npxnqobp = Zurücksetzen~present

d-vpc43pes = Leiste zeigen~present

d-nn5zjt6d = Dock zeigen~present

d-vedyud4d = Teil verschieben~present

d-gkca5uj2 = Eintrag verschieben~present

d-i4odzov3 = Anheften~present

d-u2tma44r = Gerätebereich
    .w-devices = Geräte
    .w-nearby-send = In der Nähe senden
    .w-offers-this-keep-it-in = { $peer } bietet das an; behalte es in { $name }
    .w-offers-this = { $peer } bietet das an
    .w-keep-it = Behalten
    .w-no-folder-here-takes-the-file = Kein Ordner hier nimmt die Datei
    .w-turn-down = Ablehnen
    .w-stop = Stopp
    .w-it-did-not-finish = Es ist nicht fertig geworden
    .w-try-again = Erneut versuchen
    .w-kept-here = Hier behalten
    .w-sent = Gesendet
    .w-turned-down = Abgelehnt
    .w-no-devices-in-reach = Keine Geräte in Reichweite
    .w-play-here = { $media } hier abspielen?
    .w-asks = { $source } fragt
    .w-allow = Erlauben
    .w-decline = Ablehnen
    .w-dismiss = Verwerfen
    .w-sounds-you-can-send = Klänge, die du senden kannst
    .w-one-app-s-sound = Der Klang einer App
    .w-everything-this-device-plays = Alles, was dieses Gerät abspielt
    .w-drive-with-this-pointer = { $label } mit diesem Zeiger bedienen
    .w-use-s-connection = Die Verbindung von { $label } nutzen


d-2abiywd2 = Kachel-Editor
    .w-customise-the-panel = Bedienfeld anpassen
    .w-panel-controls = Kacheln des Bedienfelds
    .w-move-to-the-front = { $control_label } nach vorn holen
    .w-hide = { $control_label } ausblenden
    .w-not-on-the-panel = Nicht auf dem Panel
    .w-show = { $control_label } einblenden
    .w-reset-the-panel = Bedienfeld zurücksetzen

d-zzd3yipu = Leisten

d-tj4z2obq = Rahmen
    .w-canvas = Fläche

d-tqbdxbtn = Rahmenlos
    .w-frameless = Rahmenlos

d-pbhumuny = Desktop

d-gb3g4lte = Dockuntergrund
    .w-dock = Dock

d-afjfzjl6 = Leiste an der Kante
    .w-top-bar = Obere Leiste

d-su7ogm3c = Obere Leiste

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
    .w-open-as-a-tab = Als Tab öffnen
    .w-apps = Apps
    .w-all-apps = Alle Apps
    .w-welcome = Willkommen
    .w-the-shell = Die Shell
    .w-overview = Übersicht
    .w-workspaces = Arbeitsbereiche
    .w-notifications = Benachrichtigungen
    .w-volume = Lautstärke
    .w-brightness = Helligkeit
    .w-open = Öffnen

d-outvdzmi = Layouts
    .w-layouts = Layouts

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
    .w-edge = Kante
    .w-fit = Sitz
    .w-material = Ausführung
    .w-when-it-shows = Wann sie zeigt
    .w-size = Größe
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
    .w-magnification = Vergrößerung

d-clgbrhsn = Hinweise

d-gjg6noug = Hinweisliste
    .w-notifications = Benachrichtigungen
    .w-clear-all = Alle löschen
    .w-nothing-to-catch-up-on = Nichts aufzuholen
    .w-search-notifications = Benachrichtigungen durchsuchen
    .w-nothing-matches-that = Nichts passt dazu
    .w-clear-the-search-to-see-every = Lösche die Suche, um jeden Hinweis zu sehen

d-y4qdfmpi = Hinweis ganz lesen
    .w-notifications = Benachrichtigungen
    .w-back-to-notifications = Zurück zu den Benachrichtigungen

d-rhowmcuw = Hinweistext
    .w-notification-preview-hidden = Benachrichtigungsvorschau verborgen

d-ytz24ncc = Rückgängig-Leiste
    .w-notifications-cleared = Benachrichtigungen gelöscht
    .w-undo = Rückgängig

d-wud3t5ro = Hinweisgruppe

d-t3ywtwqj = Hinweiszeile
    .w-hand-to-agent = An Agent übergeben
    .w-dismiss = Verwerfen

d-yyk2izra = Banner
    .w-1-more-notification = Noch 1 Benachrichtigung
    .w-more-notifications = Noch { $more } Benachrichtigungen

d-gke2gvrj = Banner ganz
    .w-open = Öffnen

d-wh7n4fhq = Bannerzeile
    .w-answer = Antworten
    .w-decline = Ablehnen
    .w-snooze = Später
    .w-read = Lesen
    .w-dismiss = Verwerfen

d-76zs5p4r = Antwortzeile
    .w-reply = Antworten
    .w-send = Senden
    .w-write-a-reply-first = Schreib zuerst eine Antwort

d-ny7ms2ry = Bildschirmanzeige

d-mz52zjsz = Tablett-Tastenbelegung

d-mpcwgqte = Tablett mit seinen Bedienelementen

d-twljqx7j = Bedienelemente einer Seite

d-7gaoxgif = Bedienelemente einer Kante

d-rcjzvfxp = Übersicht
    .w-overview = Übersicht
    .w-new-workspace = Neuer Arbeitsbereich
    .w-join-a-workspace = Einem Arbeitsbereich beitreten
    .w-search-windows = Fenster durchsuchen
    .w-no-windows = Keine Fenster
    .w-open-something-to-see-its-window = Öffne etwas, um sein Fenster zu sehen

d-766s7y4k = Arbeitsbereiche hier
    .w-here = { $len } hier
    .w-your-tilo = Dein Tilo

d-7nxhynyj = Bild

d-nxpbvfox = Fensterplatz

d-6l3wldtm = Fenstermenü

d-6nn35yb7 = Gefunden
    .w-nothing-matches = Nichts passt
    .w-windows = Fenster

d-kxdzjhoo = Fensterzeile

d-luuijvwf = Tastenhinweise Launcher
    .w-up = Hoch
    .w-down = Runter
    .w-choose = Wählen
    .w-tab = Tab
    .w-next-group = Nächste Gruppe
    .w-enter = Eingabe
    .w-open = Öffnen
    .w-shift = Umschalt
    .w-open-as-a-tab = Als Tab öffnen
    .w-ctrl = Strg
    .w-open-beside = Daneben öffnen
    .w-alt = Alt
    .w-more-actions = Weitere Aktionen
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
    .w-new = { $name }, { $unseen } neu
    .w-show-who-is-here = Zeigen, wer hier ist
    .w-stop = Stopp

d-teccuwja = Schnelleinstellungen

d-dd3gpi64 = Steuerungskachel

d-vzdj2kff = Dreh-Kachel
    .w-rotation-lock = Drehsperre

d-yxl6yq2r = WLAN-Kachel
    .w-wi-fi = WLAN
    .w-this-device-has-no-wi-fi = Dieses Gerät hat kein WLAN
    .w-a-switch-on-this-device-holds = Ein Schalter auf diesem Gerät hält WLAN aus

d-d5wzqn37 = Bluetooth-Kachel
    .w-bluetooth = Bluetooth
    .w-this-device-has-no-bluetooth = Dieses Gerät hat kein Bluetooth

d-hnvxrx3k = VPN ein- oder ausschalten
    .doc = Schaltet das VPN über sein erstes Profil ein oder über das verbundene aus: der eine Druck auf die Kachel, in welche Richtung auch immer.

d-hnvxrx3k-v = V

d-6va3umd4 = VPN-Kachel
    .w-vpn = VPN

d-uzi5ah2q = Dunkel-Kachel
    .w-dark-mode = Dunkler Modus

d-s66oqjwe = Ruhe-Kachel
    .w-do-not-disturb = Nicht stören

d-fabhsxf6 = Nachtlicht-Kachel
    .w-night-light = Nachtlicht

d-vr3zx6ry = Flugmodus-Kachel
    .w-airplane-mode = Flugmodus

d-72cehcnn = Mobilfunk-Kachel
    .w-mobile-data = Mobile Daten

d-ntsyqtrw = Energiemodus-Kachel
    .w-power-saver = Energiesparen
    .w-balanced = Ausgeglichen
    .w-performance = Leistung

d-w3fe7tjv = Pegel
    .w-unmute = Stummschaltung aufheben
    .w-mute = Stummschalten
    .w-volume = Lautstärke

d-wvxjuiww = Tonbereich
    .w-sound = Ton
    .w-unmute-the-microphone = Mikrofon einschalten
    .w-mute-the-microphone = Mikrofon stummschalten
    .w-microphone = Mikrofon
    .w-playing-here = Spielt hier
    .w-playing = Spielt
    .w-paused = Pausiert
    .w-unmute = Ton für { $application } einschalten
    .w-mute = Ton für { $application } ausschalten
    .w-play-through = { $application } zu Ende spielen
    .w-recording-here = Nimmt hier auf

d-thduakjg = Netzwerkbereich
    .w-wi-fi = WLAN
    .w-wired-network = Kabelnetzwerk
    .w-connected = Verbunden
    .w-cable-unplugged = Kabel nicht angeschlossen
    .w-wi-fi-is-off = WLAN ist aus
    .w-look-again = Erneut suchen
    .w-scanning-for-networks = Netzwerke werden gesucht …
    .w-known = Bekannt
    .w-secured = Gesichert
    .w-open = Offen
    .w-enter-the-password-for = Gib das Passwort für { $name } ein
    .w-password = Passwort
    .w-join = Verbinden
    .w-this-device-is-joining-a-network = Dieser Rechner verbindet sich gerade mit einem Netzwerk
    .w-cancel = Abbrechen
    .w-joining = Verbinde mit { $name } …
    .w-hidden-network = Verborgenes Netzwerk
    .w-join-a-network-that-does-not = Einem Netzwerk beitreten, das seinen Namen nicht sendet
    .w-network-name = Netzwerkname
    .w-saved-networks = Gemerkte Netzwerke
    .w-joins-by-itself = Tritt von selbst bei
    .w-saved = Gemerkt
    .w-this-network = Dieses Netzwerk
    .w-stop-joining-by-itself = Nicht mehr von selbst beitreten
    .w-join-by-itself = Von selbst beitreten
    .w-forget-this-network = Dieses Netzwerk vergessen
    .w-a-switch-on-this-device-holds = Ein Schalter auf diesem Gerät hält WLAN aus


d-lcjohgdk = Akkulinie
    .w-battery-settings = Akku-Einstellungen

d-ycck5awo = Ein/Aus-Zeile
    .w-lock = Sperren
    .w-settings = Einstellungen
    .w-power = Ein/Aus

d-wdjxaekq = Arbeitsbereiche
    .w-workspaces = Arbeitsbereiche
    .w-name-a-new-workspace = Benenne einen neuen Arbeitsbereich
    .w-create = Erstellen
    .w-join-a-workspace = Einem Arbeitsbereich beitreten

d-7nsn4srj = Arbeitsbereichskarte
    .w-here = { $len } hier
    .w-open = Öffnen

d-xzb2gybw = Stilumschalter

d-kssxjs7v = Stilmenü
    .w-showing = Zeigt { $style_title }
    .w-settings-for = Einstellungen für { $style_title }

d-qrahog47 = Leistenansicht
    .w-no-window-here-yet = Noch kein Fenster hier

d-4qeo26jr = Zeilen der Arbeitsbereiche

d-7sxqbtn2 = Zeilenfenster

d-2fofsa7f = Leistenfenster

d-4q76vbc7 = Stehendes Fenster
    .w-nothing-here = Nichts hier

d-p5wzoqr5 = Fensterbedienelemente
    .w-restore = Wiederherstellen
    .w-maximize = Maximieren
    .w-close = Schließen

d-bmd7xmln = Ein Platz für eine Suche
    .doc = Fügt `strip` einen Platz hinzu, der eine Suche nach `text` zeigt, mit den beiden Treffern, die die Notizen-App geantwortet hat, und gibt den Schlüssel des Platzes zurück: der Zustand, in dem ein Platz steht, bis die Person eine der gefundenen Ressourcen wählt.

d-44gfvqa5 = Ein Platz für eine Suche unter einem Schlüssel
    .doc = Fügt `strip` unter dem Schlüssel `key` einen Platz hinzu, der eine Suche nach `text` zeigt, mit den beiden Treffern, die die Notizen-App geantwortet hat.

d-44gfvqa5-strip = Spalte

d-44gfvqa5-key = Schlüssel

d-44gfvqa5-text = Text

d-bmd7xmln-strip = Spalte

d-bmd7xmln-text = Text

d-gncm64rz = Stileinstellungen als Blatt
    .w-settings-for = Einstellungen für { $style_title }
    .w-close = Schließen

d-mmqndkio = Einstellungen eines Stils
    .w-comes-from-and-its-settings-are = { $style_label } kommt aus { $package }, und seine Einstellungen gehören seinem Paket.

d-5o2gn6hf = Einstellungen eines Stils
    .doc = Eine Shell mit einer Notiz im genannten Stil, dieser Platz vorn, und die eigenen Einstellungen des Stils darüber geöffnet: was eine Person sieht, die sie aus dem Stilmenü der Leiste öffnet.

d-5o2gn6hf-style = Stil

d-z4vc77e6 = Bedienelemente des Platzes
    .w-this-window = Dieses Fenster
    .w-pin-place = Platz anheften
    .w-close = Schließen

d-oyjbbzbk = Inhalt des Platzes

d-43am22do = Ansicht des Platzes

d-x34hg2ut = Gezeigter Platz

d-5jwos7mc = Zeiger darüber
    .w-is-in = { $who } ist in { $title }

d-fziuz57n = Wechsler
    .w-of = { $at } von { $count }
    .w-switch-windows-of-this-app = Fenster dieser App wechseln
    .w-switch-windows = Fenster wechseln
    .w-this-app-s-windows-in-this = Fenster dieser App in diesem Arbeitsbereich, zuletzt verwendete zuerst. Halte Alt gedrückt und drücke ` zum Bewegen, lass Alt los zum Wechseln oder drücke Escape, um zu bleiben.
    .w-windows-in-this-workspace-most-recently = Fenster in diesem Arbeitsbereich, zuletzt verwendete zuerst. Halte Alt gedrückt und drücke Tab oder die Pfeiltasten zum Bewegen, lass Alt los zum Wechseln oder drücke Escape, um zu bleiben.
    .w-no-windows = Keine Fenster
    .w-open-something-to-switch-to-it = Öffne etwas, um dazu zu wechseln

d-dswgb57x = Fensterkarte

d-jzxijptr = Fensternamen

d-3bxeb2i2 = Fensterkopf

d-sk6mvhah = Fensterbild

d-4hp326dv = Traditioneller Desktop
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

d-7xqpekfu = Einstellungen offen
    .doc = Ob die eigenen Einstellungen des gewählten Stils über der Shell stehen (`style_settings`), geöffnet aus dem Stilmenü der Leiste: der eine Ort, an dem eine Person den gezeigten Stil ohne Agent ändert, wie es `shell.md` §6 sagt.

d-ayatksvb = Reihen-Tabs
    .doc = Reihen: ob eine Reihe mit mehreren Plätzen ihre Tableiste zeigt.

d-fyquhn4g = Rahmen einrasten
    .doc = Rahmen: ob ein auf der Arbeitsfläche losgelassener Platz auf dem Raster des Stils landet, damit Rahmen beim Platzieren in einer Linie stehen.

d-cejnvssb-filter-settings_open = Einstellungen offen

d-cejnvssb-filter-strips_tabs = Reihen-Tabs

d-cejnvssb-filter-frames_snap = Rahmen einrasten

d-cejnvssb-filter-canvas_asked = Auf der Fläche gefragt

d-2e7aappl = Größe ändern
    .doc = Gibt dem Platz `key` die Größe, auf die das Ziehen an seinem Rand oder seiner Ecke ihn gebracht hat, `size` seine Breite und Höhe in Pixeln der Fläche: ein Schritt des Anordnens der Person, seine Ecke bleibt, wo sie stand.

d-2e7aappl-key = Schlüssel

d-2e7aappl-size = Größe

d-nljxfsom = Hierher verschieben
    .doc = Legt eine auf die Arbeitsfläche gezogene Ressource dort ab, wo sie losgelassen wurde.

d-nljxfsom-target = Ziel

d-nljxfsom-at = Bei

d-u6kdedav = Anfrage zeigen
    .doc = Zeigt `found` im Platz `place`, solange er eine Anfrage zeigt: derselbe Platz, jetzt die gefundene Ressource (shell.md §8).

d-u6kdedav-place = Platz

d-u6kdedav-found = Gefunden

d-4vtnv5px = Anfrage wiederholen
    .doc = Sucht erneut nach dem, was die Suche des Platzes `place` nennt.

d-4vtnv5px-place = Platz

d-mtt6d3af = Hier als Tab
    .doc = Macht den Platz, dessen Fensterleiste auf die Reihe `s` gezogen wurde, zu einem Tab dieser Reihe. So zieht eine Person die Leiste eines Fensters auf die eines anderen, um Tabs zu bilden, und „In … als Tab verschieben“ im Fenstermenü führt dieselbe Anordnungsaktion aus.

d-mtt6d3af-s = S

d-mtt6d3af-target = Ziel

d-n2ez6yop = Umsortieren
    .doc = Stellt den Platz `place` an die Stelle `at` unter den Tabs seiner Reihe: was das Ziehen einer Pille innerhalb der Fensterleiste tut, als das eigene `move` der Shell mit dem Platz, vor dem er steht.

d-n2ez6yop-s = S

d-n2ez6yop-place = Platz

d-n2ez6yop-at = Bei

d-d64rg4sw = Eigenes Fenster
    .doc = Gibt dem Platz `place` ein eigenes Fenster: was das Herausziehen eines Tabs aus seiner Fensterleiste tut, als das eigene `split-right` der Shell für das vordere Fenster.

d-d64rg4sw-place = Platz

d-c3l4l473 = Was der Agent getan hat
    .doc = Was das Drücken der Marke des Agenten zeigt, der das Fenster am Platz `place` geöffnet hat: den Verlauf seines Workspace, wo die Schritte des Agenten stehen, wie ihn der eigene Verlauf-Knopf des Fensters zeigt.

d-c3l4l473-place = Platz

d-wln2heq6 = Nach vorn holen
    .doc = Holt das Fenster der Leiste `s` nach vorn, mit der Tastatur darin: die Sitzung holt seinen Platz nach vorn, wie bei einem Druck, einem Skript oder einem Agenten.

d-wln2heq6-s = S

d-dbsggqsm = Fensterbreite ändern
    .doc = Setzt die Breite des Fensters der Leiste `s` auf die, bis zu der sein Rand gezogen wurde: die Breite, die es nimmt, die Breite, die es beim Greifen hatte (beide in Bildschirmpunkten), welcher Rand (`0` führend, `1` folgend) und ob Umschalt gehalten wurde. Eine Anordnungsänderung der Sitzung, aufgenommen und rückgängig zu machen.

d-dbsggqsm-s = S

d-dbsggqsm-at = Bei

d-2wkn5jsq = Hierher verschieben
    .doc = Legt den getragenen Ort in die Lücke: Dort steht er als eigenes Fenster der Zeile, vor dem Fenster `next`, das der Lücke folgt.

d-2wkn5jsq-next = Nächstes Fenster

d-2wkn5jsq-target = Ziel

d-scwczm3e = Lücke

d-3ajs72ce = Einstellungen des Stils
    .doc = Die eigenen Einstellungen des Stils und die zwei Wege, einen Stil zu ändern (`docs/designs/app-language/ shell.md` §6): ohne Agent, an dem einen Ort, an dem die Einstellungen einer Person liegen, und mit Agent, über dieselben Aktionen, die die Bedienelemente der Person ausführen.

d-33qlgb4v = Schließen
    .doc = Schließt sie.

d-nhawvsi6 = Einstellungen des Stils
    .doc = Zeigt oder verbirgt sie: ein Bedienelement für beides, damit eine Person den Zustand dort sieht, wo sie drückt.

d-nhawvsi6-on = An

d-2ycpcfh4 = Reihen-Tabs
    .doc = Ob die Reihe mit mehreren Plätzen ihre Tableiste zeigt: die eigene Einstellung des Reihenstils.

d-2ycpcfh4-on = An

d-wjsjhild = Plätze am Raster einrasten
    .doc = Ob ein auf der gerahmten Arbeitsfläche losgelassener Platz auf dem Raster landet: die eigene Einstellung des Rahmenstils.

d-wjsjhild-on = An

d-qxbh2c44 = Teilname
    .w-tilo = Tilo
    .w-workspace = Arbeitsbereich
    .w-app-menus = App-Menüs
    .w-strip = Leiste
    .w-tasks = Aufgaben
    .w-clock = Uhr
    .w-people = Personen
    .w-status = Status
    .w-battery = Akku
    .w-agents = Agenten
    .w-gap = Lücke

d-y45hdphl = Name des Dock-Eintrags
    .w-launcher = Launcher

d-uaxwjp2w = Verlaufstitel
    .w-recent-in = Zuletzt in { $app_title }
    .w-recent = Zuletzt

d-n53wzmxo = Geöffnet von
    .w-opened-by-you = Von dir geöffnet
    .w-opened-by = Von { $by } geöffnet

d-ykfkkuil = Launcher-Status
    .w-searching = Wird gesucht…
    .w-nothing-matches = Nichts passt zu { $query }

d-f4dklcup = Aussehen
    .w-custom-based-on-tilo = Eigene, basierend auf Tilo
    .w-custom-based-on-macos = Eigene, basierend auf macOS
    .w-custom-based-on-windows = Eigene, basierend auf Windows
    .w-custom-based-on-gnome = Eigene, basierend auf GNOME
    .w-custom-based-on-the-traditional-desktop = Eigene, basierend auf dem traditionellen Desktop
    .w-custom-based-on-minimal = Eigene, basierend auf Minimal
    .w-tilo = Tilo
    .w-like-macos = Wie macOS
    .w-like-windows = Wie Windows
    .w-like-gnome = Wie GNOME
    .w-minimal = Minimal

d-3rrz5qxq = Beschreibung
    .w-a-dock-at-the-bottom-and = Ein Dock unten und die obere Leiste, immer sichtbar.
    .w-a-floating-rounded-dock-that-hides = Ein schwebendes, abgerundetes Dock, das sich versteckt, wenn ein Fenster es bedeckt; die Leiste trägt die Menüs der App.
    .w-a-full-width-taskbar-at-the = Eine Taskleiste über die ganze Breite unten, immer sichtbar.
    .w-the-top-bar-alone-the-overview = Nur die obere Leiste; die Übersicht auf Abruf.
    .w-windows-floating-over-the-screen-each = Fenster schweben über dem Bildschirm, jedes mit eigener Leiste, die Befehle des fokussierten Fensters in der oberen Leiste und ein Dock am Fuß.
    .w-no-chrome-at-rest-the-screen = Keine Bedienelemente in Ruhe: die Bildschirmkanten zeigen die Leiste und das Dock.

d-lpwr6szi = Ungelesen-Zeile
    .w-all-read = Alles gelesen
    .w-1-unread = 1 ungelesen
    .w-unread = { $waiting } ungelesen

d-k5yod4ck = Vor gesagt
    .w-just-now = gerade eben
    .w-min-ago = vor { $value } Min.
    .w-h-ago = vor { $value } Std.
    .w-d-ago = vor { $value } T.

d-76ufqa7r = Bildschirmanzeige-Name
    .w-volume = Lautstärke
    .w-brightness = Helligkeit
    .w-keyboard = Tastatur

d-glfjge4b = Bildschirmanzeige-Wert
    .w-muted = Stumm
    .w-off = Aus

d-ow4oy2u7 = Bildschirmanzeige-Wörter
    .w-volume = Lautstärke
    .w-brightness = Helligkeit
    .w-keyboard-backlight = Tastaturbeleuchtung
    .w-microphone-on = Mikrofon an
    .w-microphone-muted = Mikrofon stumm
    .w-airplane-mode-on = Flugmodus an
    .w-airplane-mode-off = Flugmodus aus
    .w-touchpad-on = Touchpad an
    .w-touchpad-off = Touchpad aus
    .w-playing = Spielt { $named }
    .w-play = Abspielen
    .w-paused = Pausiert { $named }
    .w-pause = Pause
    .w-next-track = Nächster Titel
    .w-previous-track = Vorheriger Titel
    .w-stop = Stopp
    .w-no-adjustable-display = Kein verstellbarer Bildschirm
    .w-night-light-on = Nachtlicht an
    .w-night-light-off = Nachtlicht aus
    .w-do-not-disturb-on = Nicht stören an
    .w-do-not-disturb-off = Nicht stören aus
    .w-magnifier-on = Lupe an
    .w-magnifier-off = Lupe aus
    .w-input-source = Eingabequelle
    .w-keys-captured = Tasten eingefangen
    .w-pointer-released = Zeiger freigegeben
    .w-changed = Geändert

d-6t5hb2z7 = Anfrageansicht
    .w-retry = Erneut versuchen
    .w-nothing-matched = Nichts passt zu { $text }

d-2nputj5f = Stileinstellungen zeigen~present

d-htrvh33d = Stilbereich
    .w-settings-for = Einstellungen für { $style_title }
    .w-close = Schließen

d-tcyusdmx = Einstellungen des Desktopstils
    .w-show-the-top-bar = Obere Leiste zeigen
    .w-bar-size = Größe der Leiste
    .w-show-the-dock = Dock zeigen
    .w-dock-edge = Dock-Kante
    .w-dock-size = Dock-Größe
    .w-magnify-the-item-under-the-pointer = Den Eintrag unter dem Zeiger vergrößern
    .w-magnification = Vergrößerung
    .w-when-the-dock-shows = Wann das Dock zeigt

d-naeq6y2p = Einstellungen des Reihenstils
    .w-show-the-tabs-of-a-strip = Tabs einer Reihe zeigen

d-xa2ijkev = Einstellungen des Rahmenstils
    .w-snap-places-to-a-grid = Plätze am Raster einrasten

d-uatqbuz2 = Einstellungen des rahmenlosen Stils
    .w-frameless-shows-each-place-as-its = Rahmenlos zeigt jeden Platz als eigene Ansicht und hat nichts einzustellen.

d-st426t4p = Ein vorderer Arbeitsbereich
    .doc = Fügt `a` den vorderen Arbeitsbereich mit seiner vorderen Zeile hinzu und gibt den Schlüssel der Zeile zurück.

d-st426t4p-name = Name

d-6bamt22e = Auf der Fläche gefragt
    .doc = Die Fläche: ob die Person auf ihr schon gefragt hat.

d-ukun4zmh = Auf der Fläche gefragt
    .doc = Die Person hat auf der Fläche gefragt: der Hinweis, den die leere Fläche bis dahin sagt, ist gesagt (`Shell.canvas_asked`) und bleibt von nun an fort.

d-dgtjjxex = Menü der Person
    .w-style-and-workspaces = { $me }: Stil und Arbeitsbereiche
    .w-settings-for = Einstellungen für { $style_title }
    .w-new-workspace = Neuer Arbeitsbereich
    .w-workspaces = Arbeitsbereiche
    .w-join-a-workspace = Einem Arbeitsbereich beitreten
    .w-workspace-settings = Einstellungen des Arbeitsbereichs
    .w-overview = Übersicht
    .w-history = Verlauf
    .w-about-tilo = Über Tilo
    .w-tilo = Tilo
    .w-who-is-here = Wer hier ist
    .w-people = Personen
    .w-your-card = Deine Karte
    .w-you = Du

d-zlo3t3an = Aufnahmestatus
    .w-watching = { $label } sieht zu
    .w-watching-and-more = { $label } und { $more } weitere sehen zu
    .w-watching-2 = Sieht zu

d-fcxy4mu5 = Shell-Ansicht
    .w-new-workspace = Neuer Arbeitsbereich
    .w-join-a-workspace = Einem Arbeitsbereich beitreten
    .w-invite-someone = Jemanden einladen
    .w-workspace-settings = Einstellungen des Arbeitsbereichs
    .w-overview = Übersicht

d-54b5rqo4 = Zeilen der Shell
    .w-no-window-here-yet = Noch kein Fenster hier

d-hwz6gayi = Fensterreihe
    .w-no-window-here-yet = Noch kein Fenster hier

d-k4p3z4ee = Vorn
    .doc = Die Leiste, deren Fenster auf diesem Gerät die Tastatur hat, nach ihrem Schlüssel; leer für keine.

d-hro7k2jr = Wartet
    .doc = Ob der Arbeitsbereich der Person noch nicht bekannt ist: Ein Gerät, das keinen hält, hat seinen Daemon gefragt, welcher ihrer ist. Bis er antwortet, hält das Layout keinen Arbeitsbereich, keine Reihe, kein Fenster und keine Person, und die Ansichten zeigen dort den Wartezustand des Toolkits.

d-uykuo63i = Person
    .doc = Die Person an diesem Gerät, mit ihrem Namen.

d-yb7sokzs = Personen
    .doc = Die anderen, die im vorderen Arbeitsbereich hier sind.

d-7n7jrcg3 = Aktionsleiste

d-hqul2vwj = Aktionsleiste

d-7lq6xkzz = Aktionsleiste

d-2ug5q5cq = Plätze
    .doc = Jeder Platz, jeder nennt die Leiste, in der er ist, damit die Ansichten nicht durch den Baum laufen müssen.

d-dt4vnrqa = Anfragen
    .doc = Die Anfragen, die die Arbeitsfläche aufstellt, während eine App sie beantwortet, nach dem Platz, der jede zeigt: was die Person eingegeben hat, was sie gefunden hat oder warum nichts (shell.md §8).

d-fqirvfzw = Verweise
    .doc = Die Ressourcen, die diese Anfragen gefunden haben, jede mit der Anfrage, zu der sie gehört.

d-zsaib3op = Layoutsanfrage
    .doc = Eine Anfrage, die ein Platz zeigt, während ihre App sie beantwortet: was die Person eingegeben hat, in welcher App und was daraus wurde.
    .noun = { $count ->
        [one] Layoutsanfrage
       *[other] Layoutsanfragen
    }

d-6jget5cg = Schlüssel
    .doc = Ihre ID: der Platz, der sie zeigt.

d-iwuvfj66 = Text
    .doc = Was die Person eingegeben hat.

d-ybpst64r = In der App
    .doc = Die gefragte App, nach ihrer kanonischen ID.

d-ibasoiis = Art
    .doc = Die Art von Ressource, die die Anfrage nennt, wie ihre App sie veröffentlicht (`tube.channel`).

d-a6ewjjom = Zustand
    .doc = Wie die Suche steht: `searching`, `one`, `several`, `none` oder `failed`.

d-xsjcdedm = Grund
    .doc = Warum sie fehlgeschlagen ist, in den Worten der App; leer, solange es besser läuft.

d-jq4ftwnw = Layoutsverweis
    .doc = Eine Ressource, die eine Anfrage gefunden hat: ihre Adresse, der Name, unter dem sie läuft, und ihre Art. Der Datensatz eines Platzes ist mit denselben drei Angaben benannt.
    .noun = { $count ->
        [one] Layoutsverweis
       *[other] Layoutsverweise
    }

d-k652vouu = Aktionsleiste

d-vdsinb5l = Anfrage
    .doc = Die Anfrage, zu der er gehört, nach ihrem Schlüssel.

d-vj5mgxi6 = Aktionsleiste

d-pnom4n55 = Aktionsleiste

d-7giiatvv = Aktionsleiste

d-njeklucw = Arbeitsbereich des Layouts
    .doc = Ein Arbeitsbereich, den das Layout der Person zeigt.
    .noun = { $count ->
        [one] Arbeitsbereich des Layouts
       *[other] Arbeitsbereiche des Layouts
    }

d-5zibpqs6 = Aktionsleiste

d-b2vzzczj = Aktionsleiste

d-hwfwvs3f = Aktuell
    .doc = Ob es der Arbeitsbereich vorn ist, den die Shell zeigt.

d-njeklucw-filter-current = Aktionsleiste

d-dpuvzi4j = Person des Layouts
    .doc = Jemand anderes, der im vorderen Arbeitsbereich hier ist: eine Person oder ein Agent.
    .noun = { $count ->
        [one] Person des Layouts
       *[other] Personen des Layouts
    }

d-3rpmhtjf = Aktionsleiste

d-6a7urcxy = Aktionsleiste

d-4s5jxxte = Zeile des Layouts
    .doc = Eine Zeile des Layouts: ein Band über die Breite eines Arbeitsbereichs, das Leisten hält.
    .noun = { $count ->
        [one] Zeile des Layouts
       *[other] Zeilen des Layouts
    }

d-3xc5xbon = Aktionsleiste

d-v5ef7tkv = Aktionsleiste

d-iqgu3ggi = Aktionsleiste

d-sm4scdu2 = Vorne
    .doc = Ob es die vordere Reihe in ihrem Arbeitsbereich ist.

d-bowkspu3 = Shell
    .doc = Die Shell, die ihn angelegt hat; leer liest sich als die dieser Shell.

d-qa4vlh5j = Verschiebung
    .doc = Wie weit die Plätze dieser Zeile seitlich verschoben sind, wie es der Desktop neben dem Baum zeigt: die Zeile wird bei diesem Versatz gezeichnet, und Rad oder Finger der Person bitten den Desktop um einen anderen.

d-4s5jxxte-filter-current = Aktionsleiste

d-4s5jxxte-filter-pan = Verschieben

d-w25t6rl7 = Leiste des Layouts
    .doc = Eine Leiste: eine Spalte aus Plätzen, mit einer Breite, in einer Zeile eines Arbeitsbereichs.
    .noun = { $count ->
        [one] Leiste des Layouts
       *[other] Leisten des Layouts
    }

d-dufcke3u = Aktionsleiste

d-x42tiiqe = Aktionsleiste

d-dvberkpa = Aktionsleiste

d-7ekq6qgi = Breite
    .doc = Ihre Breite in Promille der nutzbaren Breite ihrer Zeile (333 schmal, 667 normal, 1000 breit).

d-2cbikm5m = Aktionsleiste

d-yk2oxjrj = Aktiv
    .doc = Der Platz vorn, nach seiner Kennung; leer für keinen.

d-44lq6lon = Platziert
    .doc = Ob er einen Platz auf der Fläche seines Arbeitsbereichs hat.

d-vbv2zrob = Aktionsleiste

d-jvrwsckz = Aktionsleiste

d-m75kjoue = Aktionsleiste

d-s3lpes2e = Aktionsleiste

d-jlviav7f = Schwebend
    .doc = Ob sein Fenster über den Fenstern des Arbeitsbereichs schwebt, statt in seiner Reihe zu stehen.

d-mgsaujmo = Schwebend x
    .doc = Wo das schwebende Fenster jetzt auf dem Bildschirm steht, den dieser Teil zeichnet, von seiner linken oberen Ecke aus: wo das Layout es hält oder wo die Hand der Person es hat, während sie es trägt.

d-2jtaykbx = Schwebend y

d-3oajnqvp = Schwebende Breite

d-76b3pbs6 = Schwebende Höhe

d-s6rivl3a = Fläche schmiegt sich an
    .doc = Ob `canvas_height` das Höchste ist, was er nimmt: was er zeigt, ist so hoch wie das, was es hält, bis dahin (etwas, das gefunden wurde, wo die Person geklickt hat).

d-pznsxbgq = Aktionsleiste

d-w25t6rl7-filter-width = Aktionsleiste

d-w25t6rl7-filter-maximized = Aktionsleiste

d-w25t6rl7-filter-placed = Aktionsleiste

d-w25t6rl7-filter-canvas_x = Aktionsleiste

d-w25t6rl7-filter-canvas_y = Aktionsleiste

d-w25t6rl7-filter-canvas_width = Aktionsleiste

d-w25t6rl7-filter-canvas_height = Aktionsleiste

d-w25t6rl7-filter-canvas_hugs = Aktionsleiste

d-w25t6rl7-filter-floating = Schwebend

d-w25t6rl7-filter-float_x = Schwebend x

d-w25t6rl7-filter-float_y = Schwebend y

d-w25t6rl7-filter-float_width = Schwebende Breite

d-w25t6rl7-filter-float_height = Schwebende Höhe

d-farfsl4d = Platz des Layouts
    .doc = Ein Platz: ein Fenster, das eine Ressource durch eine Ansicht zeigt.
    .noun = { $count ->
        [one] Platz des Layouts
       *[other] Plätze des Layouts
    }

d-ba3kyokq = Aktionsleiste

d-7dno3n47 = Leiste
    .doc = Die Leiste, in der er ist.

d-jpzk4cli = Zeigt
    .doc = Die App, die die Ressource zeigt, nach ihrer kanonischen Kennung (`so.tilo.terminal`).

d-nprpusuo = Gezeigt als
    .doc = Die Ansicht, durch die sie die Ressource zeigt; leer für die Vorgabe der App.

d-ofkshr4v = Datensatz
    .doc = Der Datensatz, den er zeigt, wenn er einen zeigt.

d-qqcejiim = Titel
    .doc = Der Titel, unter dem er gezeigt wird.

d-jeatudax = Name
    .doc = Wie dieser Platz heißt, auf jeder Oberfläche: der Titel seines Fensters und der eigene Name seiner App (`Groceries` in `Notes`, den die Person gewählt hat), oder der Name der App allein, wenn das Fenster keinen Titel hat.

d-vkbhstef = Angeheftet
    .doc = Ob die Person ihn angeheftet hat: die Layout eines Agenten lässt ihn in Ruhe.

d-soy4vimv = Shell
    .doc = Die Shell, die ihn angelegt hat; leer liest sich als die dieser Shell.

d-farfsl4d-filter-pinned = Aktionsleiste

d-socv3cfz = Layout
    .doc = Ein benannter Satz von Einstellungen, den die Person in einer Aktion anwendet: eine Vorgabe für den Rahmen.

d-cb4a3qsa = Das Dock unten mit der Leiste oben: Tilos eigene
    .doc = Das Dock unten mit der Leiste oben: Tilos eigene.

d-qmzl573l = Ein schwebendes, rundes, durchscheinendes Dock unten, das sich verbirgt, solange ein Fenster es deckt, und eine obere Leiste mit den Menüs der fokussierten App nach denen des Systems
    .doc = Ein schwebendes, rundes, durchscheinendes Dock unten, das sich verbirgt, solange ein Fenster es deckt, und eine obere Leiste mit den Menüs der fokussierten App nach denen des Systems.

d-iriqaem6 = Eine Taskleiste über die volle Breite unten, immer gezeigt
    .doc = Eine Taskleiste über die volle Breite unten, immer gezeigt.

d-n44stfvj = Die obere Leiste allein; die Übersicht auf Abruf
    .doc = Die obere Leiste allein; die Übersicht auf Abruf.

d-dah75qd2 = Kein Rahmen in Ruhe; die Kanten zeigen, was die Person braucht
    .doc = Kein Rahmen in Ruhe; die Kanten zeigen, was die Person braucht.

d-zsqlezfj = Der traditionelle Desktop: Rahmen frei über der Shell, die eigenen Befehle des fokussierten Fensters in der oberen Leiste und ein schwebendes Dock am Fuß, die Form, die Menschen von macOS und GNOME kennen
    .doc = Der traditionelle Desktop: Rahmen frei über der Shell, die eigenen Befehle des fokussierten Fensters in der oberen Leiste und ein schwebendes Dock am Fuß, die Form, die Menschen von macOS und GNOME kennen.

d-ce3pp2or = Gezeichneter Platz
    .doc = Hält fest, wie hoch der Platz `key`, der sich seinem Inhalt anpasst, jetzt gezeichnet ist; `size` trägt seine Höhe in Pixeln der Fläche, wie die Karte sie anlegt: die freie Fläche darunter ist, was die Person dort sieht (canvas.md §1, §7), auch wenn der Platz in sie wachsen könnte.

d-ce3pp2or-key = Schlüssel

d-ce3pp2or-size = Größe

d-vi5fveaq = Modellwahl

# Added from the source: new words.
d-pdda7z3y = Hängendes Fenster

d-n6arlzpq = Fenster anderswo
    .w-on = { $origin_app } auf { $device_name }
    .w-on-another-device = { $origin_app } auf einem anderen Gerät
    .w-this-window-runs-on-that-device = Dieses Fenster läuft auf diesem Gerät. Öffne es dort, um es zu sehen.

d-6rjhuenw = Emoji-Karte

d-wdiwq4yt = Über-Fenster

d-tkt6f7u6 = Über-Fenster
    .w-updates = Aktualisierungen
    .w-close = Schließen
    .w-all-details = Alle Details

d-vv7k7lcl = Super-Tasten

d-fhub73iw = Platzinhalt

d-fbymgkoh = Arbeitsbereich umbenennen
    .doc = Benennt den Arbeitsbereich `key` so, wie die Person getippt hat: über das eigene veröffentlichte `rename` des Arbeitsbereichs, damit der Name jedes Gerät und jede Liste von ihm erreicht.

d-fbymgkoh-key = Schlüssel

d-fbymgkoh-name = Name

d-lqy7sqnc = Leiste umbenennen
    .doc = Benennt den Streifen `key` so, wie die Person getippt hat: eine ordnende Aktion der Sitzung, damit ein Skript und ein Agent so umbenennen wie ein Druck.

d-lqy7sqnc-key = Schlüssel

d-lqy7sqnc-name = Name

d-rs2j4mmu = Launcher-Zeile wählen
    .doc = Was das Wählen der Zeile tut, die die Person erreicht hat, abgelesen am Wert der Zeile: hier eine Anwendung oder ein Fenster öffnen, eine Zeile ausführen, die die Suche fand, oder eine der eigenen Flächen der Shell.

d-rs2j4mmu-choice = Wahl

d-rs2j4mmu-current = Aktuell

d-t34ri7fm = Eine Stufe ändern
    .doc = Ändert den Wert der Zeile, die `step` nennt (`1` hoch, `-1` runter), wie Links und Rechts es auf einer verstellbaren Zeile tun: die Lautstärke oder die Helligkeit vorn, um ein Zwanzigstel.

d-t34ri7fm-step = Stufe

d-6o7nacnz = Launcher-Zeile anders öffnen
    .doc = Was das Wählen einer Zeile mit gehaltener Taste tut: eine Zeile, die die Suche fand, öffnet sich so, wie `here` (ein Tab vorn), `beside` (ein Fenster daneben) oder `more` (der eigene Weg der Zeile) es nennt, und jede andere Zeile öffnet sich, wie sie es schlicht tut.

d-6o7nacnz-packed = Gepackt

d-6o7nacnz-current = Aktuell

d-ar6xlcsz = Launcher-Abfrage
    .doc = Das Feld des Launchers wurde bearbeitet: die Abfrage der offenen Suche ist der Text, den es jetzt hält, sodass die angezeigten Zeilen und die Suchressource, die ein Agent oder ein Skript liest, sagen, was die Person getippt hat, nicht was sie vorher getippt hat.

d-ar6xlcsz-text = Text

d-q6bzrju7 = Oberfläche öffnen
    .doc = Öffnet die Fläche `surface` der Shell (`overview`, `workspaces`, `centre`), wie es die eigenen Bedienelemente der Leiste tun.

d-q6bzrju7-surface = Oberfläche

d-n2ucmbpe = Launcher schließen
    .doc = Verlässt den Starter: der Stapel der Flächen geht zurück zur Shell in Ruhe.

d-rfepr4yx = Leistenteil
    .w-strip-name = Name der Leiste
    .w-new-strip = Neue Leiste
    .w-rename = { $name } umbenennen
    .w-move-left = { $name } nach links
    .w-move-right = { $name } nach rechts
    .w-delete = { $name } löschen

d-nojvjjii = Leistenliste
    .w-strips = Leisten
    .w-new-strip = Neue Leiste
    .w-done = Fertig
    .w-history = Verlauf

d-dpb7af4j = Meldung
    .doc = Ob die Meldungsauswahl über den Fenstern steht: die Person wählt das Fenster, um das es in einer Meldung über ein Problem geht (`feedback.tl`).

d-7bxopjro = Fensterwahl einer Meldung
    .doc = Die Fenster, um die es in der entstehenden Meldung geht, in der Reihenfolge, in der sie gewählt wurden: die eigene Aufzeichnung der Shell, worauf die Person gezeigt hat, damit eine Meldung etwas ist, das sie nachlesen kann.

d-r6satdro = Meldungsbereich
    .doc = Der Umriss des Rechtecks, das die Person über der Auswahl gezogen hat, als `x, y, Breite, Höhe` in Canvas-Pixeln: leer, solange kein Zug steht.

d-cejnvssb-filter-reporting = Meldungen

d-66jw6ezz = Fenster für eine Meldung wählen
    .doc = Ein Problem melden: der Bildschirm dunkelt auf den Grund des Desktops ab, jedes Fenster des vorderen Arbeitsbereichs steht als Karte mit seinem eigenen lebenden Bild, und die Person wählt das Fenster, um das es in der Meldung geht: ein Klick auf seine Karte oder ein Rechteck, das sie darüber zieht.
    .noun = { $count ->
        [one] Fensterwahl einer Meldung
       *[other] Fensterwahlen einer Meldung
    }

d-hb2dxuji = Schlüssel

d-hu4bokd4 = Titel

d-abe7lbj4 = App-Name

d-cswol6ca = Zuletzt geöffnet
    .doc = Was die Person zuletzt im vorderen Arbeitsbereich geöffnet hat, abgelesen am Verlauf des Arbeitsbereichs, was seine Fenster zeigten: jede Ressource einmal, die eigenen zuerst, das Neueste zuerst.

d-rjnlecke = Zuletzt geöffnete Ressource
    .doc = Eine Ressource, die die Person zuletzt geöffnet hat: was sie ist, ihre App und ihr Typ, und wer sie zuletzt geöffnet hat und wann.
    .noun = { $count ->
        [one] zuletzt geöffnete Ressource
       *[other] zuletzt geöffnete Ressourcen
    }

d-zyqmwz53 = Schlüssel
    .doc = Seine Kennung: die Adresse der Ressource.

d-g5lvltn4 = Ressource
    .doc = Die Ressource, über ihre Adresse, wie `session.open` sie nimmt.

d-33ygyyvo = Titel

d-rgnzsrl5 = Herkunfts-App
    .doc = Die App, die sie hält, über ihre kanonische Kennung.

d-qegnrgvk = App-Titel
    .doc = Der Name dieser App.

d-wpv5ccaw = Art
    .doc = Ihr Typ, so wie ihre App ihn veröffentlicht.

d-nk3qn5vg = Von
    .doc = Wer sie zuletzt geöffnet hat, nach Namen.

d-n77p6zrw = Um

d-5xciib7g = Meine
    .doc = Ob die Person, die gerade schaut, sie zuletzt selbst geöffnet hat.

d-rjnlecke-filter-at = Um

d-rjnlecke-filter-mine = Meine

d-2cmvxqpt = Problem melden
    .doc = Beginnt eine Auswahl: worum es in einer Meldung über ein Problem geht, auf dem ganzen Bildschirm gewählt.

d-fipjigt5 = Fenster für die Meldung wählen
    .doc = Das Fenster `key` ist das, um das es in der Meldung geht: es tritt der Meldung bei, die Auswahl schließt sich, und das Feld der Meldung öffnet sich über dem Fenster, die Person frei, auf den falschen Teil davon zu zeigen.

d-fipjigt5-key = Fenster

d-y6cjt65t = Das Fenster wählen, um das es in einer Meldung geht
    .doc = Das Rechteck `area`, das die Person über der Auswahl gezogen hat, mit seinen x-, y-, Breiten- und Höhenkoordinaten in Canvas-Pixeln, wählt das Fenster, das es zuerst bedeckt, wie ein Klick auf diese Karte es tut.

d-y6cjt65t-area = Bereich

d-y6cjt65t-room = Raum

d-zek2dnmp = Abbrechen
    .doc = Verlässt die Auswahl ohne etwas: kein Fenster, keine Meldung.

d-d63pkwee = Zuletzt geöffnet an einem Nachmittag
    .doc = Adas Öffnungen an einem Nachmittag und zwei von Mia, das Neueste zuerst, wie die Sitzung sie auflistet.

d-7ugeomrq = Meldungsauswahl
    .w-what-the-report-is-about = Worum es in der Meldung geht
    .w-nothing-is-open-to-report-about = Nichts ist geöffnet, worüber zu berichten wäre
    .w-click-the-window-that-isn-t = Klicke das Fenster an, das nicht funktioniert, oder ziehe ein Rechteck darüber
    .w-cancel = Abbrechen

d-6mnqsxi6 = Bedienhinweise
    .w-super = Super
    .w-space = Leertaste

d-lcq7ufsf = Verlauf
    .w-clear = Leeren
    .w-nothing-opened-lately = Zuletzt nichts geöffnet
    .w-what-you-open-shows-up-here = Was du öffnest, erscheint hier

d-6eidrgir = Verlaufszeile

d-o5w6ulql = Hinweise über Fenstern

d-txupy4rf = Streifenleiste

d-p6z3qse6 = Streifenleiste

d-ifgvywgk = Reihen-Tabs

d-2dvbjocr = Streifenleiste
    .w-of = { $number } von { $len }
    .w-new-strip = Neuer Streifen

d-ssbg6dg6 = Bildschirm
    .doc = Der Bildschirm, auf dem dieses Gerät den Streifen zeigt, nach seiner Kennung (`device.screen.output`): jeder Teil der Shell zeichnet die Streifen seines eigenen Bildschirms.

d-w25t6rl7-filter-display = Bildschirm

d-nynnosmh = Ein Streifen auf
    .doc = Fügt einen Streifen mit dem Namen `name` bei `width` zu `row` hinzu, gezeigt auf `display`, und gibt seinen Schlüssel zurück.

d-nynnosmh-row = Zeile

d-nynnosmh-name = Name

d-nynnosmh-width = Breite

d-nynnosmh-display = Bildschirm

d-dk2iupmd = Erkennungskarte

d-ndaefmcq = Wartet auf Antwort
    .doc = Was auf das Ja der Person wartet: jeder Aufruf, der es braucht, gelesen aus seinem eigenen Eintrag in den Aufrufen des Arbeitsbereichs.
    .noun = { $count ->
        [one] Wartet auf Antwort
       *[other] Wartet auf Antwort
    }

d-5xjx7oct = Ordner
    .doc = Der gezeigte Ordner, nach seinem Pfad; leer, bis eine Frage nach Dateien fragt.

d-5esxwq2o = Einträge
    .doc = Was der Ordner enthält, wie zuletzt gelesen.

d-xifz6fny = Orte
    .doc = Die Orte, die die Auswahl neben dem Ordner bietet: Wechselmedien, wo ein jetzt angeschlossenes Laufwerk eingehängt ist, damit eine Person einen Ordner darauf wählen kann.

d-tlhktser = Aufruf ändern
    .doc = Eine Änderung, die die Person am Aufruf machte, bevor sie ihn zustimmte: ein Feld, nach seinem Pfad in den Argumenten des Aufrufs, und was sie hineinschrieb.
    .noun = { $count ->
        [one] Aufruf ändern
       *[other] Aufrufe ändern
    }

d-r6c6zjdg = Pfad

d-5fhfjzcm = Text

d-ftcph3ii = Ecke der Karten
    .doc = Die Ecke des benutzten Bildschirms, in der die Karten des Live-Inhalts andocken: die Wahl der Person, auf jedem Gerät, das sie benutzt.

d-cejnvssb-filter-cards_corner = Ecke der Karten

d-jbtiqjt4 = Home
    .doc = Wo der Arbeitsbereich als Seite im Web aufgeht (`/w/<workspace>/desktop`), wenn die Shell dort dient: wohin seine Karte verlinkt.

d-tcsafce7 = Ecke der Karte
    .doc = Die Ecke des Bildschirms, in der die Karten des Live-Inhalts andocken.

d-ufe4dez5 = Oben links

d-y4tcqd6i = Oben rechts

d-kes3aytt = Unten links

d-wt6lph2u = Unten rechts

d-aegu5nc6 = Nicht erlauben
    .doc = Lehnt die Frage bei `key` ab: das eigene `resolve` ihres gefragten Eintrags, aufgerufen über seine Adresse, wie jeder Aufrufer es aufruft.

d-aegu5nc6-key = Schlüssel

d-4pwuckls = Ordner öffnen
    .doc = Zeigt den Ordner `path`, um Dateien darin zu wählen, wie dieses Gerät ihn gerade liest, mit Wechselmedien als einem Ort, den die Auswahl bietet.

d-4pwuckls-path = Pfad

d-lqtiqtq2 = Ordner schließen
    .doc = Hört auf, einen Ordner zu zeigen: die Frage, die nach Dateien fragte, ist beantwortet.

d-jid2lypn = Karten andocken
    .doc = Dockt die Karten des Live-Inhalts in der Ecke `corner` des benutzten Bildschirms an.

d-jid2lypn-corner = Ecke

d-46uwq3gq = Verlauf schließen
    .doc = Legt die Fläche weg, sobald eine kürzliche Ressource wieder neben dem Fenster vorn geöffnet wird, durch dasselbe `open`, das ein Agent und der Druck auf einen Verweis ausführen.

d-mnpvowua = Fenster nach links
    .doc = Bewegt das Fenster `w` vor das Fenster `before` seines Streifens: einen Platz daran entlang.

d-mnpvowua-w = Das Fenster

d-mnpvowua-before = Das Fenster davor

d-c4363mpd = Fenster nach rechts
    .doc = Bewegt das Fenster `w` einen Platz zur hinteren Seite: vor das Fenster nach dem nächsten, oder als letztes in seinem Streifen, solange dort keines steht.

d-c4363mpd-w = Das Fenster

d-x6ly4cml = Gleiche Breiten
    .doc = Gibt jedem Fenster der Reihe denselben Anteil: was „alle Fenster gleich“ tut, wo eine Person ihre Fenster nebeneinander in einem Band sieht.

d-x6ly4cml-w = Das Fenster

d-o4f6z532 = Ordner wählen
    .w-no-folder-open = Kein Ordner offen
    .w-a-question-that-asks-for-files = Eine Frage, die nach Dateien fragt, öffnet hier einen

d-buc46sub = Gefragt vorbei

d-q7w2n4x9 = Gefragt beim Drücken
    .w-hold-shift-to-skip = Zum Überspringen Umschalt gedrückt halten

d-o7szomwg = Gefragtes Banner

d-i7735z6w = Gefragte Banner

d-t8k3vq2m = Gefragt über der Shell

d-x266v635 = Mehr gefragt
    .w-1-more-question = 1 weitere Frage
    .w-more-questions = { $more } weitere Fragen

d-vpg3b467 = Frageliste
    .w-waiting-for-you = Wartet auf dich

d-uahwpm3g = Gefragte Karte

d-xfpagjt5 = Frage

d-d4a7v5zp = Fragetext
    .w-undo-can-t-bring-this-back = Rückgängig bringt das nicht zurück
    .w-this-is-answered-on-the-device = Das wird auf dem Gerät beantwortet, das gefragt hat
    .w-fill-in-what-it-asks-for = Fülle zuerst aus, wonach es fragt
    .w-don-t-answer = Nicht beantworten
    .w-always-let = { $asker_name } immer { $verb } lassen: { $value }
    .w-change = Ändern
    .w-don-t-allow = Nicht erlauben

d-wo44tm5w = Dateien gefragt
    .w-choose = Wählen
    .w-save = speichern
    .w-folder = Ordner
    .w-open = öffnen
    .w-up = Hoch
    .w-there-is-no-folder-above-this = Über diesem liegt kein Ordner zum Hingehen
    .w-recent = Zuletzt
    .w-name = Aktionsleiste
    .w-modified = Geändert
    .w-size = Größe
    .w-hidden-files = Versteckte Dateien
    .w-nothing-here-matches = Hier passt nichts zu „{ $query }“
    .w-no-recent-files = Keine kürzlichen Dateien
    .w-no-folders-here = Keine Ordner hier
    .w-this-folder-is-empty = Dieser Ordner ist leer
    .w-folder-2 = Ordner
    .w-files-chosen = { $len } Dateien gewählt
    .w-cancel = Abbrechen
    .w-save-2 = Speichern
    .w-choose-folder = Ordner wählen
    .w-open-2 = Öffnen
    .w-recent-files-hold-no-folder-to = Kürzliche Dateien halten keinen Ordner zum Speichern oder Wählen
    .w-name-the-file-first = Benenne zuerst die Datei
    .w-choose-a-file-first = Wähle zuerst eine Datei
    .w-replace = „{ $named }“ ersetzen?
    .w-replace-2 = Ersetzen
    .w-already-holds-a-file-with-this = { $picker_name } hält schon eine Datei mit diesem Namen. Sie zu ersetzen schreibt über das, was sie hält.

d-nsvxusve = Fragender

d-yd6yk7ui = Live-Pille
    .w-fold-back = { $live_name } zurückfalten
    .w-live = { $live_name }, live
    .w-show-as-a-card = Als Karte zeigen
    .w-more-live = { $value } weitere live

d-3nryt5p3 = Live-Abschnitt
    .w-live-now = Jetzt live

d-5wjrqfa3 = Live-Zeile
    .w-on = Auf { $device }

d-nqzenak5 = Live-Kartensteuerung
    .w-show-on-the-bar = In der Leiste zeigen
    .w-keep-off-the-bar = Aus der Leiste lassen
    .w-back-to-the-bar = Zurück in die Leiste
    .w-show-as-a-card = Als Karte zeigen
    .w-put-away = Weglegen

d-3dxyousy = Native Zeile

d-nraevli5 = Live-Steuerung

d-elqi6l5x = Live-Karten

d-n2v5vhnx = Live-Karte
    .w-card = Karte { $live_name }
    .w-on = { $source } auf { $device }
    .w-card-place = Platz der Karte
    .w-back-in-the-corner = Zurück in die Ecke
    .w-cards-at-the = Karten { $lower }
    .w-back-to-the-bar = Zurück in die Leiste
    .w-put-away = Weglegen
    .w-in = { $live_name } in { $source }

d-ele2mmbh = Kartentext
    .w-position-in = Platz in { $live_name }

d-tbvykixf = Abgestürzte Karte
    .w-app-ended = App beendet
    .w-reopen = Wieder öffnen
    .w-dismiss = Verwerfen
    .w-details = Details

d-agpwsfhm = Telefon
    .w-sign-in-to-tilo = Bei Tilo anmelden
    .w-on-a-device-that-has-your = Führe auf einem Gerät mit deiner Identität `tilo device add` aus und gib den Code, den es zeigt, hier ein. Ist dies dein erstes Gerät, fange neu an.
    .w-not-signed-in = Nicht angemeldet
    .w-tilo1 = tilo1…
    .w-sign-in = Anmelden
    .w-start-new = Neu anfangen
    .w-workspaces = Aktionsleiste
    .w-your-workspace-didn-t-let-this = Dein Arbeitsbereich hat dieses Telefon nicht hereingelassen
    .w-waiting-for-your-workspace = Warte auf deinen Arbeitsbereich…
    .w-no-workspaces-here-yet = Noch keine Arbeitsbereiche hier
    .w-back = Zurück
    .w-apps = Apps
    .w-apps-can-t-run-on-this = Apps laufen auf diesem Telefon nicht
    .w-no-apps-yet = Noch keine Apps

d-iworlvwt = Schnelleinstellungen
    .w-quick-settings = Schnelleinstellungen
    .w-bluetooth-accessory = Bluetooth-Zubehör
    .w-back-to-quick-settings = Zurück zu den Schnelleinstellungen
    .w-address = Adresse
    .w-state = Zustand
    .w-connected = Verbunden
    .w-paired = Gekoppelt
    .w-signal = Signal
    .w-dbm = { $signal } dBm
    .w-battery = Akku
    .w-name = Name
    .w-what-this-device-calls-it-the = Wie dieser Rechner es nennt; der Name, den das Zubehör meldet, bleibt seiner
    .w-rename = Umbenennen
    .w-call-it-what-the-field-says = So nennen, wie es im Feld steht
    .w-devices = Geräte
    .w-this-device = Dieser Rechner
    .w-connected-here = Hier verbunden
    .w-paired-here = Hier gekoppelt
    .w-in-reach = Erreichbar
    .w-away = Abwesend
    .w-disconnect = Trennen
    .w-connect = Verbinden
    .w-forget-this-device = Dieses Gerät vergessen
    .w-this-device-is-no-longer-paired = Dieses Gerät ist nicht mehr gekoppelt
    .w-bluetooth = Bluetooth
    .w-bluetooth-is-off = Bluetooth ist aus
    .w-disconnect-2 = { $name } trennen
    .w-connect-2 = { $name } verbinden
    .w-stop-looking = Suche beenden
    .w-look-for-devices = Nach Geräten suchen
    .w-no-devices-found = Keine Geräte gefunden

d-ij74wfyv = Zwischenablage-Abschnitt
    .w-clipboard = Zwischenablage
    .w-stop-keeping = { $copy_label } nicht mehr behalten
    .w-keep = { $copy_label } behalten
    .w-remove = { $copy_label } entfernen
    .w-clear-the-rest = Den Rest leeren
    .w-search-copies = Kopien durchsuchen
    .w-1-of-copies = 1 von { $len } Kopien
    .w-of-copies = { $len } von { $len2 } Kopien
    .w-no-copy-matches = Keine Kopie passt

d-mnufvdts = Energie-Blatt
    .w-power = Ein/Aus
    .w-lock = Sperren
    .w-log-out = Abmelden
    .w-switch-user = Benutzer wechseln
    .w-sleep = Schlafen
    .w-restart = Neu starten
    .w-shut-down = Herunterfahren

d-kvfaa7ju = Erlaubnishinweis
    .w-dismiss = Verwerfen

d-47ahldi3 = Startkarten
    .w-start-something = Etwas anfangen
    .w-only-you-see-what-you-make = Nur du siehst, was du hier machst, bis du jemanden einlädst.
    .w-notes = Notizen
    .w-write-ideas-down = Ideen aufschreiben
    .w-tasks = Aufgaben
    .w-write-down-what-needs-doing = Aufschreiben, was zu tun ist
    .w-invite-people = Personen einladen
    .w-work-on-this-with-someone = Daran mit jemandem arbeiten
    .w-catch-up = Aufholen
    .w-see-what-changed-and-why = Sehen, was sich änderte und warum
    .w-arrows = Pfeile
    .w-move = Verschieben
    .w-enter = Eingabe
    .w-open = Öffnen
    .w-super = Super
    .w-n = N
    .w-every-app = Jede App
    .w-open-an-app = Eine App öffnen

d-7pn66ekb = Autoren-Chip

d-hl7f3yqb = Fensterleisten-Teile
    .w-back-in-this-window = Zurück in diesem Fenster
    .w-forward-in-this-window = Vor in diesem Fenster
    .w-sidebar = Seitenleiste
    .w-more-actions-of-this-window = Weitere Aktionen dieses Fensters

d-xqbtyqvn = Leistenschaltfläche

d-kdh5crgd = Leisteneintrag

d-gtvxl4ug = Leiste umbenennen
    .w-window-name = Fenstername

d-j5z7brtk = Leistenort
    .w-where-the-window-goes = Wohin das Fenster geht

d-xfexkr2v = Leistensuche
    .w-search-this-window-and-replace = Dieses Fenster durchsuchen und ersetzen
    .w-search-this-window = Dieses Fenster durchsuchen
    .w-close-the-search = Suche schließen

d-6zyejo5v = Fensterleisten-Aktionen
    .w-new-tab-of-this-window = Neuer Tab dieses Fensters
    .w-search-this-window = Dieses Fenster durchsuchen
    .w-history-of-this-window = Verlauf dieses Fensters
    .w-back-in-this-window = Zurück in diesem Fenster
    .w-forward-in-this-window = Vor in diesem Fenster
    .w-starting = Starte { $title } …

d-vrlwh4vn = Dieses Fenster-Menü
    .w-this-window = Dieses Fenster
    .w-reveal = Zeigen
    .w-it-is-in-front-already = Es ist schon vorn
    .w-exit-full-screen = Vollbild verlassen
    .w-full-screen = Vollbild
    .w-restore = Wiederherstellen
    .w-fill-the-strip = Streifen füllen
    .w-snap-back = Zurückschnappen
    .w-float-this-window = Dieses Fenster schweben lassen
    .w-half-the-strip = Halber Streifen
    .w-narrow = Schmal
    .w-normal = Normal
    .w-wide = Breit
    .w-every-window-the-same = Alle Fenster gleich
    .w-wider = Breiter
    .w-as-wide-as-it-gets = So breit wie möglich
    .w-narrower = Schmaler
    .w-as-narrow-as-it-gets = So schmal wie möglich
    .w-move-left = Nach links
    .w-move-right = Nach rechts
    .w-move-into-as-a-tab = Als Tab nach { $menu_strip_name } verschieben
    .w-move-to = Nach { $name } verschieben
    .w-display = Bildschirm { $number }
    .w-move-to-a-new-strip = In einen neuen Streifen verschieben
    .w-new-tab-of-this-app = Neuer Tab dieser App
    .w-rename = Umbenennen
    .w-split-right = Rechts teilen
    .w-new-group = Neue Gruppe
    .w-unpin-window = Fenster lösen
    .w-pin-window = Fenster anheften
    .w-stop-controlling-the-desktop = Den Schreibtisch nicht mehr steuern
    .w-close-tab = Tab schließen
    .w-close-this-window = Dieses Fenster schließen

d-jdungghl = Darstellungseinstellungen
    .w-look = Aussehen
    .w-dark = Dunkel
    .w-light = Hell
    .w-translucency = Durchscheinend
    .w-accent-follow-the-theme = Akzent: dem Thema folgen
    .w-window-corners = Fensterecken
    .w-refraction = Brechung

d-65ugxki4 = Fenstermenü-Karte

d-vhbxgivv = Fenstermenü-Zeile

d-5myqhmsc = Was eine Leserin über eine Person hört
    .w-now = Jetzt: { $place }
    .w-in-a-call = In einem Anruf: { $call }
    .w-follows-you = Folgt dir
    .w-joined = Beigetreten { $joined }

d-mxfb2qdk = Wo-Zeile
    .w-working-in = Arbeitet in { $title }
    .w-viewing = Sieht { $title }
    .w-in = In { $in_app }

d-vqx3nbw5 = Anwesenheitsstatus
    .w-away = abwesend
    .w-offline = offline
    .w-busy = beschäftigt
    .w-online = online

d-bwowjxfr = Verfügbarkeitswort
    .w-focusing = Fokussiert
    .w-away = Abwesend
    .w-idle = Inaktiv
    .w-available = Verfügbar

d-ldpcj75n = Verfügbarkeit benannt
    .w-focusing = Fokussiert
    .w-away = Abwesend
    .w-idle = Inaktiv
    .w-available = Verfügbar

d-urz5g437 = Energiemodus-Bezeichnung
    .w-power-saver = Energiesparen
    .w-performance = Leistung
    .w-balanced = Ausgewogen

d-ijtt4qaw = Kopier-Bezeichnung
    .w-a-picture = Ein Bild
    .w-a-copy = Eine Kopie

d-xlwpoanc = Kopiert von
    .w-from = Von { $from }

d-b7ima7hy = Akkuwörter
    .w-minutes-left = { $percentage } %, noch { $left } Minuten

d-rdw57vib = Akkustatus
    .w-charging = wird geladen
    .w-on-the-battery = im Akkubetrieb
    .w-empty = leer
    .w-full = voll
    .w-about-to-charge = lädt gleich
    .w-about-to-drain = entlädt sich bald
    .w-plugged-in = am Strom

d-5bw77ouj = Gerätezeile
    .w-takes-the-sound-you-send = Nimmt den Ton an, den du sendest
    .w-here-now = Jetzt hier
    .w-away = Abwesend

d-fmkn7v3q = Verbindungstitel
    .w-sharing-this-connection-with = Diese Verbindung wird mit { $label } geteilt
    .w-using-s-connection = Die Verbindung von { $label } wird genutzt

d-zxfismzy = Verbindungszeile
    .w-connected-over-its-hotspot = Über seinen Hotspot { $network } verbunden
    .w-joining-the-hotspot = Tritt dem Hotspot { $network } bei
    .w-waiting-for-your-answer = Wartet auf deine Antwort
    .w-waiting-for-an-answer-there = Wartet dort auf eine Antwort

d-annpbov6 = Ort in der Nähe
    .w-away = Abwesend
    .w-receiver = Empfänger
    .w-in-reach = In Reichweite

d-fezzuk6j = Übertragung läuft
    .w-from = Von { $peer }, { $progress } %
    .w-to = An { $peer }, { $progress } %

d-6p4qx5lf = Steuerungsbezeichnung
    .w-wi-fi = WLAN
    .w-bluetooth = Bluetooth
    .w-vpn = VPN
    .w-night-light = Nachtlicht
    .w-airplane-mode = Flugmodus
    .w-mobile-data = Mobile Daten
    .w-do-not-disturb = Nicht stören
    .w-rotation-lock = Rotationssperre
    .w-power-mode = Energiemodus

d-mqcgotiw = Stiltitel
    .w-strips = Streifen

d-inhjxrxz = Fenstersymbol
    .w-app-window = app-window

d-24skrsf6 = Abfrageüberschrift
    .w-looking-for-in = Suche nach { $text } in { $in_app }

d-3fx2uvyj = Menüleistenname
    .w-a-window = Ein Fenster

d-bzkjwd5u = Ortstitel
    .w-window = Fenster

d-xgvy7oje = Personen

d-avkcokw5 = Personentafel
    .w-people = Personen
    .w-who-is-here-where-they-are = Wer hier ist, wo sie sind und was sie tun
    .w-keep-on-screen = Auf dem Bildschirm behalten
    .w-close = Schließen

d-vb456dpi = Liste
    .w-you = Du
    .w-nobody-else-is-here = Niemand sonst ist hier
    .w-invite-someone-to-this-workspace-or = Lade jemanden in diesen Arbeitsbereich ein oder öffne denselben auf einem anderen Gerät
    .w-people-here-now = Jetzt hier
    .w-people = Personen
    .w-agents = Agenten
    .w-invite-someone = Jemanden einladen

d-djpxekrm = Person in der Liste
    .w-someone = Jemand
    .w-wave-at = { $label } zuwinken
    .w-stop-following = { $label } nicht mehr folgen
    .w-follow = { $label } folgen

d-vyjuo52r = Zeile einer Person
    .w-someone = Jemand
    .w-joined = Beigetreten um { $joined }
    .w-go-where-works = Dorthin gehen, wo { $name } arbeitet
    .w-go-there = Dorthin

d-uw37p3tz = Titel der Liste
    .w-who-is-here = Wer hier ist
    .w-people = Personen

d-yvvycv2a = Hier-Zeile
    .w-just-you = Nur du
    .w-you-and-1-other = Du und 1 weitere Person
    .w-you-and-others = Du und { $others } weitere

d-erf3idj7 = Software-Art
    .w-bot = Bot
    .w-agent = Agent

d-2gu42kpe = Folgt-Zeile
    .w-following-you = { $place }, folgt dir
    .w-following-you-2 = Folgt dir
    .w-following = { $place }, folgt { $who }
    .w-following-2 = Folgt { $who }

d-vmdtf5fq = Tut-Zeile
    .w-in = In { $call }
    .w-focusing = Fokussiert
    .w-away = Abwesend
    .w-idle = Inaktiv

d-ph4t2ijg = Winken-Schaltfläche
    .w-waved = Gewunken
    .w-you-waved-at = Du hast { $name } gewunken
    .w-wave = Winken
    .w-wave-at = { $name } zuwinken

d-nf2tfrzm = Folgen-Schaltfläche
    .w-stop = Stopp
    .w-stop-following = { $name } nicht mehr folgen
    .w-follow = Folgen
    .w-follow-2 = { $name } folgen

d-b7z6kr5g = Bildschirm darunter
    .w-nothing-open-on-their-screen = Auf dem Bildschirm ist nichts offen
    .w-s-screen = Bildschirm von { $name }
    .w-keeps-their-place-private = Behält für sich, wo die Person gerade ist
    .w-someone = Jemand

d-lkpaptdc = Karte einer Person
    .w-stop-following = Nicht mehr folgen
    .w-follow = Folgen
    .w-message = Nachricht
    .w-wave = Winken
    .w-go-there = Dorthin gehen
    .w-see-its-work = Seine Arbeit ansehen
    .w-now = Jetzt
    .w-in-a-call = In einem Anruf: { $call }
    .w-devices = Geräte
    .w-this-computer = Dieser Computer
    .w-another-device = Ein anderes Gerät
    .w-copy-the-key-of = Schlüssel von { $label } kopieren
    .w-copy-this-device-s-key = Schlüssel dieses Geräts kopieren
    .w-this-device = dieses Gerät
    .w-person-id = Personen-ID
    .w-copy-id = ID kopieren
    .w-linked-accounts = Verknüpfte Konten
    .w-linked-to-on = Verknüpft mit @{ $handle } bei { $service }

d-nw3m2rzj = Kopf der Karte
    .w-someone = Jemand
    .w-follows-you = Folgt dir
    .w-written-automatically-from-what-they-do = Automatisch aus dem geschrieben, was sie tun
    .w-in-their-own-words = In ihren eigenen Worten
    .w-in = In { $in_app }

d-sfputxmu = Eigene Karte
    .w-change-picture = Bild ändern
    .w-remove-picture = Bild entfernen
    .w-add-your-name = Nenne dich
    .w-your-name = Dein Name
    .w-how-everyone-here-sees-you = So sehen dich alle hier
    .w-what-you-re-doing = Was du gerade machst
    .w-status = Status
    .w-share-what-you-re-doing = Teilen, was du machst
    .w-what-people-in-see = Was Menschen in { $here_workspace } sehen
    .w-what-people-here-see = Was Menschen hier sehen
    .w-your-colour = Deine Farbe
    .w-your-identity = Deine Identität
    .w-show-control-hints = Bedienhinweise zeigen

d-fd6cx4va = Agenten-Befehle
    .w-stop = Stopp
    .w-undo-run = Lauf zurücknehmen
    .w-take-over = Übernehmen
    .w-revoke-access = Zugriff entziehen

d-rjk6bai6 = Zurück zu allen
    .w-back-to-everyone = Zurück zu allen

d-lmr4ktas = Folgen-Chip
    .w-following = Du folgst { $name }
    .w-stop = Stopp
    .w-stopped-following = Folgen beendet
    .w-stop-following = { $name } nicht mehr folgen

d-xr7n4tqa = Gesicht der gefolgten Person

d-ehkl2kj3 = Gesicht der Person

d-ckxeoehf = Uhr, wessen Ansicht du folgst oder Neuigkeiten daneben

d-hkigcrhq = Teilen-Anzeige
    .w-sharing-1-window = 1 Fenster geteilt
    .w-sharing-windows = { $windows } Fenster geteilt
    .w-stop-sharing = Teilen von { $shown } beenden
    .w-stop-sharing-2 = Teilen beenden

d-5mioahup = Wink-Hinweis
    .w-waved-at-you = { $name } hat dir gewunken

d-6g5ux35b = Ankunfts-Hinweis
    .w-is-here = { $short } ist hier
    .w-and-1-other-are-here = { $short } und 1 weitere Person sind hier
    .w-and-others-are-here = { $short } und { $others } weitere sind hier

d-5xj23ufk = Frage-Karte
    .w-not-now = Jetzt nicht

d-ktt65xag = Teilen-Steuerung
    .w-shared-with-the-workspace = Mit dem Arbeitsbereich geteilt
    .w-shared-watching = Geteilt, { $watching } sehen zu
    .w-stop-sharing = Teilen beenden
    .w-stop-letting-them-control-it = Steuerung nicht mehr erlauben
    .w-let-them-control-it = Steuerung erlauben
    .w-share-this-window = Dieses Fenster teilen

d-ltdydcpb = Widget-Art
    .doc = Die Widgets, die jemand auf seiner Shell behalten kann.

d-pbntwsxq = Musik

d-lf7cacbm = Personen hier

d-gw4nk6ha = Kalender

d-3flqhgw3 = Systemlast

d-6dc6ihf7 = Widget verschieben
    .doc = Legt das Widget `kind` dort ab, wo ein Ziehen auf diesem Bildschirm bei `at` losließ: den Ort, an dem die Person es ließ, als Anteile des Raums, in dem es sich dort bewegen kann.

d-6dc6ihf7-kind = Art

d-6dc6ihf7-at = Bei

d-6dc6ihf7-room = Raum

d-6dc6ihf7-display = Bildschirm

d-vvqlyiwr = Widget-Name
    .w-music = Musik
    .w-people-here = Personen hier
    .w-calendar = Kalender
    .w-system-load = Systemlast

d-tadzd3lm = Widget-Kurzform
    .w-here = { $len } hier

d-vlo5ghj4 = Lastangabe
    .w-cpu-memory = { $to_int } % CPU · { $to_int2 } % Speicher

d-3qxhd5dh = Widget-Ebene
    .w-menu = Menü
    .w-back-after-a-pause = Nach einer Pause zurück
    .w-always-on-this-strip = Immer auf diesem Streifen
    .w-only-when-i-ask = Nur wenn ich frage
    .w-remove-from-the-shell = Von der Shell entfernen
    .w-fold = Einklappen
    .w-open = { $widget_name } öffnen
    .w-removed = { $widget_name } entfernt
    .w-undo = Rückgängig
    .w-dismiss = Schließen

d-fbr67wmc = Musik-Widget
    .w-position-in = Position in { $live_name }
    .w-open = { $source } öffnen

d-wfzaobux = Personen-Widget
    .w-show-who-is-here = Zeigen, wer hier ist

d-7deyrm3z = Kalender-Widget

d-6pn3zoo7 = Monatsraster

d-zx3hotic = Last-Widget
    .w-cpu = CPU
    .w-processor-load = Prozessorlast
    .w-of-gb-memory = { $to_int } von { $to_int2 } GB Speicher

d-q6dyi7r7 = Zugriffsangabe
    .w-only-you = Nur du
    .w-shared-with = Geteilt mit { $name }
    .w-shared-with-and = Geteilt mit { $name } und { $second }
    .w-shared-with-and-others = Geteilt mit { $name } und { $more } weiteren

d-amkomces = App-Zeile
    .w-on = { $origin_app } auf { $device_name }

d-3ytya6pk = Gesicht-Schaltfläche
    .w-someone = Jemand
    .w-follows-you = { $name }, folgt dir

d-32e233z5 = Verlässt
    .doc = Ob sein Fenster gerade geschlossen wurde: der Platz steht nur, solange die Abfahrt seines Rahmens läuft.

d-54wspwy2 = Auslöser
    .doc = Der Auslöser, den die Person dafür gedrückt hat, in Pixeln der Shell als `x, y`: das Dock-Symbol oder die Suchzeile, aus der das Fenster kommt und in die es zurückgeht. Leer, wenn eine Tastenkombination oder ein Agent es angefordert hat; das landet und wächst, wo es steht.

d-kxssatr3 = Verlässt
    .doc = Ob das Fenster gerade geschlossen wurde: die Sitzung hält es in der Lesung, während die Shell die Abfahrt seines Rahmens zeichnet, zurück in den Auslöser, mit dem die Person es geöffnet hat.

d-w25t6rl7-filter-leaving = Verlässt

d-farfsl4d-filter-leaving = Verlässt

d-ohzicl6b = Gehendes Fenster

d-q2ljncu4 = Gehender Platz

d-jbsxxbyx = Von hier aus geteilt
    .w-shared-out-of-here = Von hier aus geteilt
    .w-shared-out = Von hier aus geteilt

d-ndz2uyn2 = Freigabezeile
    .w-take-back = Zurücknehmen

d-73qaarxf = Freigabeangabe
    .w-anyone-with-the-link = jeder mit dem Link
    .w-someone = jemand
    .w-an-unnamed = jemand ohne Namen
    .w-can-edit = darf bearbeiten
    .w-can-view = darf ansehen
    .w-outside-this-workspace = { $who }, außerhalb dieses Arbeitsbereichs, { $what }
d-qoiyh5y7 = Eine Webseite öffnen
    .doc = Eine Webseite in der App öffnen, die Webadressen öffnet.

d-qoiyh5y7-url = URL

d-jexq3d24 = Hostworte
    .w-reading-your-account = Lese dein Konto…
    .w-your-host-could-not-be-reached = Dein Host war nicht erreichbar.
    .w-not-signed-in-yet-so-there = Noch nicht angemeldet, daher kein Tarif.
    .w-your-workspaces-run-on-this-machine = Deine Arbeitsbereiche laufen auf diesem Rechner selbst, es gibt nichts zu bezahlen.
    .w-no-plan-yet = Noch kein Tarif
    .w-of-this-period = { $machine_time } von { $machine_time2 } in diesem Zeitraum
    .w-nothing-used-this-period = In diesem Zeitraum nichts genutzt
    .w-this-period = { $machine_time } in diesem Zeitraum
    .w-credit = { $plan } · { $used } · { $credit } Guthaben

d-cpntobds = Agentenworte
    .w-removed-every-device-of-yours-refuses = Entfernt; jedes deiner Geräte lehnt ihn ab
    .w-a-program-of-yours-acting-as = Ein Programm von dir, das als du handelt
    .w-online-now-acting-as-you = Jetzt online, handelt als du
    .w-acting-as-you = Handelt als du

d-p4iyapc6 = Maschinenzeit
    .w-hr-min = { $hours } Std. { $minutes } Min.
    .w-hr = { $hours } Std.
    .w-min = { $minutes } Min.
    .w-s = { $seconds } s

d-xai2nlf5 = Dein Tilo
    .w-your-tilo = Dein Tilo
    .w-more-workspaces = Weitere Arbeitsbereiche
    .w-on-one-of-your-hosts = Auf einem deiner Hosts

d-znvl3q45 = Hosts
    .w-hosts-compute = Hosts & Rechenleistung
    .w-no-other-machine-runs-your-workspaces = Kein anderer Rechner führt deine Arbeitsbereiche aus
    .w-this-computer-runs-them-itself-and = Dieser Rechner führt sie selbst aus, es gibt nichts zu bezahlen.
    .w-manage-billing = Abrechnung verwalten
    .w-add-a-card = Karte hinzufügen

d-m54htipi = Agenten
    .w-agents-let-in = Zugelassene Agenten
    .w-no-agent-acts-as-you-yet = Noch kein Agent handelt als du
    .w-an-agent-you-let-in-shows = Ein Agent, den du zulässt, steht hier, mit dem Weg, ihn zu entfernen.

d-pou22ylc = Agentenmenü
    .w-manage = { $label } verwalten
    .w-remove-no-longer-used = { $label } entfernen — nicht mehr genutzt
    .w-remove-it-was-lost = { $label } entfernen — verloren
    .w-remove-it-was-stolen = { $label } entfernen — gestohlen
    .w-remove-someone-knows-its-key = { $label } entfernen — jemand kennt den Schlüssel

d-zmfop5wt = Fortsetzen-Angebot
    .w-continue-from = { $running } von { $elsewhere } fortsetzen
    .w-dismiss-from = { $running } von { $elsewhere } wegwischen


d-4sj6y354 = Behaltene Liste
    .w-people = Personen
    .w-who-is-here-where-they-are = { $named }. Wer hier ist, wo und was sie gerade tun
    .w-put-back-in-the-bar = Zurück in die Leiste
    .w-who-is-here = Wer hier ist
    .w-agents = Agenten
    .w-invite-people = Personen einladen
    .w-you-can-t-invite-people-to = Du kannst niemanden in diesen Arbeitsbereich einladen. Wer beitritt, entscheidet seine Inhaberin oder sein Inhaber.

d-d77soeer = Zeigen, wer hier ist
    .doc = Was die Gesichter in der Leiste tun: zeigen, wer hier ist, oder es wieder schließen.

d-npcipsge = Anwesende auf dem Bildschirm behalten
    .doc = Behält die Anwesenden am Rand auf dem Bildschirm, an dem die Person sie zuletzt behielt, oder legt sie zurück in die Leiste.

d-npcipsge-kept = Behalten

d-payy22lj = Anwesende verschieben
    .doc = Verschiebt die auf dem Bildschirm behaltenen Anwesenden an den Rand `edge`, wie ein dort losgelassenes Ziehen.

d-payy22lj-edge = Rand

d-okr3ibql = Im Fenster suchen
    .doc = Die Suchtaste der Fensterleiste: die eigene Suche des Fensters klappt das Feld der Leiste auf, wo seine App etwas an Ort und Stelle zu finden hat, und sonst öffnet sich die Systemsuche, eingeengt auf die App.

d-okr3ibql-w = Fenster

d-pjy75m5i = Suche erweitern
    .doc = Erweitert die Suche eines Fensters und nimmt das Getippte mit: über alles, was seine App hält (`app`), oder über alles (`everywhere`), durch die Systemsuche, die die Shell für dieses Ziel öffnet. Das Feld der Leiste schließt sich mit ihr.

d-pjy75m5i-w = Fenster

d-pjy75m5i-reach = Ziel

d-pjy75m5i-text = Text

d-racosa5u = Suche bestätigen
    .doc = Was die Eingabetaste in der Suche eines Fensters fragt: den nächsten Treffer, solange ein Dokument welche hat, sonst die eigene Suche der App über das Getippte.

d-racosa5u-w = Fenster

d-racosa5u-text = Text


d-frnkdv3w = Dein Winken ist angekommen
    .w-someone = Jemand
    .w-you-waved-at = Du hast { $name } gewunken

d-quzzxrza = Wie du teilst
    .w-my-words = Meine Worte
    .w-not-at-all = Gar nicht
    .w-automatically = Automatisch

d-7aupcdya = Wer was sieht
    .w-that-i-m-here = Dass ich hier bin
    .w-nothing = Nichts
    .w-where-i-am = Wo ich bin

d-yt2eannm = Was die Wahl bedeutet
    .w-your-face-never-which-window-or = Dein Gesicht, nie welches Fenster oder dein Zeiger
    .w-you-don-t-appear-and-you = Du erscheinst nicht, und du siehst die Zeiger anderer nicht
    .w-your-face-the-window-you-re = Dein Gesicht, das Fenster, in dem du bist, und dein Zeiger
