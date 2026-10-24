#!/usr/bin/env bash
# Fetch the pinned FFmpeg development prefix and print the FFMPEG_DIR value.
#
# The project does not build FFmpeg and does not bundle it. It links the static
# libraries from a prefix that `ihni-media/build.rs` names in FFMPEG_DIR, and
# this script produces that prefix from the pinned GenKFR release asset.
#
# The download is verified before it is used: the asset's sha256 and byte count
# are recorded here, and a mismatch is a hard failure. A truncated download, a
# replaced asset, or a redirect to something else must not become a build that
# links unknown code.
#
# The archive is extracted into its own empty directory. A prefix that already
# exists is not merged into, because a partial or stale tree would link without
# complaining and would be very hard to recognise later.
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
#
# Publishing an IHNI-owned asset is later work; this reuses the GenKFR release.

set -euo pipefail

readonly REPOSITORY="Nemuboshi/GenKFR"
readonly TAG="ffmpeg-libs-9.0.2"
readonly ASSET="ffmpeg-libs-9.0.2.zip"
readonly ASSET_SHA256="d6de00b8f68694c50f29cfbc2ec9c466b4ba80f90f523efd20fc661174833649"
readonly ASSET_SIZE=80867368

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
            sed -n '2,32p' "$0" | sed 's/^# \{0,1\}//'
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

# Downloading is skipped when the archive is already present and correct, so a
# repeated run costs nothing. It is re-downloaded when it is missing or wrong.
if [ -f "$archive" ] && [ "$(wc -c <"$archive" | tr -d ' ')" = "$ASSET_SIZE" ]; then
    echo "fetch-ffmpeg-libs: reusing $archive" >&2
else
    echo "fetch-ffmpeg-libs: downloading $REPOSITORY $TAG/$ASSET" >&2
    rm -f "$archive"
    gh release download "$TAG" --repo "$REPOSITORY" --pattern "$ASSET" --dir "$DOWNLOAD_DIR"
fi

# Verify before extracting. The size check catches a truncated transfer, and the
# hash catches everything else.
actual_size=$(wc -c <"$archive" | tr -d ' ')
if [ "$actual_size" != "$ASSET_SIZE" ]; then
    echo "fetch-ffmpeg-libs: $archive is $actual_size bytes, expected $ASSET_SIZE" >&2
    exit 1
fi

if command -v sha256sum >/dev/null 2>&1; then
    actual_sha256=$(sha256sum "$archive" | cut -d' ' -f1)
elif command -v shasum >/dev/null 2>&1; then
    actual_sha256=$(shasum -a 256 "$archive" | cut -d' ' -f1)
else
    echo "fetch-ffmpeg-libs: neither sha256sum nor shasum is available" >&2
    exit 1
fi

if [ "$actual_sha256" != "$ASSET_SHA256" ]; then
    echo "fetch-ffmpeg-libs: sha256 mismatch for $archive" >&2
    echo "  expected $ASSET_SHA256" >&2
    echo "  actual   $actual_sha256" >&2
    echo "The asset is not the pinned one; refusing to extract it." >&2
    exit 1
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
