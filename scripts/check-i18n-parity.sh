#!/usr/bin/env bash
#
# check-i18n-parity.sh — keep the dialog chrome translatable.
#
# Two guards, both regressions we actually shipped (#207):
#
#   (a) CATALOG PARITY. `companion/src/i18n/de.json` and `en.json` must
#       carry the same flattened key set. A key added to one catalog only
#       renders as its raw dotted path ("dialog.write_target.value") for
#       every user of the other locale.
#
#   (b) NO HARDCODED GERMAN IN COMPONENTS. A literal umlaut or ß outside a
#       comment in `companion/src/**/*.svelte` means a string bypassed the
#       catalog. That is how the `target` file-write approval line — the
#       ENTIRE authorization surface for writing an agent-chosen path —
#       reached English-locale users as German chrome they skimmed past.
#
# The German catalog is of course full of umlauts; only components are
# scanned. Comments (`<!-- … -->`, `// …`, `/* … */`) are stripped first so
# a German code comment stays allowed — the repo has several, deliberately.
#
# Run locally:  scripts/check-i18n-parity.sh
# CI:           .github/workflows/ci.yml -> job `skill-drift`

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

python3 - "$ROOT" <<'PY'
import json
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1])
status = 0


def flatten(obj, prefix=""):
    keys = set()
    for k, v in obj.items():
        path = f"{prefix}{k}"
        if isinstance(v, dict):
            keys |= flatten(v, path + ".")
        else:
            keys.add(path)
    return keys


# --- (a) catalog parity -------------------------------------------------
catalogs = {}
for name in ("de", "en"):
    p = root / "companion/src/i18n" / f"{name}.json"
    try:
        catalogs[name] = flatten(json.loads(p.read_text(encoding="utf-8")))
    except (OSError, ValueError) as e:
        print(f"ERROR: cannot read {p}: {e}")
        sys.exit(1)

only_de = sorted(catalogs["de"] - catalogs["en"])
only_en = sorted(catalogs["en"] - catalogs["de"])

if only_de:
    status = 1
    print("ERROR: keys in companion/src/i18n/de.json but NOT in en.json:")
    for k in only_de:
        print(f"  - {k}")
if only_en:
    status = 1
    print("ERROR: keys in companion/src/i18n/en.json but NOT in de.json:")
    for k in only_en:
        print(f"  - {k}")

# --- (b) no hardcoded German in components ------------------------------
GERMAN = re.compile(r"[äöüÄÖÜß]")


def strip_comments(text):
    """Blank out comment bodies, keeping newlines so line numbers survive."""

    def blank(m):
        return re.sub(r"[^\n]", " ", m.group(0))

    text = re.sub(r"<!--.*?-->", blank, text, flags=re.S)
    text = re.sub(r"/\*.*?\*/", blank, text, flags=re.S)
    text = re.sub(r"(?m)//[^\n]*", blank, text)
    return text


hits = []
for path in sorted((root / "companion/src").rglob("*.svelte")):
    stripped = strip_comments(path.read_text(encoding="utf-8"))
    for lineno, line in enumerate(stripped.splitlines(), start=1):
        if GERMAN.search(line):
            hits.append((path.relative_to(root), lineno, line.strip()))

if hits:
    status = 1
    print("ERROR: hardcoded German outside a comment in a Svelte component:")
    for rel, lineno, line in hits:
        print(f"  {rel}:{lineno}: {line}")
    print("  -> move the string into companion/src/i18n/{de,en}.json and use $_(…).")

if status == 0:
    print(
        "i18n-parity: OK — catalogs agree "
        f"({len(catalogs['en'])} keys) and no hardcoded German in components."
    )

sys.exit(status)
PY
