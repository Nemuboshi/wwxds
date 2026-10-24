#!/usr/bin/env bash
# Check that the release binary links no FFmpeg DLL, and that it does contain
# the statically linked swscale.
#
# The FFmpeg libraries are static, so a DLL import would mean a shared prefix was
# linked by mistake; that stays a hard failure for every FFmpeg library
# including swscale.
#
# swscale is the only resize path since the migration, and the parity-oracle
# feature and the retired Rust resampler were removed with task 5, so every
# release binary must embed swscale code: the image probe is now a hard
# assertion, not a report. The oracle build used to be the one configuration
# that could produce a binary whose resize path was the Rust scaler, so the
# probe only reported; with the oracle gone there is no supported build that
# legitimately lacks swscale. The check that tests cannot see — accidental
# dynamic dependencies — stays here too.
#
# Usage: scripts/check-link-gate.sh [path-to-binary]
#
# Exit status: 0 when both checks pass, 1 when either fails, 2 on a setup
# problem (no binary, no dumpbin).

set -uo pipefail

binary=${1:-target/release/ihni.exe}

if [ ! -f "$binary" ]; then
    echo "check-link-gate: no such file: $binary" >&2
    exit 2
fi

# dumpbin is not on PATH on a plain Windows shell, and its location depends on
# the Visual Studio edition and version. vswhere is the supported way to find
# it; the glob is a fallback for a machine without vswhere.
find_dumpbin() {
    local vswhere="/c/Program Files (x86)/Microsoft Visual Studio/Installer/vswhere.exe"
    if [ -x "$vswhere" ]; then
        local root
        root=$("$vswhere" -latest -products '*' \
            -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 \
            -property installationPath 2>/dev/null | tr -d '\r')
        if [ -n "$root" ] && [ -d "$root" ]; then
            local found
            found=$(find "$root/VC/Tools/MSVC" -type f -name dumpbin.exe \
                -path '*/Hostx64/x64/*' 2>/dev/null | sort | tail -n 1)
            if [ -n "$found" ]; then
                printf '%s\n' "$found"
                return 0
            fi
        fi
    fi
    for candidate in /c/Program\ Files/Microsoft\ Visual\ Studio/*/*/VC/Tools/MSVC/*/bin/Hostx64/x64/dumpbin.exe; do
        [ -f "$candidate" ] || continue
        printf '%s\n' "$candidate"
        return 0
    done
    return 1
}

if ! dumpbin=$(find_dumpbin); then
    echo "check-link-gate: could not locate dumpbin.exe" >&2
    exit 2
fi
echo "check-link-gate: using $dumpbin" >&2

# MSYS2 would rewrite the `/dependents`-style flags as paths. The tool is a
# native Windows program, so argument conversion is disabled for these calls.
dependents=$(MSYS2_ARG_CONV_EXCL='*' "$dumpbin" -dependents "$binary" 2>&1)
status=0

if printf '%s\n' "$dependents" | grep -Eiq 'avcodec|avformat|avutil|swscale|swresample'; then
    printf '%s\n' "$dependents" >&2
    echo "check-link-gate: the binary imports an FFmpeg DLL; the libraries must be static" >&2
    status=1
fi

symbols=$(MSYS2_ARG_CONV_EXCL='*' "$dumpbin" -symbols "$binary" 2>&1)
swscale_symbols=$(printf '%s\n' "$symbols" | grep -ci 'sws_' || true)
# A release MSVC image carries no COFF symbol table (the symbols live in the
# accompanying .pdb), so the symbol grep reads zero on such a build whatever
# was linked. The embedded text is the probe that actually distinguishes:
# swscale's own build paths and log text appear in every image that links it,
# and every release binary must now link it, so zero matches is a failure.
# The probe greps the image bytes directly with grep (-a: treat binary as
# text; -o: one match per line, so the second grep counts occurrences and
# not merely lines) because Git-for-Windows bash does not ship `strings` on
# GitHub runners, while GNU grep is guaranteed there and in this repository's
# documented bash toolchain.
swscale_matches=$(LC_ALL=C grep -ao 'sws_[A-Za-z0-9_]*\|swscale' "$binary" | grep -c . || true)
echo "check-link-gate: swscale symbols in $binary: $swscale_symbols (image matches: $swscale_matches)"
if [ "$swscale_matches" -eq 0 ]; then
    echo "check-link-gate: the binary contains no swscale code; the production resize path requires it statically linked" >&2
    status=1
fi

if [ "$status" -eq 0 ]; then
    echo "check-link-gate: pass (no FFmpeg DLL; swscale embedded: $swscale_matches image matches)"
fi
exit "$status"
