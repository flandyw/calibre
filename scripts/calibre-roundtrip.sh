#!/usr/bin/env bash
# Round trip a library between upstream calibre and `cr`:
#   calibre creates it -> cr edits it -> calibre must still read it cleanly.
# Needs `calibredb` on PATH and a built `cr` (default: target/release/cr).
set -euo pipefail

CR_BIN="$(realpath "${CR_BIN:-target/release/cr}")"
command -v calibredb >/dev/null || { echo "calibredb not found on PATH" >&2; exit 2; }
[ -x "$CR_BIN" ] || { echo "cr binary not found at $CR_BIN" >&2; exit 2; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cd "$work"
lib="$work/library"
cdb() { calibredb --with-library "$lib" "$@"; }
cr() { "$CR_BIN" --library "$lib" "$@"; }

printf 'Chapter 1\n\nHello world.\n' > dune.txt
printf 'Chapter 1\n\nA second edition.\n' > dune2.txt
printf '<html><head><title>Ünïcode Bök</title><meta name="author" content="José Ångström"></head><body><p>Hej</p></body></html>' > unicode.html
printf 'Chapter 1\n\nAdded by cr.\n' > added.txt

# 1. Upstream calibre creates the library: multiple authors, series, tags,
#    two formats on one book, non-ASCII names and custom columns.
cdb add dune.txt --title "Dune" --authors "Frank Herbert & Brian Herbert" \
    --tags "sf,classic" --series "Dune Saga" --series-index 1 >/dev/null
cdb add unicode.html >/dev/null
cdb add_format 1 unicode.html >/dev/null
cdb add_custom_column genre Genre text --is-multiple >/dev/null
cdb add_custom_column read Read bool >/dev/null
cdb set_custom genre 1 "space opera,desert" >/dev/null
cdb set_custom read 1 true >/dev/null

# 2. cr reads and edits it.
cr --json list > before.json
cr set 1 --title "Dune Messiah" --tag "sf,epic" --rating 8 >/dev/null
cr add added.txt >/dev/null
cr rm 2 --delete-files >/dev/null

# 3. Upstream calibre reads the result.
cdb list -f title,authors,tags,series,rating,formats,*genre,*read --for-machine > after.json
cdb check_library --csv > check.csv

python3 - "$lib" <<'PY'
import json, os, sys

lib = sys.argv[1]
before = {b["id"]: b for b in json.load(open("before.json"))}
assert sorted(before) == [1, 2], before
assert before[1]["title"] == "Dune" and before[1]["authors"] == ["Frank Herbert", "Brian Herbert"], before[1]
assert sorted(before[1]["formats"]) == ["TXT", "ZIP"], before[1]
assert before[2]["title"] == "Ünïcode Bök", before[2]

after = {b["id"]: b for b in json.load(open("after.json"))}
assert sorted(after) == [1, 3], sorted(after)
dune = after[1]
assert dune["title"] == "Dune Messiah", dune
assert dune["authors"] == "Frank Herbert & Brian Herbert", dune
assert sorted(dune["tags"]) == ["epic", "sf"], dune
assert dune["series"] == "Dune Saga" and dune["rating"] == 8, dune
assert dune["*genre"] == ["space opera", "desert"] and dune["*read"] is True, dune
assert len(dune["formats"]) == 2, dune
for book in after.values():
    for path in book["formats"]:
        assert os.path.isfile(path), f"missing format file {path}"
        assert os.path.isfile(os.path.join(os.path.dirname(path), "metadata.opf")), path
expected = os.path.join(lib, "Frank Herbert", "Dune Messiah (1)")
assert os.path.dirname(dune["formats"][0]) == expected, dune["formats"]

problems = [line for line in open("check.csv").read().splitlines() if "," in line]
assert not problems, "calibre check_library reported:\n" + "\n".join(problems)
leftovers = [n for n in os.listdir(lib) if n.endswith(("-wal", "-shm", "-journal"))]
assert not leftovers, leftovers
print("calibre round trip: ok")
PY
