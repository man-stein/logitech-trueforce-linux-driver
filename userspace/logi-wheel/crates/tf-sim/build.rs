// SPDX-License-Identifier: GPL-2.0-only
//! Build glue for the libtrueforce FFI, plus the G923 init-data codegen.
//!
//! Links the in-repo `userspace/libtrueforce` static archive. If the
//! archive is absent (fresh checkout, CI), it is built via the library's
//! own Makefile so there is exactly one authoritative build recipe.
//! Static linking is preferred: the shipped `logi-tf-sim` binary then has
//! no runtime dependency on `libtrueforce.so`.
//!
//! libtrueforce does not recognize the G923 (its `is_supported_wheel`
//! table is RS50-family only), so [`crate::g923`] talks to it directly
//! over hidraw rather than through the FFI. It still needs the exact
//! 68-packet TrueForce init sequence libtrueforce's `stream.c`/`session.c`
//! send, so rather than hand-copying those 68*64 bytes into a second Rust
//! source of truth, this build script parses them straight out of
//! `tf_init_data.h` into a generated Rust array the crate `include!`s.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Number of `0x..` byte rows [`generate_g923_init_data`] expects to find;
/// a mismatch means the header changed shape and the parser needs a look,
/// so this fails the build rather than silently emitting a short array.
const EXPECTED_PACKET_COUNT: usize = 68;
const PACKET_LEN: usize = 64;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let lib_dir = manifest
        .join("../../../libtrueforce")
        .canonicalize()
        .expect("userspace/libtrueforce not found relative to the tf-sim crate");
    let archive = lib_dir.join("libtrueforce.a");

    if !archive.exists() {
        let status = Command::new("make")
            .arg("-C")
            .arg(&lib_dir)
            .arg("libtrueforce.a")
            .status()
            .expect("failed to run make for libtrueforce");
        assert!(status.success(), "make -C {} libtrueforce.a failed", lib_dir.display());
    }

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=trueforce");
    // libtrueforce uses pthreads (stream thread, mutexes). glibc >= 2.34
    // folds pthread into libc, but older toolchains still need the flag.
    println!("cargo:rustc-link-lib=pthread");

    println!("cargo:rerun-if-changed={}", lib_dir.join("include/trueforce.h").display());
    println!("cargo:rerun-if-changed={}", lib_dir.join("Makefile").display());
    for src in ["discovery.c", "exports.c", "kf.c", "session.c", "status.c", "stream.c", "sysfs.c", "internal.h", "tf_init_data.h"] {
        println!("cargo:rerun-if-changed={}", lib_dir.join("src").join(src).display());
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    generate_g923_init_data(&lib_dir.join("src").join("tf_init_data.h"), &out_dir);
}

/// Parse the `{ 0x.., 0x.., ... },` packet rows out of `tf_init_data.h` and
/// emit `g923_init_data.rs` (a `TF_INIT_PACKETS: [[u8; 64]; 68]` plus its
/// count/len constants) into `out_dir`. The header's rows are simple enough
/// (one packet per line, only hex byte literals between the braces, no
/// nested braces) that a tiny line-oriented parser is enough; this is not a
/// general C-array parser.
fn generate_g923_init_data(header_path: &Path, out_dir: &Path) {
    let text = std::fs::read_to_string(header_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", header_path.display()));

    let mut packets: Vec<[u8; PACKET_LEN]> = Vec::new();
    for line in text.lines() {
        if !line.contains("0x") {
            continue;
        }
        let (Some(start), Some(end)) = (line.find('{'), line.rfind('}')) else { continue };
        let bytes: Vec<u8> = line[start + 1..end]
            .split(',')
            .map(str::trim)
            .filter(|tok| !tok.is_empty())
            .map(|tok| {
                u8::from_str_radix(tok.trim_start_matches("0x"), 16)
                    .unwrap_or_else(|e| panic!("bad byte literal '{tok}' in {}: {e}", header_path.display()))
            })
            .collect();
        assert_eq!(
            bytes.len(),
            PACKET_LEN,
            "packet row in {} has {} bytes, expected {PACKET_LEN}: {line}",
            header_path.display(),
            bytes.len()
        );
        let mut row = [0u8; PACKET_LEN];
        row.copy_from_slice(&bytes);
        packets.push(row);
    }
    assert_eq!(
        packets.len(),
        EXPECTED_PACKET_COUNT,
        "parsed {} init packets from {}, expected {EXPECTED_PACKET_COUNT}",
        packets.len(),
        header_path.display()
    );

    let mut out = String::new();
    out.push_str(&format!("pub const TF_INIT_PACKET_COUNT: usize = {EXPECTED_PACKET_COUNT};\n"));
    out.push_str(&format!("pub const TF_INIT_PACKET_LEN: usize = {PACKET_LEN};\n"));
    out.push_str(&format!(
        "pub static TF_INIT_PACKETS: [[u8; {PACKET_LEN}]; {EXPECTED_PACKET_COUNT}] = [\n"
    ));
    for row in &packets {
        out.push('[');
        for (i, b) in row.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("0x{b:02x}"));
        }
        out.push_str("],\n");
    }
    out.push_str("];\n");

    std::fs::write(out_dir.join("g923_init_data.rs"), out).expect("write g923_init_data.rs");
}
