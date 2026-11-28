//! The Linux userland the host-toolchain round trips compile and run in.
//!
//! A round trip decompiles a fixture with kuna, then builds the printed C with
//! `cc`/`clang` and runs it. That check means "this C computes what the binary
//! computes" only under the Linux x86-64 rules CI builds with: an 8-byte
//! `long`, the System V calling convention, glibc. A Windows toolchain (MinGW,
//! clang with `lld-link`) builds the same C with a 4-byte `long` and the Win64
//! convention, so it reports miscompiles that are not there.
//!
//! On Windows, [`route`] therefore runs a Linux toolchain, and any Linux
//! program a round trip built, inside WSL. kuna itself still runs natively; only
//! the check moves. A command is routed when its program is a bare toolchain
//! name ([`TOOLS`]) or a file that starts with the ELF magic. Every other
//! command, and every command on any other host, is left as it is.
//!
//! A tool the WSL distribution lacks, or a host without WSL, becomes a spawn
//! that fails with `NotFound`, the way a missing tool fails on Linux, so the
//! round trips' existing "optional toolchain" skips apply unchanged.
//!
//! `KUNA_TEST_WSL_DISTRO` names the distribution; unset, WSL's default is used.

use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

/// The Linux toolchain programs a round trip may run by bare name.
const TOOLS: &[&str] = &[
    "cc", "c++", "gcc", "g++", "clang", "clang++", "gfortran", "ld", "ld.lld", "as", "ar", "nm",
    "objcopy", "objdump", "readelf", "strip",
];

/// Rewrite `command` to run inside WSL when it is a Linux toolchain or a Linux
/// program (see the module docs); otherwise leave it untouched. Stdio set
/// before the call is not carried over, so route a command before setting it.
///
/// Go stays native because it cross-compiles on its own: a `go build` gets
/// `GOOS=linux`, so the fixture it makes is the Linux binary CI decompiles,
/// and a `go run` of printed Go is left as it is.
pub fn route(command: &mut Command) {
    if !cfg!(windows) {
        return;
    }
    let program = command.get_program().to_string_lossy().into_owned();
    if program == "go" {
        let build = command.get_args().next().is_some_and(|arg| arg == "build");
        if build && !command.get_envs().any(|(key, _)| key == "GOOS") {
            command.env("GOOS", "linux");
        }
        return;
    }
    let tool = is_tool(&program);
    if !tool && !is_elf(Path::new(&program)) {
        return;
    }
    let mut routed = wsl();
    if let Some(dir) = command.get_current_dir() {
        routed.arg("--cd").arg(dir);
    }
    routed.arg("-e");
    let envs: Vec<_> = command.get_envs().collect();
    if !envs.is_empty() {
        routed.arg("env");
        for (key, value) in envs {
            let key = key.to_string_lossy();
            match value {
                Some(value) => routed.arg(format!("{key}={}", to_linux(&value.to_string_lossy()))),
                None => routed.args(["-u", &key]),
            };
        }
    }
    if tool && !available(&program) {
        // Spawning a program that does not exist fails with `NotFound`.
        *command = Command::new(format!("kuna-test-wsl-lacks-{program}"));
        return;
    }
    routed.arg(to_linux(&program));
    routed.args(command.get_args().map(|arg| to_linux(&arg.to_string_lossy())));
    *command = routed;
}

/// A Linux toolchain program by bare name: one of [`TOOLS`], or a cross tool
/// whose triple targets Linux or MinGW (`x86_64-w64-mingw32-gcc`,
/// `arm-linux-gnueabihf-ld`), which CI runs from its Linux packages too. A
/// bare-metal one (`arm-none-eabi-gcc`) builds the same on any host and stays.
///
/// So do a versioned name (`clang-14`, `ld.lld-21`), the LLVM binutils
/// (`llvm-mc`) and the `qemu-*` user emulators: the replays that need one find
/// it only on the Windows PATH, and skip there otherwise.
fn is_tool(program: &str) -> bool {
    if TOOLS.contains(&program) {
        return true;
    }
    let Some((triple, tool)) = program.rsplit_once('-') else { return false };
    (triple.contains("-linux-") || triple.ends_with("-linux") || triple.contains("-mingw32"))
        && TOOLS.contains(&tool)
}

/// `wsl.exe`, on the requested distribution.
fn wsl() -> Command {
    let mut wsl = Command::new("wsl.exe");
    if let Some(distro) = std::env::var_os("KUNA_TEST_WSL_DISTRO").filter(|d| !d.is_empty()) {
        wsl.arg("-d").arg(distro);
    }
    wsl
}

/// Does `path` name a file that starts with the ELF magic?
fn is_elf(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 4];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut magic)).is_ok()
        && magic == *b"\x7fELF"
}

/// Is `tool` on the WSL distribution's PATH? Asked once per tool and process;
/// a host without WSL has none of them.
fn available(tool: &str) -> bool {
    static KNOWN: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    let mut known = KNOWN.get_or_init(Default::default).lock().unwrap();
    *known.entry(tool.to_owned()).or_insert_with(|| {
        let found = succeeds(wsl().args(["-e", "sh", "-c", &format!("command -v {tool}")]));
        if !found {
            eprintln!("note: `{tool}` is not available in WSL, so tests that need it skip");
        }
        found
    })
}

/// Can this host run a Linux program a round trip built? Always, off Windows;
/// on Windows only when WSL does, asked once per process. A missing toolchain
/// already skips a round trip, so this is for one whose compiler is native
/// (Go) and that runs the Linux binary it built.
pub fn runs_programs() -> bool {
    static RUNS: OnceLock<bool> = OnceLock::new();
    !cfg!(windows) || *RUNS.get_or_init(|| succeeds(wsl().args(["-e", "true"])))
}

/// Does `command` exit successfully? Its output is discarded.
fn succeeds(command: &mut Command) -> bool {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Where WSL mounts the Windows drives (`/mnt/`, unless `/etc/wsl.conf` moved
/// it), asked once per process.
fn mount_root() -> &'static str {
    static ROOT: OnceLock<String> = OnceLock::new();
    ROOT.get_or_init(|| {
        let asked = wsl().args(["-e", "wslpath", "-u", r"C:\"]).stdin(Stdio::null()).output();
        asked
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                path.strip_suffix("c/").or(path.strip_suffix("c")).map(str::to_owned)
            })
            .unwrap_or_else(|| "/mnt/".to_owned())
    })
}

/// `arg` with the Windows drive path in it (`C:\x\y`, `C:/x/y`, or the
/// verbatim `\\?\C:\x\y` that `canonicalize` returns) spelled as WSL sees it
/// (`/mnt/c/x/y`). The path starts the argument or follows a non-alphanumeric
/// character or a one-letter flag (`-IC:\x`, `-Wl,-T,C:\x`) and runs to the
/// argument's end. Anything else is returned as it is.
pub fn to_linux(arg: &str) -> String {
    let plain;
    let arg = match arg.find(r"\\?\") {
        Some(k) if arg.as_bytes().get(k + 5) == Some(&b':') => {
            plain = format!("{}{}", &arg[..k], &arg[k + 4..]);
            plain.as_str()
        }
        _ => arg,
    };
    let b = arg.as_bytes();
    let starts_path = |i: usize| {
        b.len() > i + 2
            && b[i].is_ascii_alphabetic()
            && b[i + 1] == b':'
            && matches!(b[i + 2], b'\\' | b'/')
            && (i == 0 || !b[i - 1].is_ascii_alphanumeric() || (i == 2 && b[0] == b'-'))
    };
    match (0..b.len()).find(|&i| starts_path(i)) {
        None => arg.to_owned(),
        Some(i) => format!(
            "{}{}{}/{}",
            &arg[..i],
            mount_root(),
            (b[i] as char).to_ascii_lowercase(),
            arg[i + 3..].replace('\\', "/")
        ),
    }
}

/// `path` as C source built on the Linux host must spell it in an
/// `#include "..."`: the WSL spelling on Windows, the path itself elsewhere.
/// (A header name is not a string literal, so nothing is escaped.)
pub fn c_path(path: impl AsRef<Path>) -> String {
    let path = path.as_ref().to_string_lossy();
    if cfg!(windows) { to_linux(&path) } else { path.into_owned() }
}

/// `go build`, making a binary for the Linux host: `GOOS=linux` on Windows,
/// the host's own target elsewhere. Go cross-compiles, so it runs natively.
pub fn go_build() -> Command {
    let mut go = Command::new("go");
    go.arg("build");
    if cfg!(windows) {
        go.env("GOOS", "linux");
    }
    go
}

/// [`route`] as a builder step, for a command spawned without the capped
/// helpers (which route on their own): `Command::new("cc").arg(..).on_linux()`.
/// It must come after the arguments, directory and environment, and before
/// any stdio.
pub trait OnLinux {
    fn on_linux(&mut self) -> &mut Self;
}

impl OnLinux for Command {
    fn on_linux(&mut self) -> &mut Self {
        route(self);
        self
    }
}
