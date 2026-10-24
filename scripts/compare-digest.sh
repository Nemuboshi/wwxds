#!/usr/bin/env bash
# Compare two `examples/digest` listings section by section.
#
# The digest tool groups its output by input: one `# input=...` header, one line
# per frame, one `# end frames=N` footer. This comparator splits both listings on
# those headers, keys each section by the whole header line, and diffs the two
# sets. That makes the result independent of the order the inputs were passed in
# (the recorded baseline used a hand-written list, not a sorted glob) while still
# failing loudly when an input disappears, appears, or changes its reported
# source properties -- a changed header shows up as a section only one side has.
#
# Usage: scripts/compare-digest.sh <baseline.txt> <new.txt>
#
# Exit status: 0 when every section is byte-identical, 1 otherwise.

set -euo pipefail

if [ "$#" -ne 2 ]; then
    echo "usage: scripts/compare-digest.sh <baseline.txt> <new.txt>" >&2
    exit 2
fi

baseline=$1
new=$2

for file in "$baseline" "$new"; do
    if [ ! -f "$file" ]; then
        echo "compare-digest: no such file: $file" >&2
        exit 2
    fi
done

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Split one listing into "$work/<side>/<sanitised header>" files.
split_sections() {
    local source=$1
    local side=$2
    mkdir -p "$work/$side"
    awk -v dir="$work/$side" '
        /^# input=/ {
            key = $0
            gsub(/[^A-Za-z0-9]+/, "_", key)
            current = dir "/" key ".txt"
            seen[key]++
            if (seen[key] > 1) {
                current = dir "/" key "_dup" seen[key] ".txt"
            }
        }
        current != "" { print > current }
    ' "$source"
}

# The first line of a split section is the header it was keyed on.
header_of() {
    head -n 1 "$1"
}

count_sections() {
    grep -c '^# input=' "$1" || true
}

split_sections "$baseline" baseline
split_sections "$new" new

status=0

# A key present on only one side: report it before the content diff, because
# `diff -r` describes it as a file addition/removal rather than a data change.
for file in "$work/baseline"/*.txt; do
    [ -e "$file" ] || continue
    name=$(basename "$file")
    if [ ! -e "$work/new/$name" ]; then
        echo "only in baseline: $(header_of "$file")" >&2
        status=1
    fi
done
for file in "$work/new"/*.txt; do
    [ -e "$file" ] || continue
    name=$(basename "$file")
    if [ ! -e "$work/baseline/$name" ]; then
        echo "only in new: $(header_of "$file")" >&2
        status=1
    fi
done

# Content diff for the sections both sides have.
if ! diff -r -u "$work/baseline" "$work/new"; then
    status=1
fi

if [ "$status" -eq 0 ]; then
    echo "compare-digest: identical ($(count_sections "$baseline") sections)"
fi
exit "$status"
