#!/usr/bin/env bash
# Record the failure behaviour of the CLI for inputs that must not succeed.
#
# The point is a stable, diffable report. FFmpeg writes the address of the
# object it is complaining about into every diagnostic (`[matroska,webm @
# 000002bc63026700]`), and that address changes from run to run, so it is
# replaced with `ADDR` before the report is printed. Everything else -- the exit
# code, the error class, and the wording -- is left exactly as produced.
#
# Usage: scripts/check-error-semantics.sh <ihni-binary> <fixture-dir>
#
# The report goes to stdout, so a run can be compared against a recorded one:
#
#     scripts/check-error-semantics.sh target/release/ihni target/m1-fixtures \
#         > target/error-semantics-now.txt
#     diff target/error-semantics-before.txt target/error-semantics-now.txt

set -uo pipefail

if [ "$#" -ne 2 ]; then
    echo "usage: scripts/check-error-semantics.sh <ihni-binary> <fixture-dir>" >&2
    exit 2
fi

binary=$1
fixtures=$2

if [ ! -x "$binary" ]; then
    echo "check-error-semantics: not an executable: $binary" >&2
    exit 2
fi

# Each entry is "<fixture> [extra arguments...]".
#
# `multi-stream.mp4 --stream-index 1` targets a stream that exists but is not
# video, which is a different rejection from a stream that is absent. The index
# is a property of the fixture written by scripts/make-m1-fixtures.sh.
cases=(
    "empty.bin"
    "invalid.bin"
    "audio-only.m4a"
    "damaged.mkv"
    "truncated.mkv"
    "cut8.mkv --stream-index 7"
    "multi-stream.mp4 --stream-index 1"
)

mask_addresses() {
    sed -E 's/@ [0-9a-f]{8,}/@ ADDR/g'
}

for entry in "${cases[@]}"; do
    # shellcheck disable=SC2086 # the entry is a deliberate word list
    set -- $entry
    name=$1
    shift

    path="$fixtures/$name"
    if [ ! -f "$path" ]; then
        echo "--- $name (ABSENT: $path)"
        continue
    fi

    echo "--- $name $*"
    # `--no-progress` keeps the periodic display out of the record: the report
    # exists to diff the exit code and the error wording, and a redirect-capturing
    # run would otherwise gain a progress line whenever the input decoded at
    # least one frame before failing. The final error message is unaffected.
    output=$("$binary" --no-progress "$path" "$@" 2>&1)
    status=$?
    printf 'exit=%d\n' "$status"
    printf '%s\n' "$output" | mask_addresses
done
