## History

weather-save-location = Einen Ort zum Verfolgen gespeichert
weather-use-as-default = { $item } zum Standardort gemacht
weather-move-to-top = { $item } nach oben verschoben
weather-remove = { $item } entfernt
weather-weather-places = { $op ->
    [move-to] Die Orte neu angeordnet
    [insert] Einen Ort hinzugefügt
   *[other] Die Orte geändert
}
weather-weather-default-place = Den Standardort geändert
weather-place-name = { $item } umbenannt

weather-morning-summary = { $temperature }, { $condition } · Beobachtung { $time }
weather-morning-source = Open-Meteo · abgerufen { $fetched }
weather-morning-fixture = Aufnahmebeispiel
weather-morning-stale = Letzte gespeicherte Beobachtung
weather-morning-freshness = Anbieterbeobachtung

# Die eigene Copy der Vorhersageseite.

# Der 24-Stunden-Streifen und die 7-Tage-Vorhersage, wie Screenreader sie
# lesen: eine Zeile pro Stunde und pro Tag mit den Werten, die der Streifen
# zeichnet.
weather-hours = Nächste 24 Stunden
weather-days = Vorhersage für 7 Tage
weather-hour-now = Jetzt
weather-summary = { $temperature }, { $condition }, gefühlt { $feels }. Es folgen { $hours } Stundenwerte und { $days } Tagesvorhersagen.
weather-hour-row = { $temperature }, { $sky }
weather-hour-row-rain = { $temperature }, { $sky }, { $chance } Niederschlagsrisiko
weather-day-row = { $high } bis { $low }, { $sky }
weather-day-row-rain = { $high } bis { $low }, { $sky }, { $chance } Niederschlagsrisiko
weather-day-extras = UV-Index { $uv }, Wind bis { $wind }

# Die Einheit einer Niederschlagsmenge, wie die Kacheln sie zeigen.
weather-unit-millimetres = mm
weather-unit-inches = Zoll

# Wie alt die Vorhersage auf dem Bildschirm ist. $when ist ein relatives
# Alter ("vor 5 Minuten"), die Uhrzeit des Abrufs ("14:05") oder Datum und
# Uhrzeit ("30.06.2026, 14:05").
weather-updated-ago = Aktualisiert { $when }
weather-updated-clock = Aktualisiert um { $when }
weather-updated-date = Aktualisiert { $when }
weather-outdated-ago = Zuletzt aktualisiert { $when }, veraltet
weather-outdated-clock = Zuletzt aktualisiert um { $when }, also veraltet
weather-outdated-date = Zuletzt aktualisiert { $when }, also veraltet
