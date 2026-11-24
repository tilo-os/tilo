imager-review-setup-hostname = Mit dem Hostnamen { $hostname } geschrieben, dann überprüft
imager-setup-action-next = Einrichtung fortsetzen
imager-setup-action-start-download = Tilo herunterladen
imager-setup-action-pause-download = Download anhalten
imager-setup-action-resume-download = Download fortsetzen
imager-setup-action-cancel-download = Download abbrechen
imager-setup-action-discard-download = Unvollständige Downloaddaten verwerfen
imager-setup-action-release-download = Download für später behalten
imager-setup-error-download-start-unavailable = Ein Download von Full Tilo kann in diesem Stand nicht beginnen.
imager-setup-error-download-unavailable = Der vollständige Tilo-Download ist nicht verfügbar.
imager-setup-error-download-review-only = Starte den Download in der Prüfung von Full Tilo.
imager-setup-error-download-pause-unavailable = Der Download läuft gerade nicht
imager-setup-error-download-resume-unavailable = Dieser gespeicherte Download kann noch nicht fortgesetzt werden
imager-setup-error-download-cancel-unavailable = Der Download läuft gerade nicht
imager-setup-error-download-discard-unavailable = Es gibt keine unvollständigen Downloaddaten zum Verwerfen
imager-setup-error-download-release-unavailable = Es gibt keinen überprüften Download, der für später behalten werden kann
imager-setup-error-download-no-folder = Tilo findet auf diesem Computer den Ordner für Downloads nicht.
imager-setup-error-download-unreadable = Der gespeicherte Tilo-Download lässt sich nicht lesen.
imager-setup-error-download-unsupported = Auf diesem Computer lässt sich das vollständige Tilo nicht herunterladen.
imager-setup-error-download-changed = Dieser Download hat sich seit deiner Wahl geändert. Prüfe den aktuellen, bevor du fortfährst.
imager-setup-error-download-selection-too-large = Die gespeicherte Download-Auswahl ist größer, als dieser Computer aufbewahrt.
imager-setup-error-download-selection-invalid = Die gespeicherte Download-Auswahl kann Tilo nicht verwenden.
imager-setup-error-download-lost = Der geprüfte Download liegt nicht mehr auf diesem Computer.
imager-setup-error-download-unsaved = Die Download-Auswahl ließ sich nicht speichern.
imager-setup-error-staging-unavailable = Die geschützte Bereitstellung ist nicht verfügbar.
imager-setup-error-staging-not-built = Die geschützte Bereitstellung ist in diesem Build nicht enthalten.
imager-setup-error-staging-not-built-retained = Die geschützte Bereitstellung ist in diesem Build nicht enthalten; der geprüfte Download wurde aufbewahrt.
imager-setup-resource-root = Geführte Tilo-Einrichtung
imager-setup-badge-review = Prüfung
imager-setup-resource-internal = Installation von Full Tilo
imager-setup-badge-internal = Full Tilo
imager-setup-detail-internal = { $readiness }
imager-setup-action-internal-set-target-size = Größe von Tilo festlegen
imager-setup-action-internal-install = Tilo neben dem aktuellen System installieren
imager-setup-action-internal-restart = In das Tilo-Installationsprogramm neu starten
imager-setup-action-internal-restart-later = Später neu starten
imager-setup-action-internal-retry = Das Tilo-Installationsprogramm beim nächsten Neustart starten
imager-setup-action-internal-remove = Das Tilo-Installationsprogramm entfernen
imager-setup-action-internal-firmware-settings = In die Firmware-Einstellungen neu starten
imager-setup-action-internal-disable-fast-startup = Schnellstart ausschalten
imager-setup-action-internal-recheck = Diesen Computer erneut prüfen
imager-setup-internal-error-not-review = Befehle zur Installation von Full Tilo gibt es in der Prüfung von Full Tilo.
imager-setup-internal-error-command-unavailable = Dieser Befehl ist im aktuellen Stand der Installation nicht verfügbar.
imager-setup-internal-error-unavailable = Die Installation von Full Tilo ist auf diesem Computer nicht verfügbar.
imager-setup-internal-error-size-range = Wähle eine Größe für Tilo zwischen { $min } und { $max } GiB.
imager-setup-internal-error-no-plan = Für dieses Laufwerk gibt es noch keinen Installationsplan.
imager-setup-internal-error-plan-changed = Der Installationsplan hat sich geändert. Prüfe ihn erneut, bevor du installierst.
imager-setup-internal-error-blocked = Etwas verhindert die Installation noch. Behebe die Punkte unter „Vor der Installation“.
imager-setup-internal-error-download = Lade Tilo herunter und überprüfe es, bevor du installierst.
imager-setup-internal-error-running = Das Hilfsprogramm des Tilo-Installationsprogramms läuft bereits.
imager-setup-internal-error-staged = Das Tilo-Installationsprogramm ist bereits eingerichtet. Starte neu, um es zu öffnen.
imager-setup-internal-error-no-pending = Auf diesem Computer gibt es kein unvollständiges Tilo-Installationsprogramm.
imager-setup-internal-error-pending-changed = Dieses unvollständige Tilo-Installationsprogramm ist nicht mehr das aktuelle.
imager-setup-internal-error-no-restart = Das Tilo-Installationsprogramm ist noch nicht zum Starten eingerichtet.
imager-setup-internal-error-remedy-unavailable = Diese Abhilfe passt gerade nicht zu diesem Computer.
imager-setup-internal-checking =
    Es wird geprüft, ob Tilo auf diesem Computer neben { $host ->
        [windows] Windows
        [linux] deinem Linux-System
       *[other] deinem aktuellen System
    } installiert werden kann…
imager-setup-check-title = Diesen Computer prüfen
imager-setup-check-only = Hier sind nur Kompatibilitätsprüfungen verfügbar. Fahre fort, um auszuwählen, was du mitnimmst.
imager-setup-check-not-ready = Behebe die Kompatibilitätsprüfungen, bevor du fortfährst.
imager-setup-check-not-enabled = Die direkte Installation ist in diesem Build nicht aktiviert. Geh zurück und bereite stattdessen ein USB-Laufwerk vor.
imager-setup-internal-not-enabled = Die direkte Installation ist in diesem Build nicht aktiviert.
imager-setup-internal-no-release = Dieser Build hat kein Installationsprogramm für den Prozessor dieses Computers.
imager-setup-internal-unsupported = Tilo kann auf diesem Computer nicht neben dem aktuellen System installiert werden: { $reasons }
imager-setup-internal-ready =
    Tilo kann auf diesem Computer neben { $host ->
        [windows] Windows
        [linux] deinem Linux-System
       *[other] deinem aktuellen System
    } installiert werden.
imager-setup-internal-plan-title = Installationsplan
imager-setup-internal-plan-checking = Das Laufwerk dieses Computers wird geprüft…
imager-setup-internal-plan-shrink =
    { $host ->
        [windows] Windows (C:)
       *[other] Deine Systempartition
    } wird um { $shrink } verkleinert, von { $from } auf { $to }.
imager-setup-internal-plan-free = Freier, nicht partitionierter Platz auf diesem Laufwerk nimmt Tilo auf. Keine vorhandene Partition wird in der Größe verändert.
imager-setup-internal-plan-target = Tilo bekommt { $target }.
imager-setup-internal-plan-staging = Das Tilo-Installationsprogramm belegt zusätzlich { $staging }, während es läuft. Dieser Platz geht nach deiner ersten Anmeldung an Tilo.
imager-setup-internal-plan-error-space = Auf diesem Laufwerk ist nicht genug sicherer Platz. Tilo und sein Installationsprogramm brauchen { $needed }, verfügbar gemacht werden können aber nur { $available }.
imager-setup-internal-plan-error-other = Für dieses Laufwerk konnte kein Installationsplan erstellt werden: { $detail }
imager-setup-internal-size-label = Größe von Tilo
imager-setup-internal-size-help =
    Wähle zwischen { $min } und { $max } GiB. Mehr Platz für Tilo lässt weniger für { $host ->
        [windows] Windows
       *[other] das aktuelle System
    }.
imager-setup-internal-expect =
    { $host ->
        [windows] Windows bleibt installiert und startet weiterhin standardmäßig.
       *[other] Dein aktuelles System bleibt installiert und startet weiterhin standardmäßig.
    } Die Installation braucht einen Neustart in das Tilo-Installationsprogramm, wo du noch einmal bestätigst, bevor etwas installiert wird. Secure Boot muss ausgeschaltet sein.
imager-setup-internal-unverified = Sichere alles Wichtige, bevor du fortfährst.
imager-setup-internal-blockers-title = Vor der Installation
imager-setup-internal-blockers-none = Auf diesem Computer verhindert nichts die Installation.
imager-setup-internal-blocker-not-uefi = Dieser Computer startet im alten BIOS-Modus. Tilo braucht einen Start über UEFI.
imager-setup-internal-blocker-secure-boot-on = Secure Boot ist eingeschaltet. Schalte es in den Firmware-Einstellungen dieses Computers aus und öffne dann die Einrichtung erneut. Die Einrichtung ändert niemals die Schlüssel von Secure Boot.
imager-setup-internal-blocker-bitlocker-locked = Das Systemlaufwerk ist mit BitLocker verschlüsselt, und sein Schutz kann für den Neustart nicht ausgesetzt werden.
imager-setup-internal-blocker-not-gpt = Das Systemlaufwerk verwendet das ältere MBR-Layout. Tilo braucht ein GPT-Laufwerk.
imager-setup-internal-blocker-disk-unsupported = Diese Laufwerkskonfiguration wird nicht unterstützt (zum Beispiel Speicherplätze, ein dynamischer Datenträger, RAID oder ein Wechseldatenträger), oder das Laufwerk konnte nicht gelesen werden.
imager-setup-internal-blocker-hibernation = Schnellstart oder Ruhezustand ist eingeschaltet, daher fährt Windows nicht vollständig herunter. Schalte es vor der Installation aus.
imager-setup-internal-blocker-insufficient-space = Es ist nicht genug freier Platz für Tilo und sein Installationsprogramm vorhanden, auch nicht nach sicherem Verkleinern der Systempartition.
imager-setup-internal-blocker-on-battery = Dieser Computer läuft mit weniger als 50 % Akku. Schließe ihn vor der Installation an den Strom an.
imager-setup-internal-blocker-pending-operation = Eine frühere Einrichtung des Tilo-Installationsprogramms ist unvollständig. Setze sie unten fort oder entferne sie.
imager-setup-internal-blocker-arch-mismatch = Keine Tilo-Version passt zum Prozessor dieses Computers.
imager-setup-internal-remedy-firmware = In die Firmware-Einstellungen neu starten
imager-setup-internal-remedy-fast-startup = Schnellstart ausschalten
imager-setup-internal-recheck = Erneut prüfen
imager-setup-internal-install = Tilo installieren
imager-setup-internal-continue = Installation fortsetzen
imager-setup-internal-installing = Wird installiert…
imager-setup-internal-run-title = Tilo-Installationsprogramm
imager-setup-internal-run-waiting = Warten auf die Zustimmung eines Administrators…
imager-setup-internal-run-progress = { $step }: { $done } von { $total }
imager-setup-internal-run-step = { $step }…
imager-setup-internal-step-shrink = Die Systempartition wird verkleinert
imager-setup-internal-step-create-esp = Die Startpartition des Installationsprogramms wird erstellt
imager-setup-internal-step-create-root = Die Partition des Installationsprogramms wird erstellt
imager-setup-internal-step-write-esp = Die Startdateien des Installationsprogramms werden geschrieben
imager-setup-internal-step-write-root = Das System des Installationsprogramms wird geschrieben
imager-setup-internal-step-verify = Das Geschriebene wird geprüft
imager-setup-internal-step-handoff = Deine Installationsauswahl wird für das Installationsprogramm gespeichert
imager-setup-internal-step-boot-entry = Das Tilo-Installationsprogramm wird zum Startmenü hinzugefügt
imager-setup-internal-step-boot-next = Das Tilo-Installationsprogramm startet beim nächsten Neustart
imager-setup-internal-step-remove-boot-next = Die einmalige Startauswahl wird gelöscht
imager-setup-internal-step-remove-boot-entry = Das Tilo-Installationsprogramm wird aus dem Startmenü entfernt
imager-setup-internal-step-remove-root = Die Partition des Installationsprogramms wird entfernt
imager-setup-internal-step-remove-esp = Die Startpartition des Installationsprogramms wird entfernt
imager-setup-internal-step-restore-shrink = Der Platz geht an die Systempartition zurück
imager-setup-internal-done-title = Bereit zum Neustart
imager-setup-internal-done =
    Das Tilo-Installationsprogramm startet einmal, beim nächsten Neustart dieses Computers. { $host ->
        [windows] Windows startet weiterhin standardmäßig. Verlässt du das Installationsprogramm, startet Windows wie gewohnt.
       *[other] Dein aktuelles System startet weiterhin standardmäßig. Verlässt du das Installationsprogramm, startet es wie gewohnt.
    }
imager-setup-internal-restart-now = Jetzt neu starten
imager-setup-internal-restart-later = Später neu starten
imager-setup-internal-restart-deferred = Sichere deine Arbeit und starte neu, wenn du bereit bist. Das Tilo-Installationsprogramm startet beim nächsten Neustart.
imager-setup-internal-restarting = Wird neu gestartet…
imager-setup-internal-removed = Das Tilo-Installationsprogramm wurde entfernt und sein Platz zurückgegeben.
imager-setup-internal-resume-title = Unvollständiges Tilo-Installationsprogramm
imager-setup-internal-resume = Eine frühere Einrichtung des Tilo-Installationsprogramms auf diesem Computer ist unvollständig oder wurde nicht verwendet. „Fortsetzen“ schließt sie mit dem überprüften Download ab. „Erneut versuchen“ startet das Tilo-Installationsprogramm beim nächsten Neustart. „Entfernen“ löscht das Installationsprogramm und gibt seinen Platz zurück.
imager-setup-internal-resume-retry = Erneut versuchen
imager-setup-internal-resume-remove = Tilo-Installationsprogramm entfernen
imager-setup-internal-failure-plan-stale = Das Laufwerk oder die Starteinstellungen dieses Computers haben sich geändert, nachdem du den Plan geprüft hast. Deshalb hat das Hilfsprogramm angehalten, bevor es etwas geändert hat. Prüfe den neuen Plan und installiere dann erneut.
imager-setup-internal-failure-boot-next-busy = Auf diesem Computer ist bereits eine andere einmalige Startauswahl gesetzt, daher hat die Einrichtung sie nicht ersetzt. Starte einmal neu, um sie zu löschen, und wähle dann „Erneut versuchen“.
imager-setup-internal-failure-digest-mismatch = Die heruntergeladenen Dateien passten nicht zu ihren überprüften Fingerabdrücken, daher wurde nichts geschrieben. Verwirf den Download und lade Tilo erneut herunter.
imager-setup-internal-failure-launch = Das Hilfsprogramm des Installationsprogramms konnte nicht starten: { $detail }
imager-setup-internal-failure-interrupted = Die Einrichtung hat aufgehört, dem Hilfsprogramm zu folgen, bevor es fertig war. Es läuft vielleicht noch; öffne die Einrichtung erneut, um zu sehen, wo es angehalten hat.
imager-setup-internal-failure-generic = Das Hilfsprogramm des Installationsprogramms hat angehalten: { $detail } Jeder abgeschlossene Schritt ist festgehalten, du kannst also fortsetzen oder das Tilo-Installationsprogramm entfernen.
imager-setup-internal-command-failed = Das hat nicht funktioniert: { $detail }
imager-setup-internal-status-checking = Dieser Computer wird geprüft…
imager-setup-internal-status-installing = Das Tilo-Installationsprogramm wird vorbereitet…
imager-setup-internal-status-retrying = Das Tilo-Installationsprogramm wird für den nächsten Neustart eingerichtet…
imager-setup-internal-status-removing = Das Tilo-Installationsprogramm wird entfernt…
imager-setup-internal-status-firmware = Neustart in die Firmware-Einstellungen…
imager-setup-internal-status-fast-startup = Schnellstart wird ausgeschaltet. Stimme der Abfrage für Administratoren zu.
imager-setup-internal-status-size = Größe von Tilo auf { $gib } GiB festgelegt.
imager-setup-internal-download-consuming = Das Tilo-Installationsprogramm wird vorbereitet
imager-setup-internal-download-consuming-detail = Das Hilfsprogramm kopiert den überprüften Download auf dieses Laufwerk.
imager-setup-internal-download-consumed = Download verwendet
imager-setup-internal-download-consumed-detail = Das Hilfsprogramm hat den überprüften Download kopiert. Der Download bleibt auf diesem Computer, bis du ihn verwirfst.
imager-setup-internal-download-kept = Installation angehalten; Download behalten
imager-setup-desktop-heading = Tilo Desktop auf diesem Computer
imager-setup-desktop-install = Tilo Desktop installieren
imager-setup-desktop-cancel = Download abbrechen
imager-setup-desktop-recheck = Erneut prüfen
imager-setup-desktop-command-unavailable = Dieser Vorgang ist im aktuellen Stand der Einrichtung nicht verfügbar.
installer-preview-destination-heading = Willkommen bei Tilo
installer-preview-destination-intro = Wo möchtest du es installieren?
installer-preview-computer-title = Auf diesem Computer
installer-preview-computer-body = Installiere Tilo neben deinem aktuellen System. Du prüfst die Änderungen am Laufwerk vor der Installation.
installer-preview-computer-usb-body = Die direkte Installation ist in diesem Build nicht aktiviert. Du kannst stattdessen ein USB-Installationsmedium erstellen.
installer-native-full-os-body = Installiere neben Windows, ohne USB-Laufwerk. Du prüfst die Änderungen am Laufwerk vor der Installation.
installer-preview-another-title = Auf einem anderen Computer
installer-preview-another-body = Erstelle einen Installationsplan und trage ihn zu diesem Computer. Auf diesem Computer wird nichts geschrieben, solange du es nicht dort oder auf deinen Geräten bestätigst.
installer-preview-migrate-heading = Bring deine Sachen mit
installer-preview-migrate-intro = Melde dich mit einem Ticket von einem deiner Geräte an und öffne dann Migrationen, um auszuwählen, was übernommen wird. Das geht auch nach der Einrichtung.
installer-preview-ticket-label = Ticket
installer-migrate-sign-in = Anmelden
installer-migrate-open = Migrationen öffnen
installer-migrate-signing-in = Anmeldung läuft…
installer-migrate-signed-in = Angemeldet.
installer-migrate-sign-in-failed = Anmeldung fehlgeschlagen.
installer-migrate-sign-in-refused = Anmeldung abgelehnt: { $reason }
installer-migrate-open-failed = Migrationen konnten nicht geöffnet werden.
installer-migrate-busy = Warte, bis die Anmeldung abgeschlossen ist.
installer-migrate-ticket-required = Gib das Ticket von deinem Gerät ein.
installer-preview-target-heading = Auf welchem Computer soll Tilo laufen?
installer-preview-target-intro = Benenne diesen Computer oder importiere den Hardwarebericht, den er erstellt hat. Das Installationsprogramm auf jenem Computer prüft Laufwerke, Start und Schutz, bevor sich etwas ändert.
installer-preview-target-identity-heading = Der andere Computer
installer-preview-target-manufacturer-label = Hersteller
installer-preview-target-model-label = Modell
installer-preview-target-variant-label = Variante oder Platinenversion
installer-preview-target-board-label = Platine (optional)
installer-preview-target-sku-label = SKU (optional)
installer-preview-target-architecture-label = Architektur
installer-preview-target-report-architecture-fixed = Dieser Bericht wurde auf einer anderen Architektur erstellt. Importiere einen Bericht für die Architektur, die du vorbereiten möchtest, oder benenne stattdessen das Modell.
installer-preview-target-payload-missing = Für die Architektur dieses Computers ist kein überprüftes Tilo-Abbild veröffentlicht, daher kann dieser Computer kein Medium schreiben, das dort startet.
installer-preview-target-required = Gib dies an, um den anderen Computer zu bestimmen.
installer-preview-target-import = Hardwarebericht importieren…
installer-preview-target-use-model = Stattdessen ein Modell benennen
installer-preview-target-recheck = Erneut prüfen
installer-preview-target-scan-hint = Um einen Bericht zu importieren, führe die Hardwareprüfung auf dem anderen Computer aus und bring die Datei mit, die sie speichert.
installer-preview-target-report-invalid = Diese Datei ist kein Tilo-Hardwarebericht für einen anderen Computer.
installer-preview-target-report-unreadable = Diese Datei konnte nicht gelesen werden.
installer-preview-target-report-missing-architecture = Der Bericht gibt nicht an, welche Architektur dieser Computer verwendet. Führe die Hardwareprüfung dort mit dieser Version von Tilo erneut aus.
installer-preview-target-report-unsupported-architecture = Für die in diesem Bericht genannte Architektur gibt es keinen Tilo-Build.
installer-preview-target-architecture-unsupported-processor-size = Tilo braucht einen 64-Bit-Prozessor mit mindestens 4 GiB Arbeitsspeicher und kann daher nicht auf einem 32-Bit-Computer laufen. Wähle einen 64-Bit-Computer.
installer-preview-target-architecture-unsupported-byte-order = Tilo speichert und teilt seine Dokumente in einem Little-Endian-Format und kann daher nicht auf diesem Big-Endian-Computer laufen. Wähle einen Little-Endian-Computer.
installer-preview-target-architecture-unsupported-graphics = Für den Prozessor dieses Computers gibt es keinen Grafiktreiber für Vulkan Compute, daher kann er den Desktop nicht darstellen. Wähle einen Computer mit einer Grafikkarte, die Vulkan Compute unterstützt.
installer-preview-target-busy = Wähle zuerst einen Bericht fertig aus, bevor du einen anderen wählst.
installer-preview-target-incomplete = Benenne ein bestimmtes Modell oder importiere einen Bericht vom anderen Computer.
installer-preview-target-incomplete-detail = Nötig sind ein Hersteller und ein Modell sowie eine Variante, Platine oder SKU, oder ein Bericht vom anderen Computer.
installer-preview-target-blocking = Eine für den Desktop nötige Komponente wird auf diesem Computer nicht unterstützt. Behebe das dort, importiere einen neuen Bericht oder benenne das Modell und fahre ohne geprüfte Hardware fort.
installer-preview-target-review-named = { $model }. Seine Laufwerke, sein Start und sein Schutz werden bei der Installation geprüft, bevor sich etwas ändert.
installer-preview-target-review-nameless = Nicht benannt. Seine Laufwerke, sein Start und sein Schutz werden bei der Installation geprüft, bevor sich etwas ändert.
installer-preview-target-review-imported = { $model }: { $supported } unterstützt, { $limited } eingeschränkt, { $unsupported } nicht unterstützt, { $unverified } nicht überprüft.

installer-preview-target-review-imported-unnamed = Hardwarebericht von diesem Computer: { $supported } unterstützt, { $limited } eingeschränkt, { $unsupported } nicht unterstützt, { $unverified } nicht überprüft.
installer-preview-target-review-blocking = Eine für den Desktop nötige Komponente wird auf diesem Computer nicht unterstützt.
installer-preview-target-check-heading = Auf dem anderen Computer geprüft
installer-preview-target-check-boot = Start und Bootvorgang
installer-preview-target-check-boot-body = Wird geprüft, bevor sich auf diesem Computer etwas ändert.
installer-preview-target-check-storage = Speicherplatz und Partitionen
installer-preview-target-check-storage-body = Wird geprüft, bevor sich auf diesem Computer etwas ändert.
installer-preview-target-check-protection = Schutz deiner Daten
installer-preview-target-check-protection-body = Wird geprüft, bevor sich auf diesem Computer etwas ändert.
installer-preview-target-check-note = Nichts hier ändert den anderen Computer. Es wird kein Unterstützungsergebnis je Modell verwendet; sein eigener Hardwarebericht und seine eigene Prüfung liefern die Belege.
installer-preview-target-file-title = Einen Hardwarebericht auswählen
installer-preview-target-file-accept = Importieren
installer-preview-target-file-filter = Hardwareberichte
installer-hardware-checking = Die Hardware dieses Computers wird geprüft …
installer-hardware-this-computer = Dieser Computer
installer-hardware-other-computer = Anderer Computer
installer-hardware-unknown-model = Unbekanntes Modell
installer-hardware-model-with-variant = { $model } ({ $variant })
installer-hardware-variant = Variante
installer-hardware-board = Platine
installer-hardware-sku = SKU
installer-hardware-firmware = Firmware
installer-hardware-running = Läuft mit
installer-hardware-architecture = Architektur
installer-hardware-summary = { $supported } unterstützt, { $limited } eingeschränkt, { $unsupported } nicht unterstützt und { $unverified } nicht geprüft.
installer-hardware-summary-blocking = { $supported } unterstützt, { $limited } eingeschränkt, { $unsupported } nicht unterstützt und { $unverified } nicht geprüft. Eine Komponente, ohne die der Schreibtisch nicht startet, wird nicht unterstützt; die Installation kann erst weitergehen, wenn das geklärt ist.
installer-hardware-summary-declared = Ein Modellname zeigt nicht, ob Tilo auf diesem Computer läuft. Benenne Hersteller, Modell und Variante oder Platine, oder importiere den dort erstellten Hardwarebericht.
installer-hardware-summary-imported-check = Das Installationsprogramm auf diesem Computer prüft Start, Änderungen am Laufwerk und Schutz erneut, bevor es handelt.
installer-hardware-summary-observation = Das laufende System verwendet diese Geräte. Die gewählte Tilo-Version wurde dafür nicht geprüft.
installer-hardware-scope-observation = Im laufenden System beobachtet, nicht mit der Tilo-Version abgeglichen.
installer-hardware-needs-attention = Braucht Aufmerksamkeit
installer-hardware-working = Funktioniert
installer-hardware-not-detected = Nicht erkannt
installer-hardware-state-supported = Unterstützt
installer-hardware-state-limited = Eingeschränkt
installer-hardware-state-unsupported = Nicht unterstützt
installer-hardware-state-unverified = Nicht überprüft
installer-hardware-state-not-detected = Nicht erkannt
installer-hardware-component-graphics = Grafik
installer-hardware-component-display = Bildschirme
installer-hardware-component-wifi = WLAN
installer-hardware-component-ethernet = Ethernet
installer-hardware-component-bluetooth = Bluetooth
installer-hardware-component-audio = Tonausgabe
installer-hardware-component-microphone = Mikrofon
installer-hardware-component-camera = Kamera
installer-hardware-component-fingerprint = Fingerabdrucksensor
installer-hardware-component-power = Energieversorgung
installer-hardware-component-acpi = ACPI
installer-hardware-component-boot = Start und Firmware
installer-hardware-component-storage = Speicher
installer-hardware-component-input = Tastatur und Zeigegeräte
installer-hardware-reason-driver-bound-one = { $device } nutzt im laufenden System { $driver }.
installer-hardware-reason-driver-bound-many = { $count } Geräte nutzen im laufenden System einen Treiber.
installer-hardware-reason-hybrid-graphics = { $device } nutzt { $driver }; es ist mehr als ein Grafikadapter vorhanden.
installer-hardware-reason-driver-missing = Für { $count } Geräte ist im laufenden System kein Treiber gebunden.
installer-hardware-reason-virtual-adapter = Es ist nur { $device } vorhanden; das ist ein virtueller Grafikadapter.
installer-hardware-reason-open-nvidia = { $device } nutzt den quelloffenen Nvidia-Treiber.
installer-hardware-reason-platform-pipeline = { $device } nutzt eine Kamerakette, die sich hier nicht prüfen lässt.
installer-hardware-reason-not-detected = Solche Hardware wurde nicht gefunden.
installer-hardware-reason-display-disconnected = Es ist kein Bildschirm angeschlossen.
installer-hardware-reason-display-inactive = { $device } ist angeschlossen, aber nicht aktiv.
installer-hardware-reason-displays-active = { $internal } eingebaute und { $external } externe Bildschirme sind aktiv.
installer-hardware-reason-no-playback = { $device } hat kein Wiedergabegerät.
installer-hardware-reason-no-capture = { $device } hat kein Aufnahmegerät.
installer-hardware-reason-fingerprint-unverified = { $device } braucht einen leserspezifischen Treiber, der sich hier nicht prüfen lässt.
installer-hardware-reason-suspend-unavailable = Der laufende Kernel bietet keinen Bereitschaftszustand an.
installer-hardware-reason-battery = { $device } ist der Akku.
installer-hardware-reason-battery-capacity = { $capacity } % geladen.
installer-hardware-reason-battery-status = Gemeldeter Zustand: { $status }.
installer-hardware-reason-battery-health = { $health } % der ursprünglichen Kapazität.
installer-hardware-reason-ac-only = Dieser Computer läuft am Stromnetz und hat keinen Akku.
installer-hardware-reason-acpi-tables = { $tables } ACPI-Firmwaretabellen stehen zur Verfügung.
installer-hardware-reason-uefi = Ein UEFI-Start ist möglich.
installer-hardware-reason-secure-boot-on = Secure Boot ist an.
installer-hardware-reason-secure-boot-off = Secure Boot ist aus.
installer-hardware-reason-tpm = { $tpm } ist vorhanden.
installer-hardware-reason-legacy-boot = Dieser Computer ist nicht im UEFI-Modus gestartet, deshalb kann Tilo seinen Starteintrag nicht hinzufügen.
installer-hardware-reason-startup-unknown = Der Startmodus ließ sich nicht lesen.
installer-hardware-reason-input-devices = { $count } Eingabegeräte wurden gefunden.
installer-hardware-reason-no-keyboard = Es wurden Zeigegeräte ohne eingebaute Tastatur gefunden.
installer-hardware-evidence-driver = Treiber { $driver }
installer-hardware-evidence-firmware-version = Firmware { $firmware }
installer-hardware-evidence-vendor = Hersteller { $vendor }
installer-hardware-evidence-product = Produkt { $product }
installer-hardware-evidence-problem = meldet ein Problem
installer-hardware-evidence-display = { $name }: { $state }
installer-hardware-evidence-display-active = aktiv
installer-hardware-evidence-display-connected = verbunden, nicht aktiv
installer-hardware-evidence-display-disconnected = nicht verbunden
installer-hardware-evidence-kernel = laufender Kernel { $version }
installer-hardware-evidence-acpi = { $tables } ACPI-Tabellen
installer-hardware-evidence-suspend = Bereitschaftszustände { $states }
installer-hardware-evidence-release = { $fact }; mit der gewählten Version abgeglichen
installer-hardware-remedy-run-on-destination = Führe die Prüfung vor der Installation auf jenem Computer selbst aus.
installer-hardware-remedy-connect = Schließe einen unterstützten Adapter an, wenn du ihn brauchst.
installer-hardware-remedy-connect-display = Schließe den Bildschirm an und prüfe dann erneut.
installer-hardware-remedy-connect-audio = Schließe ein USB- oder Bluetooth-Headset an, wenn du Ton brauchst.
installer-hardware-remedy-check-driver = Wenn es nach der Installation nicht funktioniert, prüfe dann seinen Treiber.
installer-hardware-remedy-password = Melde dich mit deinem Passwort an; die Anmeldung per Fingerabdruck lässt sich nach der Installation in den Einstellungen prüfen.
installer-hardware-remedy-external-camera = Nutze eine externe USB-Kamera, wenn die eingebaute kein Bild liefert.
installer-hardware-remedy-external-keyboard = Nutze eine externe Tastatur, wenn die eingebaute nach der Installation nicht funktioniert.
installer-hardware-remedy-power-settings = Sichere deine Arbeit, bevor du den Deckel schließt, und prüfe die Energieeinstellungen nach der Installation.
installer-hardware-remedy-display-settings = Schalte den Bildschirm nach der Installation in den Einstellungen ein.
installer-hardware-remedy-uefi = Starte den Installer im UEFI-Modus aus dem Startmenü der Firmware und prüfe dann erneut.
installer-hardware-remedy-power-adapter = Prüfe das Netzteil und prüfe dann erneut.
installer-hardware-source-this-computer = Von diesem Computer gelesen
installer-hardware-source-this-computer-not-other = Von diesem Computer gelesen, nicht vom anderen Computer
installer-hardware-source-imported = Von einem anderen Computer importiert
installer-hardware-source-declared = Modell für einen anderen Computer benannt; Hardware nicht geprüft
installer-preview-desktop-title = Als Desktop
installer-preview-desktop-body = Verwende den Tilo-Desktop mit deinem vorhandenen Linux-System.

installer-preview-migrate-title = Von diesem Computer übernehmen
installer-preview-migrate-body = Mach deine Dokumente, unterstützten Apps und Konten in Tilo zugänglich. Deine Originale bleiben unverändert.
installer-preview-identity-heading = Hast du Tilo schon einmal verwendet?
installer-preview-new-title = Ich verwende es zum ersten Mal
installer-preview-new-body = Erstelle deine Tilo-Identität. Willkommen!
installer-preview-profile-heading = Dein Profil
installer-preview-profile-intro = Menschen, mit denen du zusammenarbeitest, sehen diesen Namen und dieses Bild.
installer-preview-name-label = Dein Name
installer-preview-password-heading = Schütze deine Einrichtung
installer-preview-password-intro = Schützt deine Identität und gespeicherte Anmeldungen während der Installation. Es ist unabhängig vom Anmeldepasswort deines Computers.
installer-preview-password-label = Einrichtungspasswort
installer-preview-confirmation-label = Einrichtungspasswort bestätigen
installer-preview-password-hint = Verwende mindestens 15 Zeichen, am besten mehrere nicht zusammengehörige Wörter.
installer-preview-purpose-heading = Wofür wird dieses Gerät verwendet?
installer-preview-purpose-intro = Dasselbe Tilo OS, mit Voreinstellungen für seine Aufgabe.
installer-preview-purpose-computer-title = Computer
installer-preview-purpose-computer-body = Für den Alltag. Lege in den Energieeinstellungen fest, wann er in den Ruhezustand geht; deine zugelassenen Geräte synchronisieren, solange er wach ist.
installer-preview-purpose-server-title = Immer erreichbarer Server
installer-preview-purpose-server-body = Läuft auch im Leerlauf weiter, damit deine zugelassenen Geräte synchronisieren können. Bildschirmabschaltung, Sperre, Deckel- und Akkuschutz bleiben aktiv. Verwende eine zuverlässige Stromversorgung.
installer-preview-devices-heading = Diesen Computer benennen
installer-preview-devices-intro = Unter diesem Namen erkennen deine anderen Tilo-Geräte diesen Computer.
installer-preview-device-name-label = Computername
installer-preview-device-default = Mein Computer

installer-preview-devices-another-heading = Jenen Computer benennen

installer-preview-devices-another-intro = Unter diesem Namen erkennen deine Tilo-Geräte jenen Computer.
installer-preview-files-title = Dateien und Einstellungen
installer-preview-files-body = Dein ganzer persönlicher Ordner, einschließlich versteckter Dateien, Browserprofile, gespeicherter Zugangsdaten, Flatpak-Apps und Einstellungen.

installer-preview-hide-details = Details ausblenden
installer-preview-storage-heading = Platz für Tilo wählen
installer-preview-storage-intro = Behalte dein aktuelles System und installiere Tilo daneben.
installer-preview-desktop-heading = Tilo als Desktop hinzufügen
installer-preview-desktop-intro = Behalte dein vorhandenes Linux-System.
installer-preview-storage-usb-fallback-heading = Tilo von USB installieren
installer-preview-storage-usb-fallback-intro = Die direkte Installation ist in diesem Build nicht verfügbar. Verwende ein USB-Laufwerk, um Tilo auf diesem Computer zu installieren.

installer-preview-plan-os-heading = Was wird darauf laufen?

installer-preview-os-title = Tilo OS

installer-preview-os-body = Tilo mit Desktop und Apps. Das Anmeldepasswort wird beim ersten Start auf diesem Computer gesetzt.

installer-preview-server-title = Tilo Server

installer-preview-server-body = Kein Desktop und kein Anmeldepasswort. Er verwahrt deine Arbeitsbereiche und wird von deinen anderen Geräten aus verwaltet.

installer-preview-disk-heading = Auf welche Festplatte wird installiert?

installer-preview-disk-intro = Der Plan nennt eine Regel. Die Laufwerke des Computers entscheiden, worauf sie zutrifft; trifft sie auf keines oder mehrere zu, beantwortest du das auf deinen Geräten.

installer-preview-disk-largest-title = Das größte Laufwerk, das nicht das Startlaufwerk ist

installer-preview-disk-largest-body = Wird bei der Installation gegen die Laufwerke des Computers geprüft.

installer-preview-disk-empty-title = Ein Laufwerk ohne Daten

installer-preview-disk-empty-body = Wird bei der Installation gegen die Laufwerke des Computers geprüft.

installer-preview-disk-only-title = Das einzige Laufwerk, das nicht das Startlaufwerk ist

installer-preview-disk-only-body = Wird bei der Installation gegen die Laufwerke des Computers geprüft.

installer-preview-disk-open-title = Bei der Installation fragen

installer-preview-disk-open-body = Du wählst aus den gemeldeten Laufwerken, bevor etwas geschrieben wird.

installer-preview-method-heading = Wie erreicht der Plan den Computer?

installer-preview-method-intro = Das Medium und die Befehle tragen den Plan und einen einmalig gültigen Code. Der Code läuft nach 24 Stunden ab; der Plan kann sich danach noch ändern.

installer-preview-machine-heading = Name und Einstellungen

installer-preview-machine-intro = Lass eine Wahl offen und beantworte sie auf jenem Computer oder auf deinen Geräten.

installer-preview-delivery-heading = Wohin er geht

installer-preview-usb-title = Ein USB-Laufwerk

installer-preview-usb-body = Schreibe den Installer auf ein Laufwerk, das gelöscht werden darf, und starte den Computer davon.

installer-preview-netboot-title = Ein Rettungs- oder Netboot-System

installer-preview-netboot-body = Gib ihm einen Befehl, sobald es läuft; es installiert über das Netzwerk.

installer-preview-alongside-title = Neben seinem Linux

installer-preview-alongside-body = Führe einen Befehl in dem Linux aus, das schon darauf läuft. Dieses System bleibt erhalten und startet zuerst.

installer-preview-name-open-title = Bei der Installation fragen

installer-preview-name-open-body = Du benennst ihn bei der Installation, auf ihm oder auf deinen Geräten.

installer-preview-network-heading = Netzwerk

installer-preview-network-dhcp-title = Automatisch (DHCP)

installer-preview-network-dhcp-body = Er bezieht seine Adressen aus dem Netzwerk, dem er beitritt.

installer-preview-network-open-title = Bei der Installation fragen

installer-preview-network-open-body = Wird auf dem Computer gesetzt, wenn die automatische Konfiguration nicht funktioniert.

installer-preview-unlock-heading = Entsperren nach einem Neustart

installer-preview-unlock-tpm-title = Sein eigener Hardwareschutz

installer-preview-unlock-tpm-body = Das Laufwerk entsperrt sich über den Schutz des Computers; niemand tippt ein Passwort am Gerät ein.

installer-preview-unlock-owner-device-title = Eines deiner Geräte

installer-preview-unlock-owner-device-body = Er bittet ein freigegebenes Gerät von dir ums Entsperren.

installer-preview-unlock-passphrase-title = Eine Passphrase

installer-preview-unlock-passphrase-body = Jemand am Gerät tippt die Passphrase ein.

installer-preview-unlock-open-title = Bei der Installation fragen

installer-preview-unlock-open-body = Du wählst aus den Methoden, die seine Hardware unterstützt.

installer-preview-updates-heading = Updates, denen er folgt

installer-preview-updates-stable-title = Stabile Versionen

installer-preview-updates-stable-body = Er aktualisiert, wenn eine Version getestet und angekündigt ist.

installer-preview-updates-dev-title = Entwicklungsversionen

installer-preview-updates-dev-body = Er aktualisiert mit den neuesten Änderungen, die Fehler enthalten können.

installer-preview-updates-open-title = Bei der Installation fragen

installer-preview-updates-open-body = Du wählst, bevor er beitritt.

installer-preview-hosting-heading = Arbeitsbereiche, die er verwahrt

installer-preview-hosting-none-title = Vorerst keine

installer-preview-hosting-none-body = Du fügst Arbeitsbereiche aus deinem Server-Eintrag hinzu, sobald er installiert ist.

installer-preview-hosting-open-title = Nach der Installation fragen

installer-preview-hosting-open-body = Du wählst aus deinem Server-Eintrag, welche Arbeitsbereiche er verwahrt und ob er nur verschlüsselte Kopien hält oder ihre Arbeit übernimmt.

installer-plan-heading = Der Plan

installer-plan-token = Trage diesen einmalig gültigen Code zum Computer: { $token }

installer-plan-expiry = Der Code gilt noch { $hours } Stunden. Geht das Medium verloren, ist nur dieser Code offengelegt, und er läuft ab.

installer-plan-carry-usb = Schreibe den Installer auf ein Laufwerk, das gelöscht werden darf, und bewahre den Code dabei auf:

installer-plan-carry-netboot = Führe in einem Rettungs- oder Netboot-System auf diesem Computer aus:

installer-plan-carry-alongside = Führe in dem Linux aus, das schon auf diesem Computer läuft:

installer-plan-open-heading = Was der Computer noch fragt

installer-plan-answer-name = Sein Name: wird bei der Installation gefragt.

installer-plan-answer-network = Sein Netzwerk: automatisch oder auf dem Computer gesetzt, wenn das scheitert.

installer-plan-answer-disk = Das Laufwerk, auf das installiert wird: gewählt aus den gemeldeten Laufwerken, bevor etwas geschrieben wird.

installer-plan-answer-disk-drive = Das Laufwerk, auf das installiert wird: { $model }, { $size }.

installer-plan-answer-disk-open = Das Laufwerk bleibt offen: { $reason }

installer-plan-answer-unlock = Entsperren: gewählt aus den Methoden, die seine Hardware unterstützt.

installer-plan-answer-updates = Updates: gewählt, bevor er beitritt.

installer-plan-answer-hosting = Die Arbeitsbereiche, die er verwahrt: gewählt aus deinem Server-Eintrag.

installer-plan-answer-other = Wird bei der Installation beantwortet.

installer-plan-unavailable = Dieser Computer kann keinen Rechner planen. Melde dich mit tilo login auf einem deiner Geräte an und versuche es erneut.

installer-plan-waiting-heading = Computer, die auf die Installation warten

installer-plan-waiting-intro = Diese Pläne haben einen einmalig gültigen Code, der noch nicht verwendet wurde. Jeder öffnet bei der Installation, was er noch fragt.

installer-plan-waiting-summary = { $os }, Code läuft in { $hours } Stunden ab.

installer-plan-waiting-unnamed = Ein Computer ohne Namen

installer-plan-waiting-everything-answered = Jede Wahl ist getroffen; er installiert ohne Nachfragen.

installer-plan-make = Den Plan erstellen

installer-plan-make-and-medium = Den Plan erstellen und ein USB-Laufwerk wählen
installer-preview-review-heading = Deine Einrichtung prüfen
installer-preview-review-intro = Wähle einen Eintrag aus, um ihn zu ändern.
installer-preview-ready-heading = Deine Einrichtungsauswahl ist bereit
installer-preview-ready-intro = Es wurde nichts installiert.
installer-preview-continue = Weiter
installer-preview-back = Zurück
installer-preview-finish = Vorschau beenden
installer-preview-restart = Einen anderen Weg ausprobieren
installer-preview-choose-first = Wähle eine Option, um fortzufahren.
installer-preview-device-required = Gib diesem Gerät einen Namen.
installer-preview-password-short = Verwende mindestens 15 Zeichen.
installer-preview-password-spaces = Entferne Leerzeichen am Anfang oder Ende.
installer-preview-password-common = Verwende mehrere nicht zusammengehörige Wörter.
installer-preview-password-numeric = Verwende mindestens 20 Ziffern oder füge Buchstaben hinzu.
installer-preview-password-repetitive = Vermeide wiederholte Zeichen oder Muster.
installer-preview-password-invalid = Wähle ein anderes Passwort.
installer-preview-password-mismatch = Die Passwörter stimmen nicht überein.
installer-preview-deferred-handoff = Deine Identität, Anmeldungen und Dateien werden auf dem Zielcomputer abgeschlossen, nachdem dort beim ersten Start das Anmeldepasswort festgelegt wurde.

installer-preview-deferred-handoff-server = Deine Identität und Anmeldungen werden auf dem Server abgeschlossen, nachdem er installiert ist. Ein Server hat kein Anmeldepasswort.
installer-preview-identity-busy = Warte, bis die Vorbereitung fertig ist.
installer-preview-invalid-choice = Diese Auswahl ist auf dieser Seite nicht verfügbar.
installer-preview-wrong-page = Diese Aktion ist auf der aktuellen Seite nicht verfügbar.
installer-demo-unavailable = Diese Aktion ist in der Vorführung nicht verfügbar.
installer-demo-notice-title = Nur Vorschau
installer-demo-notice-body = Beispieldaten. Es wird nichts installiert oder verbunden.
installer-preview-at-start = Du bist am Anfang der Einrichtung.
installer-preview-finished = Die Vorschau ist abgeschlossen. Du kannst einen anderen Weg ausprobieren.
installer-preview-storage-checking = Der Speicher dieses Computers wird geprüft…
installer-preview-storage-capacity = Laufwerk mit { $capacity }
installer-preview-storage-size-label = Platz für Tilo
installer-review-allocation = { $size } GiB für Tilo.
installer-review-unallocated = Verwendet nicht zugewiesenen Speicherplatz; die Partition deines aktuellen Systems bleibt unverändert.
installer-review-resize = Die Partition deines aktuellen Systems wird von { $before } auf { $after } verkleinert. Ihre Dateien bleiben, wo sie sind.
installer-review-not-ready = Schließe deine Einrichtungsauswahl ab, bevor du die Installation prüfst.
installer-review-context-pending = Die Installation ist in diesem Fenster nicht bereit. Öffne die Einrichtung erneut, um fortzufahren.
installer-review-storage-pending = Der Speicher ist nicht bereit. Bearbeite „Platz für Tilo“ und prüfe die Kapazität erneut.
installer-review-storage-shortfall = Tilo braucht { $needed }; verfügbar sind { $available }. Bearbeite „Platz für Tilo“, um die Kapazität zu prüfen.
installer-preview-storage-size-range = Tilo braucht mindestens { $min } GiB. Bis zu { $max } GiB sind verfügbar.
installer-preview-storage-existing = Dein aktuelles System
installer-preview-storage-tilo = Tilo
installer-preview-storage-temporary = Vorübergehender Platz für das Installationsprogramm
installer-preview-storage-shrink = Die Partition deines aktuellen Systems wird in der Größe verändert. Ihre Dateien bleiben, wo sie sind.
installer-preview-storage-shortfall = Diese Installation braucht { $needed }; verfügbar sind { $available }. Wähle eine kleinere Größe für Tilo oder installiere per USB auf ein anderes Laufwerk. Wenn du Dateien zuerst auf anderen Speicher verschiebst, prüfe die Kopie, bevor du Platz freigibst. Es wurde nichts geändert.
installer-preview-storage-unreadable = Dieses Laufwerk kann von deinem aktuellen System aus nicht vorbereitet werden. Du kannst stattdessen ein USB-Installationsmedium verwenden.
installer-preview-storage-release-blocked = Die direkte Installation ist in diesem Build nicht verfügbar. Du kannst stattdessen von USB installieren.
installer-preview-storage-unavailable = Die direkte Installation ist nicht verfügbar. Verwende stattdessen ein USB-Installationsmedium.
installer-preview-storage-range-error = Wähle eine Größe im verfügbaren Bereich.
installer-preview-prepare-usb = Ein USB-Installationsmedium erstellen
installer-preview-save = Änderungen speichern
installer-preview-cancel = Abbrechen
installer-accounts-connect = Verbinden
installer-preview-prepare-direct = Installation prüfen
installer-preview-install-profile-invalid = Deine Installationseinstellungen müssen korrigiert werden, bevor du fortfährst.
installer-preview-direct-review-unavailable = Die Installation konnte für diesen Computer nicht vorbereitet werden. Deine Einrichtung bleibt erhalten; du kannst es erneut versuchen oder ein USB-Installationsmedium erstellen.
installer-preview-device-invalid = Wähle einen kürzeren Gerätenamen in einer einzigen Zeile.
installer-preview-storage-show-details = Details zum Laufwerk
installer-desktop-prepare = Tilo Desktop einrichten
installer-desktop-password-intro = Wähle ein Passwort, das deine vorbereitete Tilo-Identität und deine Verbindungen schützt. Das Anmeldepasswort deines Computers bleibt gleich.
installer-desktop-keep-existing = Deine Dateien, Anwendungen und dein Linux-Konto bleiben unverändert. Dein aktueller Desktop bleibt die Standardsitzung.
installer-desktop-unavailable = Die Einrichtung von Tilo Desktop ist auf diesem Computer nicht verfügbar.
installer-desktop-retry = Erneut versuchen
installer-media-capacity-checking = Die Größe des Abbilds wird geprüft, bevor du ein Laufwerk wählst.
installer-media-capacity-not-ready = Die Größe des Installationsabbilds ist noch nicht bekannt. Geh zurück, wähle ein fertiges Abbild und versuche es erneut.
installer-media-capacity-invalid = Der für diese Installation nötige Platz konnte nicht berechnet werden. Geh zurück und prüfe das Abbild und die Auswahl für die Übernahme.
installer-media-capacity-estimate = Plane mindestens { $required } für das Installationsprogramm und alles Ausgewählte ein. Diese Schätzung enthält Platz für Dateisystem-Metadaten.
installer-media-capacity-required = Geschätzter Platzbedarf: { $required }. Dieses Laufwerk hat { $available }. Wähle ein Laufwerk mit genug Platz.
installer-media-drive-too-small = { $contents }. Braucht { $required }: Wähle ein Laufwerk mit mehr Platz.
installer-media-ejected = { $drive } wurde ausgeworfen. Du kannst es jetzt abziehen.
installer-media-eject-failed = Das Laufwerk wurde beschrieben, konnte aber nicht ausgeworfen werden: { $reason } Schließe alles, was es verwendet, und versuche es erneut, oder fahre herunter, bevor du es abziehst.
imager-write-stopping = Wird sicher angehalten… Lass das Laufwerk angeschlossen, bis der Abbruch abgeschlossen ist.
imager-write-exit-unowned = Das Ende des Abbildschreibers kann nicht überprüft werden.
imager-write-worker-stopped = Das Schreiben auf das Laufwerk wurde unerwartet beendet. Schließe das Laufwerk wieder an, prüfe den Vorgang erneut und versuche es noch einmal.
imager-write-worker-start-failed = Der Abbildschreiber konnte nicht starten: { $error }
imager-write-cancelled = Vor dem Ende angehalten. Das Laufwerk ist womöglich unvollständig. Prüfe den Vorgang und versuche es erneut, bevor du es verwendest.
installer-review-setup = Deine Einrichtung
installer-review-device-body = Dieses Gerät in Tilo
installer-review-usb-fallback = Die direkte Installation ist in diesem Build nicht aktiviert. Erstelle ein USB-Installationsmedium und starte diesen Computer dann davon neu.
installer-review-usb-choice-required = Wähle „Ein USB-Installationsmedium erstellen“, um auf diesem Computer fortzufahren.
installer-review-installation-unavailable = Die Installation ist hier nicht verfügbar. Deine Einrichtungsauswahl bleibt erhalten.
installer-storage-check-capacity = Verfügbaren Platz prüfen…
installer-storage-btrfs-resize = Dein aktuelles System behält { $after } von seinen { $before }. Mindestens { $reserve } bleiben über dem Platz, den das Dateisystem braucht. Deine Dateien bleiben, wo sie sind.
installer-guided-host-unavailable = Bereite Tilo auf einem Linux-Computer vor. Die Einrichtung der Installation ist auf diesem Computer noch nicht verfügbar.
installer-preview-current-title = Die Identität dieses Computers verwenden
installer-preview-current-body = Fahre mit der Tilo-Identität fort, die auf diesem Computer bereits verwendet wird.

imager-folder-cloud-unavailable = Windows markiert dieses Element als nur online, daher wird es weder gezählt noch kopiert und nicht heruntergeladen. Mach es in Windows verfügbar und prüfe dann erneut; das kann mehr Speicherplatz brauchen.
imager-folder-reparse-unsupported = Dieses Element ist ein Windows-Analysepunkt, etwa eine Verknüpfung, eine Verzweigung oder ein von der Cloud verwaltetes Element. Dieses Kopierprogramm unterstützt es nicht, auch wenn sein Inhalt lokal ist. Sein Inhalt wird weder gezählt noch kopiert.
installer-hardware-reason-starts-here = Dieser Computer startet ein für { $architecture } gebautes Tilo.
installer-hardware-reason-starts-board = Es wurde für die Platine { $board } gebaut.
installer-hardware-reason-starts-devicetree = Es startet aus seinem eigenen Device Tree, { $devicetree }.
installer-hardware-reason-other-architecture = Das Tilo hier ist für { $architecture } gebaut, dieser Computer ist aber { $running }.
installer-hardware-reason-board-without-devicetree = Die Platine { $board } startet { $architecture } aus einem Device Tree, und dieses Image enthält keinen dafür.
installer-hardware-evidence-architecture = Architektur { $architecture }
installer-hardware-evidence-virtualization = Virtualisierung { $names }
installer-hardware-evidence-board = Platine { $board }
installer-hardware-evidence-devicetree = Device Tree { $devicetree }
installer-welcome-title = Willkommen bei Tilo
installer-welcome-note = Vor der Installation prüfen wir deine Auswahl. Auf deinen Laufwerken ändert sich nichts, bis du bestätigst.
installer-keyboard-note = Wähle die Belegung deiner Tastatur. Du kannst sie später ändern.
installer-account-name = Dein Name (optional)
installer-account-name-note = Verwende den Namen, den andere sehen sollen. Dein Benutzername zur Anmeldung kann anders sein.
installer-account-username = Benutzername
installer-account-username-note = Wird zur Anmeldung und für dein persönliches Verzeichnis verwendet.
installer-account-password = Dein Passwort
installer-account-password-note = Es schützt deine Dateien. Verwende mindestens 15 Zeichen, zum Beispiel mehrere nicht zusammengehörige Wörter.
installer-account-password-confirm = Gib dein Passwort erneut ein
installer-account-pin = Deine Start-PIN
installer-account-pin-note = Verwende mindestens 6 Ziffern. Dein Computer begrenzt falsche Versuche. Bewahre den Wiederherstellungsschlüssel für Hardwareänderungen auf.
installer-account-pin-confirm = Gib deine Start-PIN erneut ein
installer-storage-no-drives = Noch sind keine Laufwerke verfügbar. Prüfe, ob das Laufwerk angeschlossen ist, und aktualisiere die Laufwerksliste. Es wurde nichts geändert.
installer-storage-erase-warning = Damit wird alles auf dem ausgewählten Laufwerk ersetzt, einschließlich anderer Betriebssysteme und Dateien. Sichere alles, was du behalten möchtest. Nichts wird gelöscht, bis du die Installation geprüft und bestätigt hast.
installer-storage-keep-guidance = Behalte dein anderes Betriebssystem und seine Dateien. Tilo braucht mindestens 35 GiB nicht zugewiesenen Speicherplatz oder geeignete leere Partitionen. Vorhandene Partitionen werden niemals verkleinert oder verschoben. Falls der Platz nicht reicht, kannst du sicher zurückgehen und in deinem anderen Betriebssystem Platz schaffen.
installer-media-title = Mehr Video- und Audioformate abspielen
installer-media-note = Wird von den Herausgebern der Formate auf diesen Computer geladen, sobald er online ist. Du kannst es in den Einstellungen ausschalten.
installer-media-install = Installieren
installer-media-skip = Nicht jetzt
installer-media-chosen = Ausgewählt: wird installiert, sobald das neue System online ist
installer-media-undo = Rückgängig
installer-storage-windows-preparation = Auf diesem Laufwerk wurde Windows- oder NTFS-Speicher gefunden. Starte vor dem Fortfahren Windows, sichere den BitLocker-Wiederherstellungsschlüssel und schalte den Schnellstart aus. Falls du noch nicht zugewiesenen Speicherplatz brauchst, verkleinere Windows mit der Datenträgerverwaltung. Tilo kann diese Änderungen an Windows nicht vornehmen und verändert die Größe von NTFS nicht.
installer-storage-linux-ready = Linux behalten: Der nicht zugewiesene Speicherplatz nimmt Tilo auf, vorhandene Partitionen bleiben unverändert.
installer-storage-use-capacity = N GiB für Tilo verwenden
installer-storage-allocation-summary = { $capacity } für Tilo verwenden; { $remaining } bleibt ungenutzt. { $details }
installer-storage-alongside-capacity = { $root } verschlüsselter Speicher für Tilo, belegt { $total } nicht zugewiesenen Speicherplatz einschließlich seiner Startpartitionen.
installer-storage-linux-no-space = Es wurde kein sicherer Platz neben dem vorhandenen System gefunden. { $preparation } Es wurde nichts gelöscht; du kannst ohne Installation zurückgehen.
installer-storage-linux-preparation = Sichere deine Dateien. Freier Platz innerhalb von ext4 oder Btrfs ist kein nicht zugewiesener Speicherplatz: Schaffe mit den unterstützten Offline-Partitionierungswerkzeugen deiner Distribution von einem Rettungsmedium aus 35 GiB zusammenhängenden, nicht zugewiesenen Speicherplatz und aktualisiere dann die Laufwerksliste. Dieses Installationsprogramm verkleinert vorhandene Dateisysteme, LVM- oder LUKS-Container nicht automatisch. Dieses Medium ist kein vollständiger Live-Desktop.
installer-storage-boot-guidance = Fehlt dein anderes System im Startmenü von Tilo, verwende die Startauswahl der Firmware deines Computers. Vorhandene Starteinträge und die Standardauswahl bleiben erhalten.
installer-storage-files-guidance = Vorhandene Linux-Partitionen werden in Tilo nicht automatisch eingebunden. Für den Import ausgewählte Dateien sind davon unabhängig. Richte Nextcloud nach der Installation separat ein; nutze keinen beschreibbaren Synchronisierungsordner gemeinsam mit einem anderen System.
installer-hardware-unavailable = Dieser Computer sagt zu wenig über sein Modell aus, um den Kompatibilitätsbericht zu erstellen; deshalb wurde nichts an ihm geprüft.
installer-hardware-blocks-install = Eine Komponente, ohne die der Schreibtisch nicht startet, wird auf diesem Computer nicht unterstützt; deshalb endet die Installation an diesem Schritt. Es wurde nichts geändert.
installer-keyboard-layout = Tastaturbelegung wählen
installer-language-locale = Sprache und Region wählen
installer-time-zone = Zeitzone wählen

## Fenster- und Barrierefreiheits-Texte
installer-state-stopped = Gestoppt
installer-state-verified = Geprüft
installer-export-close = Zum Schließen Escape drücken
installer-export-profile-note = Genau dieses eingebettete Profil; Geheimnisse werden auf dem Gerät gesammelt.
installer-recovery-key-copy-note = Kopiere ihn in einen Passwortmanager oder eine verschlüsselte Notiz auf einem anderen Laufwerk. Der Installer leert seine Zwischenablage nach 5 Minuten, beim Neustart oder beim Schließen.
installer-recovery-key-copied = Kopiert. Füge ihn vor dem Neustart in einen Passwortmanager oder eine verschlüsselte Notiz auf einem anderen Laufwerk ein.
installer-recovery-key-heading = Wiederherstellungsschlüssel
installer-installing-note = Läuft. Das kann ein paar Minuten dauern; nicht ausschalten.
installer-installing = Tilo wird installiert…
installer-details = Details
installer-failed-note = Der Installer hat gestoppt, bevor er ein startfähiges System bestätigen konnte. Auf dem Ziel kann bereits eine teilweise verschlüsselte Installation liegen. Namen, Labels oder Metadaten auf der Platte machen das Löschen nicht sicher. Speichere unten den Wiederherstellungsschlüssel, falls einer erzeugt wurde, prüfe dann den Speicher oder nutze Ctrl+Alt+F2 für die Wiederherstellungswerkzeuge.
installer-failed = Installation fehlgeschlagen
installer-boot-shortcuts = Tilo kann Menüeinträge für eindeutige Loader auf der gemeinsamen EFI-Partition anlegen. Wenn Linux nicht aufgeführt ist, nutze die Startauswahl der Firmware. Loader auf anderen EFI-Partitionen werden dort möglicherweise nicht aufgeführt.
installer-boot-order = Um Tilo oder dein vorhandenes System zu wählen, öffne die Startauswahl der Firmware dieses Rechners. Der Installer hat deren bisherige Vorgabe beibehalten.
installer-installed = Tilo ist installiert
installer-recovery-key-note = Ein Wiederherstellungsschlüssel wird während der Installation angezeigt. Er ist deine einzige Sicherung, falls das TPM zurückgesetzt wird oder ausfällt. Bewahre ihn sicher auf.
installer-image-note = Diese Installation nutzt das Tilo-Image auf diesem Medium; sie lädt kein Update herunter. Künftige Systemupdates kommen aus dem signaturpflichtigen Tilo-Repository und werden nach dem ersten Start automatisch geprüft, sobald der Rechner online ist.
installer-review = Prüfe deine Auswahl
installer-filesystem-recommended = Für Tilo empfohlen
installer-filesystem = Dateisystem
installer-time-zone-pick = Wähle aus der Liste oder tippe zum Filtern (IANA-Name, etwa Europe/Amsterdam oder UTC).
installer-language-note = Das legt Sprache, Formatierung und regionale Gepflogenheiten nach der Installation fest.
installer-a11y-language-locale = Sprache & Region
installer-hostname-note = Der Name, unter dem dein Rechner im Netzwerk erscheint.
installer-hostname = Rechnername
installer-security-key-option = Optional kannst du einen FIDO2-Sicherheitsschlüssel (etwa einen YubiKey) als weiteren Weg zum Entsperren hinterlegen. Stecke ihn vor der Installation an.
installer-security-key = Sicherheitsschlüssel
installer-encryption-always = Immer an, mit LUKS2
installer-disk-encryption = Festplattenverschlüsselung
installer-options-heading = Installationsoptionen
installer-recovery-tools = Brauchst du Dateien von diesem Rechner, oder willst du eine Nebeneinander-Installation von Tilo entfernen? Drücke Ctrl+Alt+F2 für die Wiederherstellungswerkzeuge; Ctrl+Alt+F1 führt hierher zurück.
installer-account-create = Konto anlegen
installer-account-secure = Sichere dein Konto
installer-account-server-title = Schütze die Festplatte dieses Servers
installer-account-server-note = Auf diesem Rechner meldet sich niemand an. Er wird beim ersten Start Teil deiner Identität, und du verwaltest ihn von deinen anderen Geräten aus.
installer-account-server-secret = Entsperrgeheimnis der Festplatte
installer-account-server-secret-note = Wird bei jedem Neustart des Servers gebraucht. Bewahre den Wiederherstellungsschlüssel sicher auf.
installer-account-server-secret-confirm = Gib das Entsperrgeheimnis erneut ein
installer-region-measured = Die Position wird vom Anfang der gewählten Festplatte gemessen. Vorhandene Partitionen zwischen freien Bereichen bleiben unberührt.
installer-region-choose = Wähle den genauen freien Bereich für Tilo
installer-disk-protected = Diese Festplatte ist durch Hardware oder Richtlinie schreibgeschützt.
installer-install-tilo = Tilo installieren
installer-wifi-saved = Hier hergestellte WLAN-Verbindungen werden für den ersten Start gespeichert. Du kannst auch ohne eine fortfahren.
installer-network-privileged = Aus Sicherheitsgründen öffnet der privilegierte Live-Installer keine Webinhalte aus einem Portal. Fahre offline fort und öffne diese Adresse nach dem ersten Start.
installer-wifi-password = WLAN-Passwort
installer-network-optional = Optional. Tilo installiert von diesem Medium und braucht kein Internet.
installer-network-connect = Mit einem Netzwerk verbinden

## Fakten, Eigenschaftsnamen und Bereichstitel
imager-selected-image = Ausgewähltes Image: { $name }
imager-section-recent = Zuletzt verwendet
imager-section-releases = Tilo-Versionen
imager-a11y-erase-drive = { $drive } löschen und beschreiben
installer-property-network = Netzwerk
installer-property-name = Name
installer-property-username = Benutzername
installer-property-formats = Weitere Video- und Audioformate
installer-property-keyboard = Tastaturbelegung
installer-property-timezone = Zeitzone
installer-property-hostname = Rechnername
installer-property-unlocks-with = Entsperrt mit
installer-property-encryption = Verschlüsselung
installer-property-filesystem = Dateisystem
installer-property-existing-files = Vorhandene Dateien
installer-property-boot-choice = Startauswahl
installer-property-choose-os = Betriebssystem wählen
installer-property-tilo-efi = Tilo-EFI-Partition
installer-property-tilo-root = Tilo-Systempartition
installer-choose-os-restart = Der nächste Neustart startet einmalig Tilo; { $host } bleibt die Vorgabe der Firmware
installer-property-installer-space = Installer-Bereich
installer-disk-holds-installer = Hier liegen außerdem der laufende Installer und dein vorhandenes { $host }-System. { $host } bleibt unverändert.
installer-property-this-disk = Dieser Datenträger
installer-property-existing-partitions = Vorhandene Partitionen
installer-property-storage = Tilo-Speicher
installer-property-method = Installationsart
installer-property-disk = Datenträger
installer-type-to-confirm = Gib { $expected } ein, um { $operation } zu bestätigen
installer-install-alongside = Wird neben { $host } in dem von Tilo Imager vorbereiteten Bereich installiert. Auf diesem Datenträger liegt auch der laufende Installer; { $host } und seine Dateien bleiben unverändert.
installer-wifi-unavailable = WLAN-Steuerung nicht verfügbar: { $error }
installer-network-signin-unavailable = Eine Anmeldeseite wurde gefunden, lässt sich aber nicht öffnen: { $error }
installer-network-signin-address = Anmeldeadresse im Netzwerk: { $uri }
imager-section-before-installing = Vor der Installation
imager-section-full-download = Vollständiger Tilo-Download
imager-section-desktop = Tilo Desktop

imager-section-selected-image = Ausgewähltes Image
installer-install-unavailable = { $message }. Die Installation ist nicht möglich.
installer-operation-erase = das vollständige Löschen des Datenträgers
installer-operation-guided-plan = den geführten Partitionsplan im freien Speicher
installer-operation-tilo-format = das Formatieren der Tilo-Start- und Systempartition

## Was das Fenster beim Schreiben und Installieren zeigt
imager-filesystem-help = Für Tilo wird Btrfs empfohlen.
imager-drive-empty = Leeres Laufwerk
imager-drive-partitions = { $count ->
    [one] 1 Partition
   *[other] { $count } Partitionen
}
imager-drive-partitions-held = { $count ->
    [one] 1 Partition
   *[other] { $count } Partitionen
} ({ $filesystems })

imager-write = Schreiben
imager-write-driving = Ausgewähltes Laufwerk löschen und beschreiben
imager-write-cancel = Schreiben abbrechen
imager-write-cancelling = Wird angehalten…
imager-eject = Auswerfen
imager-start-over = Von vorn beginnen
imager-review-and-retry = Prüfen und erneut versuchen
imager-browse = Durchsuchen…
imager-release-cancel = Download abbrechen
imager-preview-embedded = Eingebettete Einrichtung ansehen
imager-release-progress = { $stage }, { $percent } %
imager-release-channel = { $target }, Kanal { $channel }
imager-release-channel-downloads-first = { $target }, Kanal { $channel }, Downloads zuerst
imager-writing-verify-now = Das Laufwerk wird zurückgelesen und jedes Byte geprüft.
imager-writing-verify-next = Danach wird das Laufwerk zurückgelesen und geprüft.
imager-writing-from-to = { $source } nach { $target }. Lass das Laufwerk angeschlossen, bis der Vorgang fertig ist.
imager-storage-device = Speichergerät mit { $size }
imager-installer-title = Tilo-Installation
imager-installer-guidance = Hinweise zur Installation
imager-installer-continue = Weiter
imager-installer-continue-offline = Ohne Netz weiter
imager-installer-next = Weiter
imager-installer-preflight-failed = Die Prüfung des vorbereiteten Speichers ließ sich nicht starten: { $detail }
imager-installer-connecting = Verbindung mit { $network }…
imager-installer-checking-networks = Netzwerke werden gesucht…

# Was die Schaltflächen der Einrichtung sagen, eine Meldung je Schaltfläche; die
# Seiten, in denen die Fenster des Imagers stehen; und die Worte des
# Abbild-Assistenten und der Download-Anzeige.
installer-step-keyboard = Tastatur
installer-step-network = Netzwerk
installer-step-disk = Datenträger
installer-step-account = Konto
installer-step-options = Optionen
installer-step-media = Medien
installer-step-review = Prüfen
installer-step-install = Installieren
installer-action-power-off = Ausschalten
installer-action-reboot = Jetzt neu starten
installer-action-return-to-host = Zurück zu { $host }
installer-action-refresh-networks = Netzwerke neu suchen
installer-action-refresh-disks = Datenträger neu suchen
installer-action-review-storage = Speicher prüfen
installer-action-recovery-promise = Ich bewahre meinen Wiederherstellungsschlüssel auf
installer-action-copy-recovery-key = Wiederherstellungsschlüssel kopieren
installer-action-copy-recovery-key-again = Wiederherstellungsschlüssel erneut kopieren
installer-action-recovery-saved = Ich habe diesen Schlüssel außerhalb dieses Computers gespeichert
installer-action-begin = Installation starten
installer-region-use = { $size } frei ab { $start } verwenden
installer-region-selected = Ausgewählt: { $size } frei ab { $start }
imager-page-choose-image = Abbild auswählen
imager-page-choose-drive = Laufwerk auswählen
imager-page-prepare-setup = Einrichtung vorbereiten
imager-page-review-install = Prüfen und installieren
imager-page-review-write = Prüfen und schreiben
imager-drive-unnamed = dieses Laufwerk
imager-confirm-system-disk = Das ist dein Systemdatenträger
imager-confirm-internal-disk = Alles auf dem internen Datenträger { $device } wird gelöscht
imager-confirm-disk = Alles auf { $device } wird gelöscht
imager-confirm-system-consequence = Alles darauf wird gelöscht, auch das laufende System. Das lässt sich nicht rückgängig machen.
imager-confirm-consequence = Das lässt sich nicht rückgängig machen.
imager-writer-unavailable = Dieser Computer kann noch keine Laufwerke beschreiben.
imager-image-unreadable = Diese Datei ist kein lesbares Datenträgerabbild.
imager-write-done = { $name } ist geschrieben und geprüft.
imager-install-done = { $name } ist installiert und geprüft.
imager-write-failed = { $name } konnte nicht geschrieben werden. Schließe das Laufwerk wieder an und prüfe dann erneut.
imager-install-failed = { $name } konnte nicht installiert werden. Schließe das Laufwerk wieder an und prüfe dann erneut.
imager-drop-title = Datenträgerabbild auswählen
imager-drop-message = ISO-, IMG- oder RAW-Datei hier ablegen
imager-empty-drives = USB-Laufwerk oder SD-Karte einlegen
imager-no-releases = Auf diesem Computer sind keine Tilo-Versionen gespeichert.
imager-plain-body = Einrichtungsoptionen gibt es nur für Tilo-Abbilder. Dieses Abbild wird unverändert geschrieben.
imager-setup-internal-download-title = Vollständiger Tilo-Download
imager-download-title-downloading = Tilo wird heruntergeladen
imager-download-title-ready = Download bereit
imager-download-title-paused = Download pausiert
imager-download-title-cancelled = Download abgebrochen
imager-download-title-failed = Download fehlgeschlagen
imager-download-title-released = Für später behalten
imager-download-title-discarded = Unfertige Download-Daten verworfen

imager-network-connected = Verbunden
imager-network-secured = Gesichert
imager-network-open = Offen
imager-installer-select-disk = Wähle ein Laufwerk, um zu sehen, wie Tilo es mitbenutzen kann.
imager-installer-format-prepared = Nur die vorbereiteten Tilo-Partitionen für Start und Wurzel formatieren; die vorhandene EFI-Partition und die Partitionen anderer Systeme bleiben, wie sie sind.
imager-installer-no-disk = Kein Laufwerk gewählt
imager-installer-erase = Damit werden { $device } und alle Daten darauf gelöscht.
imager-installer-complete-host = Die Installation ist fertig. Starte neu, um Tilo zu beginnen; { $host } bleibt in der Startreihenfolge der Firmware voreingestellt. Der vorübergehend belegte Installationsplatz geht an Tilo zurück, sobald du dich zum ersten Mal anmeldest. Signierte Systemaktualisierungen werden nach dem ersten Start online geprüft; bei dieser Installation wurde keine Aktualisierung geladen.
imager-installer-complete = Die Installation ist fertig. Nimm den Installationsdatenträger heraus und starte neu. Signierte Systemaktualisierungen werden nach dem ersten Start online geprüft; bei dieser Installation wurde keine Aktualisierung geladen.
imager-installer-refresh-none = Die Laufwerksliste ist neu gelesen; es wurden keine installierbaren Laufwerke gefunden.
imager-installer-refresh-review = Die Laufwerksliste ist neu gelesen. Prüfe das Ziel, bevor du fortfährst.
imager-installer-refresh-select = Die Laufwerksliste ist neu gelesen. Wähle das Ziel erneut.
imager-desktop-checking = Dieser Rechner und der signierte Tilo-Desktop-Katalog werden geprüft…
imager-desktop-unavailable = Tilo Desktop ist für { $host } noch nicht verfügbar.
imager-desktop-already-installed = Tilo Desktop { $version } ist auf { $host } schon installiert. Fahre fort, um die Tilo-Sitzung zu prüfen; bestätige die Prüfung, wenn du gefragt wirst.
imager-desktop-ready = Tilo Desktop { $version } steht für { $host } bereit. Fahre fort, um es herunterzuladen; bestätige die Installation, wenn du gefragt wirst.
imager-desktop-downloading = Tilo Desktop { $version } wird für { $host } geladen und geprüft. Abbrechen stoppt, bevor etwas installiert wird.
imager-desktop-authorizing = Warte auf deine Zustimmung, Tilo Desktop { $version } auf { $host } zu installieren.
imager-desktop-preparing = Tilo Desktop { $version } wird für { $host } vor der Installation noch einmal geprüft.
imager-desktop-installing = Tilo Desktop { $version } wird auf { $host } installiert. Das Schließen der Einrichtung stoppt die Installation nicht.
imager-desktop-verifying = Die installierte Tilo-Sitzung auf { $host } wird geprüft.
imager-desktop-installed = Tilo Desktop { $version } ist auf { $host } installiert und geprüft. Sichere deine Arbeit und melde dich ab, wenn du so weit bist, und wähle dann Tilo am Anmeldebildschirm. Deine jetzige Sitzung bleibt voreingestellt.
imager-desktop-download-cancelled = Der Download wurde abgebrochen.
imager-desktop-not-approved = Die Installation wurde nicht bestätigt.
imager-desktop-needs-admin = Die Installation braucht die Zustimmung einer Administratorin oder eines Administrators.
imager-desktop-stopped-early = Die Prüfung dieses Rechners wurde vorzeitig beendet.
imager-desktop-not-ready = Dieser Rechner ist für Tilo Desktop nicht bereit.
imager-desktop-download-failed = Der Download ist fehlgeschlagen.
imager-desktop-install-failed = Die Installation ist fehlgeschlagen.
imager-desktop-failed = { $what } { $detail }
imager-desktop-failed-retry = { $what } { $detail } Deine Ordnerauswahl bleibt erhalten; prüfe erneut, um es noch einmal zu versuchen.
imager-retained-windows = Windows bleibt installiert und bleibt der Startstandard.
imager-retained-linux = Deine Linux-Installation bleibt, wie sie ist, und bleibt die voreingestellte Sitzung.
imager-retained-other = Dein jetziges System bleibt installiert und bleibt der Standard.
imager-desktop-unsupported-component = Eine für den Desktop nötige Komponente wird nicht unterstützt. Behebe das unten und prüfe dann erneut.

## What the destination step and the copy say
installer-battery-power = Der Akku steht bei { $percent } %. Schließe diesen Computer vor der Installation an das Stromnetz an.
installer-fs-btrfs = Snapshots, Kompression und Zurückrollen.
installer-fs-ext4 = Einfach und bewährt, ohne Snapshots.
installer-fs-xfs = Hoher Durchsatz für große Dateien, ohne Snapshots.
installer-encrypt-verified = Tilo verschlüsselt die Festplatte mit LUKS2 und bindet das Entsperren an den Secure-Boot-Zustand dieses Computers und die freigegebene Tilo-Startmessung. Du nutzt eine kurze Start-PIN; ein Wiederherstellungsschlüssel wird einmal während der Installation angezeigt.
installer-encrypt-target-unverified = Tilo verschlüsselt die Festplatte immer mit LUKS2. Für diese Installation ist kein verifizierter Start möglich, nutze daher eine starke Startpassphrase. Eine kurze PIN ist nur sicher, wenn das TPM die vollständige Startkette prüfen kann.
installer-encrypt-lockdown-off = Tilo verschlüsselt die Festplatte immer mit LUKS2. Secure Boot ist aktiv, der Kernel-Lockdown aber nicht, daher kann Tilo diesen Start nicht als verifiziert behandeln und nutzt stattdessen eine starke Festplattenpassphrase.
installer-encrypt-secure-boot-off = Tilo verschlüsselt die Festplatte immer mit LUKS2. Kurze PINs über TPM2 setzen einen verifizierten Start voraus, aber Secure Boot ist nicht aktiv, daher nutzt diese Installation eine starke Festplattenpassphrase.
installer-encrypt-no-tpm = Tilo verschlüsselt die Festplatte immer mit LUKS2. Dieser Computer hat keinen TPM2-Chip, daher gibst du beim Start eine Festplattenpassphrase ein. Ein Wiederherstellungsschlüssel wird einmal während der Installation als Sicherung angezeigt.
installer-filesystem-cannot-boot = Mit { $filesystem } startet dieses Image nicht; wähle ein unterstütztes Dateisystem.
installer-image-official = Offizielle Tilo-Version, geprüft
installer-image-local = Lokales Image, unverändert geschrieben

## The wizard's reasons, items and stations, and the writer's progress
installer-security-key-off = Kein Sicherheitsschlüssel
installer-security-key-enroll = Sicherheitsschlüssel hinterlegen
installer-method-erase = Datenträger löschen
installer-method-alongside = Daneben installieren
installer-alongside-change-guided = Drei neue Tilo-Partitionen (EFI, Boot und verschlüsselter Root) werden ausschließlich in dem geprüften unbelegten Bereich auf { $device } erstellt und formatiert. Bestehende Betriebssystem-Partitionen bleiben erhalten.
installer-alongside-change-prepared = Nur die vorbereiteten Tilo-Boot- und Root-Partitionen auf { $device } werden formatiert. Die vorhandene EFI-Partition und die Partitionen anderer Betriebssysteme bleiben erhalten.
installer-alongside-change-note = { $change } Änderungen an Firmware und Secure Boot können weiterhin eine BitLocker-Wiederherstellungsabfrage unter Windows auslösen; sichere diesen Wiederherstellungsschlüssel, bevor du fortfährst.
installer-state-idle = Wartet
installer-state-running = Wird installiert
installer-state-done = Installiert
installer-state-failed = Fehlgeschlagen
installer-blocked-other-step = Das gehört zum Schritt { $wanted }; das Installationsprogramm steht bei { $current }. Mit Weiter oder Zurück kommst du dorthin.
installer-blocked-password-hash = Ein Passwort-Hash wird nur von einer ausdrücklich unbeaufsichtigten Installation angenommen.
installer-blocked-next-review = Die Prüfung endet mit dem Start der Installation, nicht mit Weiter.
installer-blocked-next-install = Die Installation läuft; es gibt keinen nächsten Schritt.
installer-blocked-back-install = Die Installation läuft; das Installationsprogramm kann nicht zurück.
installer-blocked-back-first = Das ist der erste Schritt.
installer-blocked-not-staged = Dieses Installationsprogramm wurde nicht neben einem vorhandenen System abgelegt.
installer-blocked-return-running = Die Installation läuft; warte, bis sie fertig ist oder fehlschlägt.
installer-blocked-return-done = Tilo ist installiert. Starte diesen Computer neu, um es zu starten.
installer-blocked-network-busy = Es läuft bereits ein Netzwerkvorgang.
installer-blocked-no-networks = Es sind keine WLAN-Netze sichtbar; aktualisiere die Liste.
installer-blocked-no-network-choice = Wähle zuerst ein WLAN-Netz.
installer-blocked-network-connected = Dieses WLAN-Netz ist bereits verbunden.
installer-blocked-no-recovery-key = Es wurde noch kein Wiederherstellungsschlüssel erzeugt.
installer-blocked-power-not-finished = Ausschalten und Neustarten werden angeboten, sobald die Installation fertig ist.
installer-blocked-recovery-unsaved-reboot = Sichere den Wiederherstellungsschlüssel außerhalb dieses Computers und bestätige das, bevor du neu startest.
installer-blocked-recovery-unsaved-power-off = Sichere den Wiederherstellungsschlüssel außerhalb dieses Computers und bestätige das, bevor du ausschaltest.
installer-blocked-installer-disk = Der laufende Installationsdatenträger kann nicht sein eigenes Ziel sein.
installer-blocked-read-only = Dieser Datenträger ist durch Hardware oder Richtlinie schreibgeschützt.
installer-blocked-begin-no-target = Wähle einen Datenträger (und bei „Daneben“ einen Bereich), bevor du installierst.
installer-blocked-begin-recovery = Bestätige zuerst, dass du deinen Wiederherstellungsschlüssel sicherst.
installer-item-region = { $size } frei ab { $start }
installer-item-storage = Installationsziel
installer-item-storage-erase = Den ausgewählten Datenträger löschen
installer-item-review = Prüfen und installieren
installer-item-review-no-target = Wähle zuerst ein Ziel.
installer-review-erase = Das löscht { $device } mit allen Daten darauf und installiert dann Tilo. Unwiderruflich: Gib den Gerätenamen als Bestätigung ein.
installer-review-alongside = { $impact } { $boot } { $files } Unwiderruflich: Gib den Bestätigungssatz ein.
installer-detail-step = Schritt { $number } von { $count }: { $step }
installer-status-type-confirmation = Gib { $phrase } genau so ein, um das Formatieren zu bestätigen.
imager-phase-verifying = Wird geprüft
imager-phase-installing = Wird installiert
imager-phase-writing = Wird geschrieben
imager-writing-eta = noch etwa { $time }
imager-step-source = Quelle
imager-step-drive = Laufwerk
imager-step-customize = Anpassen
imager-step-confirm = Prüfen
imager-step-write = Schreiben
imager-step-done = Fertig

## What the setup says while it works
imager-setup-status-choose-experience = Wähle, wie Tilo eingerichtet werden soll.
imager-setup-status-rechecking-before-download = Dieser Computer wird vor dem Download noch einmal geprüft…
imager-setup-status-review-last = Die Prüfung ist der letzte Schritt der Einrichtung.
imager-setup-status-cancelling-package = Der Download wird abgebrochen…
imager-setup-status-rechecking-before-retry = Dieser Computer wird vor dem erneuten Versuch noch einmal geprüft…
imager-setup-error-download-not-offered = Zurzeit wird kein Tilo-Download angeboten.
imager-setup-error-download-multiple = Für diesen Computer werden mehrere Tilo-Downloads angeboten.
imager-setup-error-download-catalog = Die Downloadliste konnte nicht gelesen werden.
imager-setup-status-downloading = Tilo wird heruntergeladen…
imager-setup-status-pausing-download = Der Download wird angehalten…
imager-setup-error-no-saved-download = Es ist kein gespeicherter Download verfügbar.
imager-setup-status-cancelling-download = Der Download wird abgebrochen…
imager-setup-error-no-unfinished-download = Es sind keine unfertigen Download-Daten verfügbar.
imager-setup-status-discarding-download = Unfertige Download-Daten werden verworfen…
imager-setup-error-no-verified-download = Es wird kein geprüfter Download vorgehalten.
imager-setup-status-keeping-download = Der Download wird für später behalten…
imager-setup-status-staging-unavailable = Es wurde noch nichts installiert. Die geschützte Ablage ist auf diesem Computer nicht verfügbar.
imager-desktop-installed-sign-out = Tilo Desktop ist installiert und geprüft. Melde dich ab, wenn du so weit bist.
imager-desktop-cannot-download = Tilo lässt sich auf diesem Computer nicht herunterladen, daher wurde nichts installiert.
imager-setup-internal-download-cancelled-detail = Der Download wurde abgebrochen, bevor er fertig war.

## What the download card says under its heading
imager-setup-internal-download-choose = Wähle Tilo herunterladen, nachdem du diese Einrichtung geprüft hast.
imager-download-sentence-checking-release = Die Tilo-Version wird geprüft.
imager-download-sentence-checking-earlier = Es wird geprüft, was früher heruntergeladen wurde.
imager-download-sentence-downloading = Tilo wird heruntergeladen und geprüft.
imager-download-sentence-ready = Der Download ist geprüft. Es wurde nichts installiert.
imager-download-sentence-paused = Angehalten. Beim Fortsetzen wird zuerst geprüft, was heruntergeladen wurde.
imager-download-sentence-cancelled = Abgebrochen. Heruntergeladene Daten bleiben, bis du sie verwirfst.
imager-download-sentence-failed = Der Download wurde nicht fertig.
imager-download-sentence-released = Der geprüfte Download bleibt auf diesem Computer. Es wurde nichts installiert.
imager-download-sentence-discarded = Unfertige Download-Daten wurden entfernt. Fertige Teile können bleiben.
imager-download-sentence-handoff-failed = Der geprüfte Download bleibt erhalten; die Installation wurde beendet.

## What the wizard reports about the target it was given
installer-status-install-start = Die Installation auf { $disk } für { $user } beginnt ({ $filesystem }).
installer-status-install-start-server = Die Installation auf { $disk } beginnt ({ $filesystem }).
installer-blocked-no-such-disk = Der Datenträger { $device } ist nicht aufgeführt; aktualisiere die Liste.
installer-error-unknown-filesystem = Unbekanntes Dateisystem: { $filesystem }
installer-keyboard-map-name = Tastaturbelegung { $layout }
## „Laufwerke erneut suchen“ in der Fensterleiste
imager-rescan-drives = Laufwerke erneut suchen

## The customization form's sections and fields
imager-profile-section-identity = Identität
imager-profile-section-region = Region
imager-profile-section-storage = Speicher
imager-profile-section-network-identity = Netzwerk und Identität
imager-profile-hostname = Hostname
imager-profile-full-name = Vollständiger Name
imager-profile-username = Benutzername
imager-profile-locale = Sprache und Region
imager-profile-timezone = Zeitzone
imager-profile-keyboard = Tastaturbelegung
imager-profile-filesystem = Dateisystem
imager-profile-hostname-required = Bitte einen Hostnamen eingeben.
imager-profile-username-required = Bitte einen Benutzernamen eingeben.
imager-profile-required = Bitte einen Wert eingeben.
imager-profile-choose-listed = Bitte einen Wert aus der Liste wählen.
imager-profile-choose-listed-field = Bitte { $field } aus der Liste wählen.
imager-write-setup-wifi-ssid = WLAN-Netzwerk
imager-write-setup-wifi-psk = WLAN-Passwort
imager-write-setup-ssh-key = Öffentlicher SSH-Schlüssel der Eigentümerin
imager-write-setup-device-label = Gerätename
imager-write-setup-enroll = Als neues Gerät aufnehmen
imager-write-setup-enroll-note = Das neue Gerät wird eines deiner Geräte. Dein Kontoschlüssel bleibt auf diesem Computer.

## Where a release runs, and the filesystem it installs
imager-release-target-arm64 = Generisch ARM64
imager-release-target-x86-64 = Generisch x86-64
imager-release-target-media = Generisches Medium
imager-release-target-profile = Nur { $profile }
imager-filesystem-btrfs = Btrfs
imager-filesystem-ext4 = Ext4
imager-filesystem-xfs = XFS
installer-network-via = Internet über { $ssid } (für den ersten Start gespeichert)
installer-network-available = Internet verfügbar
installer-network-offline-available = { $connectivity }. Die Offline-Installation bleibt möglich.
installer-connectivity-full = Mit dem Internet verbunden
installer-connectivity-portal = Anmeldung erforderlich
installer-connectivity-limited = Eingeschränkte Verbindung
installer-connectivity-offline = Offline
installer-connectivity-unknown = Verbindung nicht geprüft

## Actions

imager-destination-set-media-codecs = Medien-Codecs installieren
imager-setup-action-install-desktop = Tilo Desktop installieren

## What the app is called in a window and in the launcher
installer-app-title = Tilo-Installer

## Action and resource names, as the palette, agents and console list them
imager-destination-back = Zurück
imager-destination-begin = Installation starten
imager-destination-connect-network = Mit dem gewählten WLAN verbinden
imager-destination-next = Weiter
imager-destination-return-to-host = Zum bestehenden System zurückkehren
imager-destination-select-disk = Diese Festplatte verwenden
imager-destination-select-network = WLAN auswählen
imager-destination-select-region = Diesen Bereich verwenden
imager-destination-set-disk-passphrase = Eigenes Startgeheimnis für die Festplatte festlegen
imager-destination-set-filesystem = Dateisystem auswählen
imager-destination-set-hostname = Gerätenamen im Netzwerk festlegen
imager-destination-set-identity = Benutzernamen und Namen festlegen
imager-destination-set-layout = Tastaturbelegung auswählen
imager-destination-set-locale = Sprache und Region auswählen
imager-destination-set-method = Installationsart auswählen
imager-destination-set-passphrase = Konto- und Startgeheimnis festlegen
imager-destination-set-password-hash = Vorab gehashtes Kontopasswort festlegen
imager-destination-set-security-key = Sicherheitsschlüssel registrieren
imager-destination-set-timezone = Zeitzone auswählen
imager-destination-status = Status des Installers
imager-installer-preview-back-in-preview = Zur vorherigen Seite zurückkehren
imager-installer-preview-check-storage = Festplatte dieses Computers erneut prüfen
imager-installer-preview-choose-and-continue-preview = Option wählen und weiter
imager-installer-preview-choose-preview = Option auf dieser Seite wählen
imager-installer-preview-clear-target-hardware-report = Importierten Hardwarebericht nicht mehr verwenden
imager-installer-preview-continue-preview = Weiter zur nächsten Seite
imager-installer-preview-edit-preview = Einen Abschnitt der Einrichtung ändern
imager-installer-preview-import-target-hardware-report = Hardwarebericht des anderen Computers importieren
imager-installer-preview-open-migrations = Migrationen öffnen
imager-installer-preview-prepare-desktop-installation = Tilo Desktop für diesen Benutzer vorbereiten
imager-installer-preview-prepare-direct-installation = Installation auf diesem Computer vorbereiten
imager-installer-preview-prepare-usb = USB-Medienschreiber öffnen
imager-installer-preview-read-preview = Auswahl der Einrichtung lesen
imager-installer-preview-recheck-target-hardware = Beschreibung des anderen Computers erneut prüfen
imager-installer-preview-restart-preview = Einrichtung neu beginnen
imager-installer-preview-set-migrate-ticket = Ticket von tilo device add eingeben
imager-installer-preview-set-preview-confirmation = Entsperrpasswort bestätigen
imager-installer-preview-set-preview-device-name = Namen dieses Geräts festlegen
imager-installer-preview-set-preview-password = Entsperrpasswort eingeben
imager-installer-preview-set-storage-size = Speicherplatz für Tilo auswählen
imager-installer-preview-set-target-architecture = Architektur wählen, auf der das Medium startet
imager-installer-preview-set-target-identity-field = Den anderen Computer benennen
imager-installer-preview-sign-in-for-migration = Mit dem Ticket beim Workspace anmelden
imager-installer-preview-toggle-storage-details = Aufteilung der Festplatte ein- oder ausblenden
imager-setup-cancel-desktop-download = Download abbrechen
imager-setup-read-desktop-status = Bereitschaft für Tilo Desktop auf diesem Computer lesen
imager-setup-read-setup-status = Status der Einrichtung lesen
imager-setup-recheck-desktop = Verfügbarkeit von Tilo Desktop erneut prüfen
imager-sources = Abbildquellen
imager-sources-clear-source = Gewähltes Abbild entfernen
imager-sources-select-source = Abbild zum Schreiben wählen
imager-write-plan-cancel-download = Download des Installers abbrechen
imager-write-plan-choose-drive = Ziellaufwerk auswählen
imager-write-plan-customize-image = Abbilddatei anpassen
imager-write-plan-export-config = Konfiguration für den Betrieb ohne Bildschirm exportieren
imager-write-plan-set-ssh-key = SSH-Schlüssel des Besitzers festlegen
imager-write-plan-set-wifi-psk = WLAN-Passwort festlegen
imager-write-plan-status = Schreibstatus anzeigen
imager-write-plan-write = Laufwerk löschen und Abbild schreiben
