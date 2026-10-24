//! Build script for the WWXD reference-vector oracle.
//!
//! Compiles the pinned `detection.c` with `reference.c` and enforces two gates: the
//! reference must be checked out at the documented commit, and `detection.c` /
//! `detection.h` must be unmodified. The oracle therefore cannot silently drift
//! from the control it is meant to reproduce.

use std::path::PathBuf;
use std::process::Command;

const REFERENCE_COMMIT: &str = "a5870862fd85138b5e4822a076377b96592c9cd2";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/wwxd-reference/src");
    let revision = Command::new("git")
        .arg("-C")
        .arg(&source)
        .args(["rev-parse", "HEAD"])
        .output()?;
    if !revision.status.success() || String::from_utf8(revision.stdout)?.trim() != REFERENCE_COMMIT
    {
        return Err(
            "WWXD reference must be cloned at the documented commit a5870862 before building the oracle"
                .into(),
        );
    }
    let clean = Command::new("git")
        .arg("-C")
        .arg(&source)
        .args([
            "diff",
            "--quiet",
            "HEAD",
            "--",
            "detection.c",
            "detection.h",
        ])
        .status()?;
    if !clean.success() {
        return Err("WWXD reference detection files differ from the pinned source".into());
    }

    println!("cargo:rustc-env=WWXD_REFERENCE_COMMIT={REFERENCE_COMMIT}");
    println!("cargo:rerun-if-changed=reference.c");
    println!("cargo:rerun-if-changed=VSHelper.h");
    println!("cargo:rerun-if-changed={}", source.display());

    cc::Build::new()
        .file(source.join("detection.c"))
        .file("reference.c")
        .include(source)
        .include(".")
        .std("c11")
        .compile("wwxd_reference");
    Ok(())
}
