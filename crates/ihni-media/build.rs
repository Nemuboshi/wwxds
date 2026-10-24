//! Builds the C shim and links the static FFmpeg libraries.
//!
//! The libraries come from a dev prefix named by `FFMPEG_DIR`. The project does
//! not bundle them; `scripts/fetch-ffmpeg-libs.sh` downloads and verifies the
//! pinned GenKFR asset and prints the value to use.
//!
//! The pinned asset is a vcpkg `x64-windows-static-md-release` build: static
//! libraries against the **dynamic** C runtime. Building with `+crt-static`
//! would produce a binary that cannot link, so that configuration is rejected
//! here rather than failing later with an unexplained unresolved symbol.

use std::env;
use std::path::{Path, PathBuf};

/// FFmpeg libraries this crate uses, in no particular order.
///
/// `avcodec` depends on `swresample`, so it is linked even though the shim
/// never calls it directly. `swscale` is the production resize path: the shim's
/// scaler entry points compile and link in every build.
const FFMPEG_LIBS: [&str; 5] = ["avformat", "avcodec", "swscale", "swresample", "avutil"];

/// System libraries FFmpeg's static archives reference on Windows.
///
/// This is the list FFMS2's Visual Studio project documents, which is also what
/// the pinned asset was built and tested against.
const WINDOWS_SYSTEM_LIBS: [&str; 16] = [
    "bcrypt", "ncrypt", "ws2_32", "secur32", "mfplat", "mfuuid", "strmiids", "ole32", "user32",
    "crypt32", "psapi", "uuid", "oleaut32", "shlwapi", "gdi32", "vfw32",
];

fn main() {
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
    println!("cargo:rerun-if-changed=csrc/ihni_ffmpeg.c");

    let target = env::var("TARGET").expect("Cargo always sets TARGET");
    if target != "x86_64-pc-windows-msvc" {
        panic!(
            "ihni-media links a Windows x64 MSVC FFmpeg build; target '{target}' is not \
             supported. Set TARGET or build on Windows with the MSVC toolchain."
        );
    }

    let features = env::var("CARGO_CFG_TARGET_FEATURE").unwrap_or_default();
    if features.split(',').any(|feature| feature == "crt-static") {
        panic!(
            "the pinned FFmpeg libraries are linked against the dynamic C runtime, so \
             '+crt-static' is unsupported; build without it"
        );
    }

    let directory = ffmpeg_dir();
    let include = directory.join("include");
    for header in [
        "libavutil/avutil.h",
        "libavcodec/avcodec.h",
        "libavformat/avformat.h",
        // The resize path is compiled in every build, so its header is always
        // required.
        "libswscale/swscale.h",
    ] {
        assert!(
            include.join(header).is_file(),
            "{} does not contain {header}",
            directory.display()
        );
    }

    let mut build = cc::Build::new();
    build
        .file("csrc/ihni_ffmpeg.c")
        .include(&include)
        .warnings(true);
    build.compile("ihni_ffmpeg");

    println!(
        "cargo:rustc-link-search=native={}",
        directory.join("lib").display()
    );
    for library in FFMPEG_LIBS {
        println!("cargo:rustc-link-lib=static={library}");
    }
    println!(
        "cargo:rustc-link-lib=static={}",
        zlib_name(&directory.join("lib"))
    );
    for library in WINDOWS_SYSTEM_LIBS {
        println!("cargo:rustc-link-lib={library}");
    }
}

/// The FFmpeg dev prefix.
fn ffmpeg_dir() -> PathBuf {
    let Some(value) = env::var_os("FFMPEG_DIR") else {
        panic!(
            "FFMPEG_DIR is not set. Run 'bash scripts/fetch-ffmpeg-libs.sh' and export the \
             FFMPEG_DIR value it prints."
        );
    };
    let directory = PathBuf::from(value);
    assert!(
        directory.join("include").is_dir() && directory.join("lib").is_dir(),
        "FFMPEG_DIR must hold include/ and lib/: {}",
        directory.display()
    );
    directory
}

/// Names the zlib archive the prefix actually has.
///
/// vcpkg's static triplets ship `zs.lib` since zlib 1.3.2; `z.lib` is the DLL
/// import library. Older prefixes use `zlib.lib` or `z.lib`.
fn zlib_name(lib: &Path) -> &'static str {
    for name in ["zs", "zlib", "z"] {
        if lib.join(format!("{name}.lib")).is_file() {
            return name;
        }
    }
    panic!(
        "no zlib static library found in {}: expected zs.lib, zlib.lib, or z.lib",
        lib.display()
    );
}
