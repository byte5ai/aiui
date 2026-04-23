<p align="center">
  <img src="assets/aiui-logo.png" alt="aiui" width="360">
</p>

<p align="center">
  <strong>Claude Code kann fragen, bestätigen, Formulare stellen — als echte macOS-Dialoge.</strong>
</p>

<p align="center">
  <a href="https://github.com/byte5ai/aiui/releases/latest">
    <img alt="Download aiui.app" src="https://img.shields.io/badge/Download%20für%20Mac-aiui.app-4f46e5?style=for-the-badge&logo=apple">
  </a>
  <a href="https://github.com/byte5ai/aiui/blob/main/LICENSE">
    <img alt="MIT License" src="https://img.shields.io/badge/MIT-Open%20Source-171717?style=for-the-badge">
  </a>
</p>

---

## Der Chat ist manchmal der falsche Ort

Wenn Claude Code Dir eine Frage stellt, die wirklich eine Auswahl aus
mehreren Optionen ist, musst Du heute wieder zurück in den Chat und sie
in Worten beantworten. Wenn er eine Bestätigung braucht, bevor er
irgendwo eine Tabelle löscht, erscheint im Chat ein blauer Kasten mit
„Ja/Nein", aber danach nichts Individuelles mehr. Soll er ein Passwort
kurz sehen — landet es im Chat-Verlauf.

Das geht besser.

**aiui** lässt Claude Code echte Dialoge auf Deinem Mac öffnen:

- **„Welche von diesen drei Deploy-Strategien?"** Ein Fenster mit drei
  Karten, klar beschrieben. Du klickst. Fertig.
- **„Soll ich die Produktions-Tabelle 'orders' löschen?"** Roter Button,
  klare Warnung, ein Klick.
- **„Trag mal bitte Name, Rolle und Start-Datum ein."** Ein sauberes
  Formular statt einer tippintensiven Chat-Session.
- **„Priorisiere diese 5 Tickets in der Reihenfolge, die Du magst."**
  Drag-and-Drop, zurück geht die Reihenfolge als saubere Liste.
- **„Gib mir kurz Deinen API-Token."** Maskiertes Feld, nie im Verlauf
  sichtbar.

Der Agent bekommt Deine Antwort strukturiert zurück und arbeitet weiter.
Keine Zwischenkontexte, keine Web-Dashboards, die irgendwo in Deinem
System hängen bleiben. Nur ein hübsches, vertrautes macOS-Fenster.

<p align="center">
  <img src="assets/aiui-icon.png" alt="aiui app icon" width="180">
</p>

## Funktioniert lokal und remote

Du benutzt Claude Code direkt auf Deinem Mac? aiui klinkt sich ein.

Du benutzt Claude Code via SSH auf einem Remote-Server (Dev-Maschine,
Projekt-VM)? aiui richtet automatisch einen Tunnel ein, und der
Remote-Agent kann Dialoge genauso bei Dir am Mac aufpoppen lassen. Ein
einmaliges Eintragen des Hosts in den Einstellungen, danach ist's
selbstverständlich.

## Installation in 3 Minuten

1. **[aiui.app](https://github.com/byte5ai/aiui/releases/latest)
   runterladen** (DMG, Apple Silicon), öffnen und in den
   `Applications`-Ordner ziehen.
2. **Einmal starten** — per Doppelklick im Finder. aiui trägt sich
   automatisch in Claude Desktop ein.
3. **Claude Desktop neu starten.** Das war's.

Ab jetzt läuft aiui unsichtbar im Hintergrund, immer nur so lange wie
Claude Desktop selbst offen ist. Kein Dock-Icon, kein Menubar-Clutter,
keine Hintergrunddienste.

Zukünftige Updates kommen automatisch — aiui meldet sich höflich mit
einer „Update verfügbar"-Nachricht, ein Klick, fertig.

### In Deinem Projekt einbinden

Leg eine Datei `.mcp.json` im Projektordner an (oder ergänze sie):

```json
{
  "mcpServers": {
    "aiui": {
      "command": "uvx",
      "args": ["aiui-mcp"]
    }
  }
}
```

Beim nächsten Claude-Code-Start zieht [`uv`](https://docs.astral.sh/uv/)
automatisch das aktuelle aiui-Server-Paket — Du musst nichts extra
installieren.

Probier's direkt: *„Frag mich kurz mit aiui, welche von drei
Deploy-Strategien ich für heute will."* Der Agent öffnet einen
Optionen-Dialog, Du klickst, er arbeitet weiter.

## Was Du davon hast

| Was Dich heute nervt | Mit aiui |
|---|---|
| Im Chat tippen, was eigentlich ein Klick wäre | Echter macOS-Dialog |
| Destruktive Aktionen mit „bitte bestätigen Sie im Chat" | Rot gestylter Ja-Nein-Button, eindeutig |
| Tokens und Passwörter, die im Verlauf landen | Maskiertes Passwort-Feld, nie im Transcript |
| Selbstgebaute temporäre Web-UIs pro Task | Überflüssig |
| Agent erfindet sich auf Remote-Hosts nichts einfallen lässt | Dialoge tunneln automatisch zurück an Deinen Mac |

## Datenschutz

aiui läuft rein lokal auf Deinem Mac. Keine Telemetrie, keine
Nutzungsdaten, keine Inhalte verlassen Dein System. Ein Auth-Token liegt
in `~/.config/aiui/` (Mode 0600) und wird nur auf Hosts kopiert, die Du
explizit in den Einstellungen einträgst.

## Für Agenten: der Skill

Damit der Agent kein generisches „UI-Slop" produziert, installiert aiui
beim Start ein Skill-Dokument in Claude Codes Skill-Verzeichnis. Darin
stehen klare Regeln: welches Widget wofür, wie Labels klingen, welche
Anti-Patterns zu vermeiden sind. Das hält Dialoge konsistent und
angenehm zu bedienen. Auf jedem Remote, den Du einrichtest, landet das
Skill automatisch mit.

Den kompletten Katalog findest Du in
[`docs/skill.md`](docs/skill.md).

## Hilfe

| Symptom | Was zu tun ist |
|---|---|
| Dialog kommt nicht | `/Applications/aiui.app` öffnen, Status oben prüfen. Remote muss dort grün auf „verbunden" stehen. |
| „aiui companion not reachable" im Chat | Claude Desktop ist zu, oder der Mac pennt. |
| „token rejected (401)" | Alter aiui-Prozess hält den Port auf dem Remote belegt: `pkill -f aiui` auf dem Remote, Remote in aiui-Settings „Entfernen" und neu „Einrichten". |

Bugs oder Wünsche → [Issue aufmachen](https://github.com/byte5ai/aiui/issues/new).
Der „Problem melden"-Button in den Einstellungen füllt Version und
Build-ID vorne an.

## Open Source

aiui ist MIT-lizenziert und bei [byte5ai/aiui](https://github.com/byte5ai/aiui)
zu Hause. Pull Requests und Issues sind willkommen — siehe
[CONTRIBUTING.md](CONTRIBUTING.md) für den Bauplan.

Python-Server-Paket: [`aiui-mcp`](https://pypi.org/project/aiui-mcp/) auf
PyPI.
