# Shell auf Deutsch. Keys are the declarations' stable ids (app.ids); see locales/en-US.ftl.

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

d-clhqzrkb = Breite
    .doc = Wie breit eine Leiste in ihrer Zeile steht, in Promille der Zeilenbreite.

d-hrjwxtwf = Drei Zehntel der Zeile: eine Liste, eine Reihe von Optionen
    .doc = Drei Zehntel der Zeile: eine Liste, eine Reihe von Optionen.

d-ipingyqi = Zwei Drittel der Zeile: eine Notiz, eine Unterhaltung
    .doc = Zwei Drittel der Zeile: eine Notiz, eine Unterhaltung.

d-nfufnx7j = Die ganze nutzbare Breite der Zeile
    .doc = Die ganze nutzbare Breite der Zeile.

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

d-vipg3gla = Synchronisierung

d-agni63m4 = Akku
    .w-charging = { $percentage } %, wird geladen

d-moh3hzne = Glocke
    .w-notifications-silenced = Benachrichtigungen stumm
    .w-notifications = Benachrichtigungen
    .w-1-unread = { $name }, 1 ungelesen
    .w-unread = { $name }, { $waiting } ungelesen

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
    .w-no-devices-in-reach = Keine Geräte in Reichweite

d-niotqkfd = Bluetooth weitergeben
    .w-move-it-to = Verschieben nach…

d-2abiywd2 = Kachel-Editor
    .w-customise-the-panel = Bedienfeld anpassen
    .w-panel-controls = Kacheln des Bedienfelds
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

d-outvdzmi = Layouts
    .w-chromepreset = Rahmenvorgabe

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

d-clgbrhsn = Hinweise
    .w-notifications = Benachrichtigungen
    .w-clear-all = Alle löschen
    .w-nothing-to-catch-up-on = Nichts aufzuholen
    .w-search-notifications = Benachrichtigungen durchsuchen
    .w-nothing-matches-that = Nichts passt dazu
    .w-clear-the-search-to-see-every = Lösche die Suche, um jeden Hinweis zu sehen

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
    .w-read = Lesen
    .w-dismiss = Verwerfen

d-76zs5p4r = Antwortzeile
    .w-reply = Antworten
    .w-send = Senden
    .w-write-a-reply-first = Schreib zuerst eine Antwort

d-ny7ms2ry = Bildschirmanzeige

d-rcjzvfxp = Übersicht
    .w-overview = Übersicht
    .w-new-workspace = Neuer Arbeitsbereich
    .w-join-a-workspace = Einem Arbeitsbereich beitreten
    .w-search-windows = Fenster durchsuchen
    .w-no-windows = Keine Fenster
    .w-open-something-to-see-its-window = Öffne etwas, um sein Fenster zu sehen

d-766s7y4k = Arbeitsbereiche hier
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
    .w-new = { $name }, { $unseen } neu
    .w-show-who-is-here = Zeigen, wer hier ist
    .w-stop = Stopp

d-teccuwja = Schnelleinstellungen
    .w-quick-settings = Schnelleinstellungen

d-dd3gpi64 = Steuerungskachel

d-vzdj2kff = Dreh-Kachel
    .w-rotation-lock = Drehsperre

d-yxl6yq2r = WLAN-Kachel
    .w-wi-fi = WLAN
    .w-this-device-has-no-wi-fi = Dieses Gerät hat kein WLAN

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

d-thduakjg = Netzwerkbereich
    .w-wi-fi = WLAN
    .w-wired-network = Kabelnetzwerk
    .w-connected = Verbunden
    .w-cable-unplugged = Kabel nicht angeschlossen
    .w-wi-fi-is-off = WLAN ist aus
    .w-scanning-for-networks = Netzwerke werden gesucht …
    .w-enter-the-password-for = Gib das Passwort für { $password_for } ein
    .w-password = Passwort
    .w-join = Verbinden
    .w-cancel = Abbrechen
    .w-hidden-network = Verborgenes Netzwerk
    .w-join-a-network-that-does-not = Einem Netzwerk beitreten, das seinen Namen nicht sendet
    .w-network-name = Netzwerkname
    .w-saved-networks = Gespeicherte Netzwerke

d-zjz5sskl = Bluetooth-Bereich
    .w-bluetooth = Bluetooth
    .w-bluetooth-is-off = Bluetooth ist aus
    .w-stop-looking = Suche beenden
    .w-look-for-devices = Nach Geräten suchen
    .w-no-devices-found = Keine Geräte gefunden

d-lcjohgdk = Akkulinie

d-ycck5awo = Ein/Aus-Zeile
    .w-lock = Sperren
    .w-settings = Einstellungen
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

d-7nsn4srj = Arbeitsbereichskarte
    .w-here = { $len } hier

d-xzb2gybw = Stilumschalter

d-kssxjs7v = Stilmenü
    .w-showing = Zeigt { $style_title }
    .w-settings-for = Einstellungen für { $style_title }

d-qrahog47 = Leistenansicht
    .w-no-window-here-yet = Noch kein Fenster hier

d-4qeo26jr = Zeilen der Arbeitsbereiche

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

d-fziuz57n = Wechsler
    .w-of = { $at } von { $count }
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

d-4zknrelh = Einstellungen offen
    .doc = Ob die eigenen Einstellungen des gewählten Stils über der Shell stehen (`style_settings`), geöffnet aus dem Stilmenü der Leiste: der eine Ort, an dem eine Person den gezeigten Stil ohne Agent ändert, wie es `shell.md` §6 sagt.

d-dwct4abs = Reihen-Tabs
    .doc = Reihen: ob eine Reihe mit mehreren Plätzen ihre Tableiste zeigt.

d-gfmzi3qr = Rahmen einrasten
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

d-5giytccr = Erstes öffnen
    .doc = Was Enter bei einer Anfrage tut: Die erste App, die der Text findet, öffnet sich, sodass „no“ und Enter Notizen öffnen; eine Anfrage, die keine App nennt, beantwortet die Suche überall, wie Enter es schon immer für das tat, was die eigenen Zeilen der Shell nicht enthalten.

d-5giytccr-query = Anfrage

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

d-wln2heq6 = Nach vorn holen
    .doc = Holt das Fenster der Leiste `s` nach vorn, mit der Tastatur darin: die Sitzung holt seinen Platz nach vorn, wie bei einem Druck, einem Skript oder einem Agenten.

d-wln2heq6-s = S

d-jipcl6u3 = Einstellungen des Stils
    .doc = Die eigenen Einstellungen des Stils und die zwei Wege, einen Stil zu ändern (`docs/designs/app-language/ shell.md` §6): ohne Agent, an dem einen Ort, an dem die Einstellungen einer Person liegen, und mit Agent, über dieselben Aktionen, die die Bedienelemente der Person ausführen.

d-wxxxvca4 = Schließen
    .doc = Schließt sie.

d-33wxlq7h = Einstellungen des Stils
    .doc = Zeigt oder verbirgt sie: ein Bedienelement für beides, damit eine Person den Zustand dort sieht, wo sie drückt.

d-33wxlq7h-on = An

d-zitymayc = Reihen-Tabs
    .doc = Ob die Reihe mit mehreren Plätzen ihre Tableiste zeigt: die eigene Einstellung des Reihenstils.

d-zitymayc-on = An

d-lfa3rl6s = Plätze am Raster einrasten
    .doc = Ob ein auf der gerahmten Arbeitsfläche losgelassener Platz auf dem Raster landet: die eigene Einstellung des Rahmenstils.

d-lfa3rl6s-on = An

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
    .w-when-the-dock-shows = Wann das Dock zeigt

d-e3tczbcu = Einstellungen des Reihenstils
    .w-show-the-tabs-of-a-strip = Tabs einer Reihe zeigen

d-mnbjennf = Einstellungen des Rahmenstils
    .w-snap-places-to-a-grid = Plätze am Raster einrasten

d-xwhxaimf = Einstellungen des rahmenlosen Stils
    .w-frameless-shows-each-place-as-its = Rahmenlos zeigt jeden Platz als eigene Ansicht und hat nichts einzustellen.

d-st426t4p = Ein vorderer Arbeitsbereich
    .doc = Fügt `a` den vorderen Arbeitsbereich mit seiner vorderen Zeile hinzu und gibt den Schlüssel der Zeile zurück.

d-st426t4p-name = Name

d-6bamt22e = Auf der Fläche gefragt
    .doc = Die Fläche: ob die Person auf ihr schon gefragt hat.

d-ukun4zmh = Auf der Fläche gefragt
    .doc = Die Person hat auf der Fläche gefragt: der Hinweis, den die leere Fläche bis dahin sagt, ist gesagt (`Shell.canvas_asked`) und bleibt von nun an fort.

d-dgtjjxex = Menü der Person
    .w-you = Du
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

d-zlo3t3an = Aufnahmestatus
    .w-watching = { $label } sieht zu
    .w-watching-and-more = { $label } und { $more } weitere sehen zu
    .w-watching-2 = Sieht zu

d-fcxy4mu5 = Shell-Ansicht

d-iavyg4wt = Shell-Oberflächen
    .w-the-shell = Die Shell
    .w-overview = Übersicht
    .w-workspaces = Arbeitsbereiche
    .w-notifications = Benachrichtigungen

d-54b5rqo4 = Zeilen der Shell
    .w-no-window-here-yet = Noch kein Fenster hier

d-k4p3z4ee = Vorn
    .doc = Die Leiste, deren Fenster auf diesem Gerät die Tastatur hat, nach ihrem Schlüssel; leer für keine.

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

d-4s5jxxte-filter-current = Aktionsleiste

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
