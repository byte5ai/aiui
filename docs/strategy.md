# aiui — Product Strategy

> Lebendes Strategie-Dokument. Hält die Klammer, gegen die jede Feature-, Architektur- und Roadmap-Entscheidung gemessen wird. Konkrete Features gehören nicht hier rein — die laufen als GitHub-Issues.

## Was aiui ist

aiui ist ein Kanal, über den Coding-Agenten auf dem Mac des Users echte UI rendern — strukturierte Eingaben, die im Chat zu unhandlich oder zu fehleranfällig wären. Topologie heute: Tauri-Companion am Mac, läuft als lokaler MCP-Server in Claude Desktop; Remote-Agenten (Claude Code via SSH) erreichen ihn über Reverse-Tunnel und POSTen Dialog-Specs an `localhost:7777`.

## Designregel

**Kandidaten für die aktuelle Produktstufe (V1) müssen drei Dinge gleichzeitig erfüllen:**

1. **short-lived** — die UI lebt nur für den aktuellen Turn, danach ist sie weg.
2. **agent-actively-used** — ein realer Agent greift dazu in seiner täglichen Arbeit, nicht "könnte man theoretisch".
3. **user-Mehrwert** — die UI bringt dem User mehr als die Chat-Alternative; sie reißt ihn nicht aus dem Kontext, sondern liefert genau dort, wo Chat unstrukturiert würde.

Wer eines der drei nicht erfüllt, gehört nicht in V1 — entweder gar nicht ins Produkt oder in eine andere Stufe.

## Paradigma-Trennung: V1 vs. V2

aiui hat zwei Daseinsformen, die sich nicht ablösen, sondern **koexistieren** wie Dropdown-Menü und Vollbild-App in derselben Mac-Anwendung. Beide bleiben dauerhaft sinnvoll, weil sie unterschiedliche Szenarien bedienen.

| | **V1 — Strukturierte Eingabe ohne Kontext-Bruch** | **V2 — Bühne übernehmen** |
|---|---|---|
| Szenario | Agent braucht 5 Sekunden strukturierten Input vom User | Agent übernimmt für eine Phase die Aufmerksamkeit, User taucht ein |
| Lebensdauer | ein Turn | über mehrere Turns, eigene Session |
| Lifecycle-Owner | der Agent (öffnet, wartet, schließt) | die Surface selbst |
| Form | Modal-Fenster, OS-Notification | Vollbild-Surface mit eigener Navigation und State |
| Sichtbarkeit von macOS/Claude Desktop | ständig sichtbar, aiui ist Gast | tritt zurück, aiui ist Hauptbild |
| Versprechen | "Ich frage Dich kurz, dann geht's für Dich weiter wie bisher" | "Für die nächsten Minuten arbeitest Du in einer Oberfläche, die der Agent gestaltet" |

V1 ist heute weitgehend gebaut (drei Tools, zwölf Field-Types). V2 existiert noch nicht.

### Warum die Trennung scharf bleiben muss

V2 ist **kein V1-mit-mehr-Features**. Wer V1 immer weiter aufbohrt — persistente Modals, aggregierte Inboxen, halb-langlebige Surfaces — landet in einer Mischform, die weder das eine noch das andere sauber bedient. V1 muss in sich rund werden und dann scharf bleiben; V2 entsteht als eigener Schritt mit eigenem Architektur-Set.

## Nordstern: DOS → Windows → eigenständiges OS

| Phase | DOS/Windows | aiui |
|---|---|---|
| Heute | DOS-Prompt, gelegentlicher Grafik-Hop | macOS/Claude Desktop mit Modal-Fenstern (V1) |
| Nächster Schritt | Windows 3.0: aus DOS gestartet, übernimmt aber den ganzen Bildschirm | aiui-Vollbild-Surface, generative Oberfläche, macOS rückt aus dem Sichtfeld (V2) |
| Fernziel | Windows 95/NT standalone | aiui braucht macOS/Claude Desktop nicht mehr als Träger |

Die letzte Stufe ist heute weit weg und kein Roadmap-Punkt. Sie ist trotzdem wichtig, weil sie verhindert, dass das Produkt sich als "noch ein Tauri-Widget-Toolkit" missversteht. **Jede Investitionsentscheidung wird daran gemessen, ob sie Richtung Norden zeigt, neutral ist oder davon wegführt.** Neutral ist okay (nicht jedes Feature muss strategisch sein), aber dauerhaft nur neutral wäre Stillstand.

## Architektur-Prinzipien

Gelten unabhängig von V1/V2:

**1. Lifecycle-Owner-Prinzip.** Jede UI-Surface braucht einen Owner, der ihren Anfang und ihr Ende verantwortet. In V1 ist der Owner der Agent — er öffnet, wartet synchron, bekommt Result, fertig. Persistente Surfaces (über Turns hinweg) verlangen einen anderen Owner als den Agenten selbst, weil Agenten vergessen, abstürzen, in Compaction sterben oder ihre Session vom User nicht fortgeführt wird. **Folge:** Persistente UI gehört per Definition nicht in V1. Wer ein "live status pane" oder "progress window" baut, das überleben soll, hat einen Architektur-Mismatch oder muss V2 bauen.

**2. Anti-Slop durch Skill-Doku.** Agenten bauen Slop, wenn das Vokabular zu groß und die Anleitung zu dünn ist. [`docs/skill.md`](skill.md) ist nicht Begleitdoku, sondern aktives Steuerinstrument: jedes neue Widget bekommt einen Eintrag mit klarem When-to-use, einem sauberen Beispiel und einer Anti-Pattern-Spalte. Widgets ohne diesen Eintrag werden nicht gemerged.

**3. aiui ist keine Datei-Transfer-Schicht.** Der Reverse-Tunnel transportiert UI-Specs und Antworten — keine Dateien, keine Inhalte vom Mac auf den Remote-Host. Wer das aufweicht, baut sich eine zweite, parallele Sync-Infrastruktur, die Sicherheits- und Pairing-Annahmen sprengt.

**4. Keine Übernahme von Aufgaben, die Claude Desktop besser löst.** Wenn ein UI-Element einen Lifecycle-Owner braucht, den nur Claude Desktop selbst sauber besitzt (Session-State, Aufgaben-Pane, Permissions-Flow), gehört das Feature dorthin, nicht in aiui. aiui ergänzt Claude Desktop, dupliziert es nicht.

**5. Erweiterungs-Naht respektieren.** Neue Form-Felder gehen in `companion/src/lib/widgets/Form.svelte` plus Schema-Eintrag in `companion/src-tauri/src/mcp.rs`. Neue Dialog-Arten kriegen ein eigenes Svelte-Widget und ein eigenes MCP-Tool. Der HTTP-Render-Contract (`POST /render` mit `{spec: {kind, ...}}`) bleibt stabil. Keine Sonderlocken am Transport.

## V2-Voraussetzungen

V2 ist nicht nur "Vollbild statt Modal". Es verlangt drei strukturell neue Dinge, die V1 nicht hat und die nicht inkrementell aus V1 fallen:

1. **Eine reichere UI-Beschreibungssprache** als das aktuelle JSON-Spec. Irgendwo zwischen DSL und "der Agent streamt Komponenten-Bäume". Das ist die eigentliche Designarbeit für V2 und der Punkt, an dem das Produkt entweder elegant oder sloppy wird.
2. **State und Navigation über die Zeit.** Nicht "ein Call = ein Result", sondern Surfaces, die leben, vom Agenten weiter bespielt werden, in sich navigierbar sind.
3. **Voice und User-Initiative als Primär-Input.** Tippen ist in einer Vollbild-Surface zu eng. Globaler Hotkey, Sprachausgabe, Sprach-Eingabe — und vor allem: der User initiiert, nicht nur der Agent. Das ist die Umkehr der heutigen response-only-Logik.

## Orthogonale Achse: Reichweite

iOS-Companion plus Cloud-Relay (oder iCloud-Container) sind eine **eigene Achse**, weder V1 noch V2. Sie entkoppeln aiui von "User sitzt am Mac" und "User hat SSH-Reverse-Tunnel eingerichtet". Adoptions-Hebel ist groß (Codespaces, Devcontainer, fremde Maschinen, mobile Antworten unterwegs), Trade-off ist Cloud-Infrastruktur und Vertrauensfrage.

Diese Achse kann V1 oder V2 begleiten und wird nicht damit verwechselt. Sie verschiebt das Produkt geographisch, nicht paradigmatisch.

## Was wir explizit nicht bauen — und warum

Was wir nicht bauen, ist genauso strategisch wie was wir bauen.

| Idee | Warum nicht |
|---|---|
| `pick_path` (NSOpenPanel) | Macht aiui zur Datei-Transfer-Schicht (Mac-Pfad → Remote-Agent → Inhalt). Bricht Architektur-Prinzip 3. Use-Case-Dichte für die Remote-Topologie zu dünn. |
| `diff` als eigenes Tool | Marginal. Claude Desktop hat eigenen Diff-Viewer für lokale Sessions. Für Remote-CLI ist ein Window-Diff bestenfalls Komfort, nicht Kategoriefehler-Behebung. Nicht aiui's Job (Prinzip 4). |
| `progress` mit Live-Updates | Verlangt langlebigen Owner, den der Agent strukturell nicht sauber sein kann. Verstößt gegen Prinzip 1. Verwaiste Progress-Bars sind das wahrscheinliche Resultat. Gehört in Claude Desktop's Aufgaben-Pane. |
| Multi-Agent-Inbox als V2 | Falsche Einsortierung. Inbox-Aggregation bleibt im Modal-Paradigma — sie ist V1-Erweiterung, nicht V2. V2 ist Paradigma-Bruch, nicht Skalierung. |

## Prozess-Konsequenzen

- Jede neue Feature-Idee wird gegen Designregel und Nordstern geprüft, bevor sie ins Backlog kommt.
- Feature-Backlog läuft als GitHub-Issues, nicht als Anhang an dieses Paper.
- Polish-Themen laufen ebenfalls als Issues, getaggt als `polish`.
- Dieses Paper ist lebendes Dokument, aber kein Tagebuch — es wird überarbeitet, wenn sich die Strategie ändert, nicht jedes Mal, wenn ein Feature dazukommt.
