#![allow(dead_code)]

pub mod linux_host;
pub mod process;

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_SCRATCH_ID: AtomicU64 = AtomicU64::new(0);

/// GCC 14/15 (Ubuntu 26.04's default `cc`) promote these C diagnostics to
/// errors by default, and `-w` demotes warnings, not errors. Decompiler output
/// routinely reuses a word as both integer and pointer, so append these to any
/// `cc` invocation that compiles emitted C to keep it a warning there. The
/// negative warning form for GCC 15's return-mismatch is also accepted by older
/// GCC, whose unknown `-Wno-error=...` form would instead reject the invocation.
pub const CC_GCC15_DEMOTE: &[&str] = &[
    "-Wno-error=int-conversion",
    "-Wno-error=incompatible-pointer-types",
    "-Wno-error=implicit-function-declaration",
    "-Wno-error=implicit-int",
    "-Wno-return-mismatch",
];

/// How a child's `ExitStatus` reads after it panicked (exit code 101): Rust
/// spells it `exit status: 101` on Unix and `exit code: 101` on Windows.
pub const PANIC_EXIT: &str = if cfg!(windows) { "exit code: 101" } else { "exit status: 101" };

/// How a child's `ExitStatus` reads after it aborted: `SIGABRT` on Unix, and
/// on Windows the `STATUS_STACK_BUFFER_OVERRUN` code a Rust abort exits with.
pub const ABORT_EXIT: &str = if cfg!(windows) { "exit code: 0xc0000409" } else { "signal: 6" };

/// The repository root, canonical. On Windows `canonicalize` returns the
/// verbatim form (`\\?\C:\...`), whose doubled leading backslash a quoted
/// console argument reads as one escaped backslash; the root is returned in
/// its plain form (`C:\...`) instead, which every reader takes as it is.
pub fn repo_root() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    match root.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(plain) if plain.as_bytes().get(1) == Some(&b':') => PathBuf::from(plain),
        _ => root,
    }
}

pub fn fixture(name: &str) -> String {
    repo_root()
        .join("decompiler/crates/kuna-analysis/tests/fixtures")
        .join(name)
        .to_str()
        .unwrap()
        .to_owned()
}

pub fn run_kuna(args: &[&str]) -> (String, String, i32) {
    let output = Command::new(env!("CARGO_BIN_EXE_kuna"))
        .args(args)
        .output()
        .expect("spawn kuna");
    (
        String::from_utf8(output.stdout).expect("UTF-8 stdout"),
        String::from_utf8(output.stderr).expect("UTF-8 stderr"),
        output.status.code().expect("kuna exited without a signal"),
    )
}

/// A unique generated-fixture path under cargo's own per-target scratch
/// directory, which is ignored and cleaned with the build.
pub fn scratch_file(stem: &str, extension: &str) -> PathBuf {
    let scratch_root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("kuna-cli");
    std::fs::create_dir_all(&scratch_root).expect("create test scratch directory");
    let id = NEXT_SCRATCH_ID.fetch_add(1, Ordering::Relaxed);
    scratch_root.join(format!("{stem}-{}-{id}.{extension}", std::process::id()))
}
