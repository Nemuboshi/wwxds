#!/usr/bin/env bash
# Fetch the FFmpeg development prefix and print the FFMPEG_DIR value.
#
# The project does not build FFmpeg and does not bundle it. It links the static
# libraries from a prefix that `wwxds-media/build.rs` names in FFMPEG_DIR, and
# this script produces that prefix from this repository's `ffmpeg-libs-*`
# release, published by the `ffmpeg-libs.yml` workflow.
#
# The source is the tag and asset name recorded below. The tag prefix
# `ffmpeg-libs-` is reserved for these build inputs and is kept apart from
# program releases; the release itself is marked as a pre-release.
#
# Usage:
#
#     bash scripts/fetch-ffmpeg-libs.sh [--force]
#
# `--force` replaces an existing prefix. Without it, an existing prefix is
# reused and only its presence is checked.
#
# The value printed on standard output is the one to export:
#
#     export FFMPEG_DIR="$(bash scripts/fetch-ffmpeg-libs.sh)"

set -euo pipefail

readonly REPOSITORY="Nemuboshi/wwxds"
readonly TAG="ffmpeg-libs-9.0.2"
readonly ASSET="ffmpeg-libs-9.0.2.zip"

# Both paths are inside the gitignored target directory, and they are separate:
# the archive is downloaded to one directory and extracted into another, so a
# planted file in the archive cannot land next to the script that runs.
readonly DOWNLOAD_DIR="target/ffmpeg-download"
readonly PREFIX_DIR="target/ffmpeg-libs"

force=0
for argument in "$@"; do
    case "$argument" in
        --force) force=1 ;;
        -h | --help)
            sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "fetch-ffmpeg-libs: unknown argument '$argument'" >&2
            exit 2
            ;;
    esac
done

# The prefix is reusable only when it looks complete. A directory that merely
# exists is not enough: an interrupted extraction leaves one behind.
prefix_is_complete() {
    local prefix=$1
    [ -d "$prefix/include" ] &&
        [ -d "$prefix/lib" ] &&
        [ -f "$prefix/include/libavcodec/avcodec.h" ] &&
        [ -f "$prefix/include/libavformat/avformat.h" ] &&
        [ -f "$prefix/include/libavutil/avutil.h" ] &&
        [ -f "$prefix/lib/avcodec.lib" ] &&
        [ -f "$prefix/lib/avformat.lib" ] &&
        [ -f "$prefix/lib/avutil.lib" ]
}

if [ "$force" -eq 0 ] && prefix_is_complete "$PREFIX_DIR"; then
    echo "fetch-ffmpeg-libs: reusing $PREFIX_DIR" >&2
    printf '%s\n' "$PREFIX_DIR"
    exit 0
fi

if ! command -v gh >/dev/null 2>&1; then
    echo "fetch-ffmpeg-libs: the GitHub CLI 'gh' is required" >&2
    exit 1
fi

mkdir -p "$DOWNLOAD_DIR"
archive="$DOWNLOAD_DIR/$ASSET"

# Downloading is skipped when the archive is already present, so a repeated run
# costs nothing. Extraction and the completeness check below reject a broken
# or partial download.
if [ -f "$archive" ]; then
    echo "fetch-ffmpeg-libs: reusing $archive" >&2
else
    echo "fetch-ffmpeg-libs: downloading $REPOSITORY $TAG/$ASSET" >&2
    rm -f "$archive"
    gh release download "$TAG" --repo "$REPOSITORY" --pattern "$ASSET" --dir "$DOWNLOAD_DIR"
fi

if ! command -v unzip >/dev/null 2>&1; then
    echo "fetch-ffmpeg-libs: 'unzip' is required" >&2
    exit 1
fi

# Extract. Git for Windows usually ships `unzip`, but it is not guaranteed on a
# minimal runner; Windows' own `tar.exe` (bsdtar) reads zip archives as well.
# GNU tar, which is what `tar` resolves to inside Git Bash, does not, so the
# system tar is named explicitly rather than looked up on PATH.
extract() {
    local archive=$1
    local destination=$2
    if command -v unzip >/dev/null 2>&1; then
        unzip -q "$archive" -d "$destination"
        return
    fi
    if [ -x /c/Windows/System32/tar.exe ]; then
        /c/Windows/System32/tar.exe -xf "$archive" -C "$destination"
        return
    fi
    echo "fetch-ffmpeg-libs: neither 'unzip' nor the Windows 'tar.exe' is available" >&2
    exit 1
}

# Extract into the prefix directory itself, after removing whatever is there.
#
# Extracting in place rather than into a staging directory and renaming avoids a
# directory rename, which on Windows fails while any process holds a handle
# anywhere in the tree — including a shell whose current directory is inside it.
# The completeness check below is what makes this safe: a prefix that exists but
# is incomplete is never reused, so an interrupted extraction is re-done by the
# next run rather than linked against.
rm -rf "$PREFIX_DIR"
mkdir -p "$PREFIX_DIR"
echo "fetch-ffmpeg-libs: extracting to $PREFIX_DIR" >&2
extract "$archive" "$PREFIX_DIR"

if ! prefix_is_complete "$PREFIX_DIR"; then
    echo "fetch-ffmpeg-libs: the extracted tree is missing expected files" >&2
    find "$PREFIX_DIR" -maxdepth 2 -mindepth 1 -print | sort >&2
    exit 1
fi

echo "fetch-ffmpeg-libs: $PREFIX_DIR is ready" >&2
printf '%s\n' "$PREFIX_DIR"
