<p align="center">
  <img src="assets/aiui-logo.png" alt="aiui" width="320">
</p>

<p align="center">
  Native dialogs for Claude Code — ask, confirm, collect — right where you work.
</p>

<p align="center">
  <a href="https://github.com/byte5ai/aiui/releases/latest">
    <img alt="Download aiui.app" src="https://img.shields.io/badge/Download-aiui.app-4f46e5?style=for-the-badge&logo=apple">
  </a>
  <a href="https://github.com/byte5ai/aiui/blob/main/LICENSE">
    <img alt="MIT" src="https://img.shields.io/badge/License-MIT-171717?style=for-the-badge">
  </a>
</p>

---

## Was ist aiui?

Claude Code ist mächtig im Chat, aber wenn der Agent Dir eine klare Wahl
vorlegen will, ein Formular mit mehreren Feldern braucht, oder eine
destruktive Aktion bestätigen lassen muss, zwingt er Dich normalerweise
wieder ins Tippen. **aiui** baut diese Momente zu echten Dialogen um, die
direkt auf Deinem Mac erscheinen — egal ob der Agent lokal läuft oder via
SSH auf einem Remote-Host.

<p align="center">
  <img src="assets/aiui-icon.png" alt="aiui app icon" width="200">
</p>

Ein Klick, eine Auswahl, eine Antwort — der Agent bekommt sie strukturiert
zurück und arbeitet weiter. Kein Doppeltippen, keine selbstgebauten
Web-Dashboards, keine Kontextwechsel.

## Installation

1. **[aiui.app herunterladen](https://github.com/byte5ai/aiui/releases/latest)**
   (DMG, Apple Silicon) und per Drag-and-Drop nach **Applications** ziehen.
2. aiui einmal starten (Finder-Doppelklick).
   Beim ersten Start richtet aiui sich automatisch in Claude Desktop ein.
3. Claude Desktop neu starten.

Das war's. aiui läuft ab jetzt unsichtbar im Hintergrund, sobald Claude
Desktop offen ist. Beim Schließen von Claude Desktop beendet es sich selbst.

Künftige Updates kommen automatisch — aiui meldet sich kurz mit einer
„Update verfügbar"-Meldung, ein Klick, fertig.

## Nutzung in einem Projekt

Füge in Deinem Projekt eine Datei `.mcp.json` an (oder ergänze eine
bestehende):

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

[`uv`](https://docs.astral.sh/uv/) holt sich die aktuelle Version beim
ersten Start automatisch — Du musst nichts weiter installieren. Sobald Du
Claude Code in diesem Projekt öffnest, sind die aiui-Werkzeuge verfügbar:

- **Ask** — Frage mit 2–6 Optionen
- **Confirm** — Ja/Nein, optional rot für destruktive Aktionen
- **Form** — Fenster mit mehreren Feldern (Text, Zahl, Liste, Datum,
  Farbe, Baum, …) und mehreren Buttons

Probier's mit einer Chat-Zeile wie *„Zeig mir kurz mit aiui drei
Deploy-Strategien zur Auswahl"*.

## Remote-Hosts

Arbeitest Du mit Claude Code auf einem Remote-Server (z.B. über Claude
Desktop's SSH-Feature)? Öffne `/Applications/aiui.app` per Finder-Klick,
trag in den Settings den SSH-Alias Deines Hosts ein (oder `user@hostname`)
und klick „Einrichten". aiui

- hinterlegt einen Auth-Token auf dem Remote-Host,
- kopiert die Agent-Regeln dorthin,
- hält einen verschlüsselten Tunnel zurück zu Deinem Mac,
- reconnected automatisch bei Netzwechsel oder Suspend.

Danach läuft der Remote-Agent genauso wie lokal.

## Für Agents: der Skill

aiui installiert beim Start automatisch ein Skill-Doc
(`~/.claude/skills/aiui/SKILL.md`), das Claude Code jedesmal lädt. Darin
stehen klare Regeln, wie der Agent gute Dialoge baut — welches Widget zu
welchem Zweck, wie Labels klingen, welche Anti-Patterns zu vermeiden sind.
Das hält Dialoge konsistent und verhindert „UI-Slop".

Auf jedem Remote, den Du einrichtest, landet das Skill-Doc ebenfalls
automatisch.

## Hilfe & Troubleshooting

| Symptom | Check |
|---|---|
| Dialog kommt nicht | `/Applications/aiui.app` öffnen, Status-Anzeige oben prüfen. Remote muss dort auf „verbunden" stehen. |
| „aiui companion not reachable" im Agent-Chat | Claude Desktop ist noch nicht offen, oder der Mac ist im Ruhezustand. |
| „token rejected (401)" | Ein alter aiui-Prozess hält noch Port 7777 auf dem Remote belegt. `pkill -f aiui` auf dem Remote, dann den Remote in aiui-Settings einmal „Entfernen" + neu „Einrichten". |
| App öffnet nicht (Gatekeeper) | Ab v0.2.0 sind Releases Apple-notarisiert, Gatekeeper lässt sie durch. Wenn Du eine Entwickler-Zip nutzt: `xattr -dr com.apple.quarantine /Applications/aiui.app`. |

Bugs oder Wünsche → [**Issue aufmachen**](https://github.com/byte5ai/aiui/issues/new). In
aiui-Settings gibt's dafür einen Direktbutton, der Version und Build-ID
vorausfüllt.

## Datenschutz

aiui läuft komplett lokal auf Deinem Mac. Es sendet keine Telemetrie,
keine Nutzungsdaten, keine Content-Snippets. Der Auth-Token bleibt in
`~/.config/aiui/` (Mode 0600) und wird nur per SSH auf Hosts übertragen,
die Du explizit eingerichtet hast.

## Weiter lesen

- [Widget-Katalog](docs/skill.md) — Entscheidungsregeln, Beispiele,
  Anti-Patterns (identisch mit dem Skill-Doc, das aiui installiert).
- [Changelog](CHANGELOG.md) — Alle Änderungen pro Release.
- [Contributing](CONTRIBUTING.md) — Wenn Du mitbauen willst.
- [aiui-mcp auf PyPI](https://pypi.org/project/aiui-mcp/) — das Paket,
  das `uvx aiui-mcp` im Hintergrund zieht.

## Lizenz

MIT — siehe [LICENSE](LICENSE).
