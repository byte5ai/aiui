# aiui-Widget-Katalog

Dieser Katalog ist für den Agent — er erklärt, wann welches Widget zu nutzen ist und wann nicht. Alle Widgets sind MCP-Tools unter dem Namespace `aiui_*` (bzw. `mcp__aiui__*` je nach Client).

## Entscheidungsregel (TL;DR)

| User-Intent | Widget |
|---|---|
| Einfache Bestätigung (Ja/Nein) | `confirm` |
| Eine Wahl aus 2–6 Optionen | `ask` |
| Freie Eingabe + ggf. mehrere Felder / Liste / Aktionen | `form` |
| Bei destruktiven Aktionen | `confirm` mit `destructive=True` |

**Nicht** zum UI greifen, wenn der Agent die Antwort per Chat-Prompt genauso klar und effizient bekommt. UI ist für Situationen, in denen Struktur, Vergleichbarkeit oder Reduktion von Tippfehlern einen messbaren Mehrwert liefert.

---

## `confirm`

Ja/Nein-Dialog. Returnt `{cancelled, confirmed}`.

```python
aiui.confirm(
    title="Migration starten?",
    message="15 Tabellen werden umgezogen. Dauer ca. 3 Min.",
    destructive=False,
)
```

**Nutze für**: harte Ja/Nein-Entscheidungen, besonders destruktive Aktionen. Ein Wort als Erklärung, nicht mehr.

**Nicht nutzen für**: offene Fragen oder mehr als zwei Optionen → dann `ask`.

---

## `ask`

AskUserQuestion-Superset mit Options und optionaler Freitext-Option. Returnt `{cancelled, answers: [values], other?}`.

```python
aiui.ask(
    question="Welche Strategie für die Migration?",
    options=[
        {"label": "In-place", "description": "Schnell, aber kein Rollback.", "value": "inplace"},
        {"label": "Blue-Green", "description": "Doppelte Infra, sicher.", "value": "bluegreen"},
        {"label": "Dump & Restore", "description": "Downtime ca. 10 Min.", "value": "dump"},
    ],
    multi_select=False,
    allow_other=True,
)
```

**Nutze für**: Single- oder Multi-Choice mit Erklärungs-Bedarf. 2–6 Optionen sind gut lesbar.

**Nicht nutzen für**: mehr als 8 Optionen (unübersichtlich → `form` mit `list`), oder wenn die Optionen sich dynamisch aus freier Eingabe ergeben.

---

## `form` — der Komposit-Baustein

Ein vollständiges Fenster mit beliebig kombinierten Field-Blöcken und mehreren Action-Buttons. Returnt `{cancelled, action?, values}`. `values` ist ein Objekt `{field_name: value, ...}`.

### Feld-Typen

| Kind | Zweck | Result-Typ |
|---|---|---|
| `text` | Ein- oder mehrzeiliger Text | String |
| `password` | Secrets, Eingabe maskiert | String |
| `number` | Zahl mit min/max/step | Number |
| `select` | Dropdown | String (value) |
| `checkbox` | Einzelner Bool-Toggle | Bool |
| `slider` | Bereich mit Live-Anzeige | Number |
| `date` | ISO-Datum | String (`YYYY-MM-DD`) |
| `static_text` | Reine Info/Erklärung (kein Input) | — |
| `list` | Universale Liste (siehe unten) | `{selected, order}` |

### `list` — vier Modi in einem Widget

| `selectable` | `multi_select` | `sortable` | Ergebnis |
|---|---|---|---|
| — | — | — | Info-Liste (read-only) |
| ✓ | — | — | Single-Choice (Radio-Style) |
| ✓ | ✓ | — | Multi-Choice (Checkbox-Liste) |
| — | — | ✓ | Reihenfolge festlegen (Drag) |
| ✓ | ✓ | ✓ | Pick-and-Order (selektieren + sortieren) |

Response: `{selected: [values], order: [values]}` — `order` ist immer vorhanden (auch bei nicht-sortable).

### `actions` — Footer mit beliebig vielen Buttons

Wenn nicht gesetzt: Default = Abbrechen + Senden. Mit `actions` hat der Agent mehrere Pfade:

```python
actions=[
    {"label": "Speichern", "value": "save", "primary": True},
    {"label": "Später", "value": "defer", "skip_validation": True},
    {"label": "Verwerfen", "value": "discard", "destructive": True},
]
```

Response: `{action: "save", values: {...}}`. `skip_validation=True` erlaubt, dass der User abhauen kann, auch wenn Pflichtfelder leer sind.

### Beispiel: Feature-Brief-Dialog

```python
aiui.form(
    title="Neuer Feature-Entwurf",
    header="Discovery",
    fields=[
        {"kind": "static_text", "text": "Skizziere kurz den Nutzer-Job. Max. 2 Sätze."},
        {"kind": "text", "name": "job", "label": "User-Job", "multiline": True, "required": True},
        {"kind": "select", "name": "scope", "label": "Scope",
         "options": [
             {"label": "Quick Win", "value": "qw"},
             {"label": "Feature", "value": "f"},
             {"label": "Epic", "value": "e"},
         ], "default": "f"},
        {"kind": "list", "name": "stakeholders", "label": "Stakeholder einbinden",
         "items": [
             {"label": "Produkt", "value": "prod"},
             {"label": "Design", "value": "design"},
             {"label": "Engineering", "value": "eng"},
             {"label": "Sales", "value": "sales"},
         ],
         "selectable": True, "multi_select": True,
         "default_selected": ["prod", "eng"]},
        {"kind": "date", "name": "deadline", "label": "Gewünschter Launch"},
    ],
    actions=[
        {"label": "Abbrechen", "value": "cancel", "skip_validation": True},
        {"label": "Entwurf speichern", "value": "draft", "skip_validation": True},
        {"label": "Los geht's", "value": "commit", "primary": True},
    ],
)
```

### Anti-Patterns

- **Ein-Feld-Fenster mit nur `text`-Input** → Stattdessen Chat-Prompt. UI ist overkill.
- **Lange Erklärungen in `static_text`** → Halte es knapp. Mehr als 3 Sätze gehören als Chat-Message vorher.
- **Dieselbe Liste als `select` und `list`** → `select` für 2–8 simple Optionen, `list` für alles was Reihenfolge, Mehrfachauswahl oder Beschreibungen braucht.
- **`confirm` verkleiden als `form`** → Wenn's nur Ja/Nein ist, `confirm` nutzen.

---

## Anti-Patterns (überall gültig)

- **UI für Status-Anzeige ohne Interaktion**: Nein. Chat-Message reicht.
- **UI um Datei-Inhalte zu zeigen**: Nein. Agent hat Read-Tool.
- **Wiederholte kleine Rückfragen**: Eine `form` mit mehreren Feldern ist besser als fünf einzelne `ask`s.
- **UI für rein technische Ausgaben** (Logs, JSON-Dumps): Chat rendert das ebenso gut.

## Pattern-Kompass

| Situation | Empfehlung |
|---|---|
| Single-Choice mit Erklärungstext pro Option | `ask` |
| Multi-Step-Wizard | (v1.2 — aktuell: `form` mit `static_text`-Steps) |
| Datei soll ausgewählt werden | **Nicht per aiui** — User nutzt Claude Desktop File-Upload |
| Secret eingeben | `form` mit `kind: password` |
| Settings-Panel mit vielen Optionen | `form` |
| Konfliktlösung (pick A/B/C) | `ask` mit Beschreibungen |
| Priorisierung | `form` mit `list` + `sortable: true` |
