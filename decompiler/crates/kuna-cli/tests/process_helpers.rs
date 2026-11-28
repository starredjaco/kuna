mod common;

use std::io::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::process::Command;
use std::time::{Duration, Instant};

use common::process;

const CHILD_MODE: &str = "KUNA_CLI_PROCESS_TEST_MODE";
const PIPE_BYTES: usize = 512 * 1024;

fn child(mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "captures_both_pipes_without_deadlock",
            "--nocapture",
        ])
        .env(CHILD_MODE, mode);
    command
}

#[test]
fn captures_both_pipes_without_deadlock() {
    if let Ok(mode) = std::env::var(CHILD_MODE) {
        match mode.as_str() {
            "pipes" => {
                std::io::stdout()
                    .write_all(&vec![b'x'; PIPE_BYTES])
                    .unwrap();
                std::io::stderr()
                    .write_all(&vec![b'y'; PIPE_BYTES])
                    .unwrap();
            }
            "fail" => {
                eprintln!("child diagnostic");
                std::process::exit(7);
            }
            "wait" => loop {
                std::thread::park();
            },
            _ => panic!("unknown child mode: {mode}"),
        }
        std::process::exit(0);
    }

    let output = process::output_with_timeout(
        &mut child("pipes"),
        Duration::from_secs(30),
        Duration::from_millis(10),
    )
    .expect("both pipes must drain before the cap");
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.ends_with(&vec![b'x'; PIPE_BYTES]));
    assert_eq!(output.stderr, vec![b'y'; PIPE_BYTES]);

    let output = process::output_with_timeout(
        &mut child("fail"),
        Duration::from_secs(30),
        Duration::from_millis(10),
    )
    .expect("a failed child is not a timeout");
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stderr, b"child diagnostic\n");
}

#[test]
fn timeout_stops_a_nonterminating_child() {
    let start = Instant::now();
    assert!(process::output_with_timeout(
        &mut child("wait"),
        Duration::from_millis(100),
        Duration::from_millis(10),
    )
    .is_none());
    assert!(start.elapsed() < Duration::from_secs(30));
}

#[test]
fn missing_tools_are_optional_but_failed_tools_are_not() {
    let absent = common::scratch_file("absent-tool", "exe");
    assert!(!absent.exists());
    assert!(process::optional_output(&mut Command::new(absent)).is_none());
    let output = process::optional_output(&mut child("pipes")).unwrap();
    assert_eq!(output.stderr, vec![b'y'; PIPE_BYTES]);
    assert!(catch_unwind(AssertUnwindSafe(|| {
        process::optional_output(&mut child("fail"));
    }))
    .is_err());
}

#[cfg(unix)]
#[test]
fn an_unusable_tool_is_not_missing() {
    let path = common::scratch_file("nonexecutable-tool", "bin");
    std::fs::write(&path, b"not executable").unwrap();
    assert!(catch_unwind(AssertUnwindSafe(|| {
        process::optional_output(&mut Command::new(&path));
    }))
    .is_err());
    std::fs::remove_file(path).unwrap();
}

#[test]
fn required_commands_must_exist_and_succeed() {
    let output = process::required_output(&mut child("pipes"));
    assert_eq!(output.stderr, vec![b'y'; PIPE_BYTES]);
    let absent = common::scratch_file("required-absent-tool", "exe");
    assert!(!absent.exists());
    assert!(catch_unwind(AssertUnwindSafe(|| {
        process::required_output(&mut Command::new(absent));
    }))
    .is_err());
    assert!(catch_unwind(AssertUnwindSafe(|| {
        process::required_output(&mut child("fail"));
    }))
    .is_err());
}

#[cfg(unix)]
fn fake_compiler(program: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = common::scratch_file("fake-sanitizer-compiler", "sh");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nwhile [ \"$#\" -gt 1 ]; do [ \"$1\" = -o ] && out=\"$2\"; shift; done\nprintf '#!/bin/sh\\n{program}\\n' > \"$out\"\nchmod +x \"$out\"\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[cfg(unix)]
#[test]
fn only_a_sanitizer_runtime_that_hangs_is_skipped() {
    let hanging = fake_compiler("exec sleep 600");
    let start = Instant::now();
    assert!(!process::probe_sanitizer(
        &hanging,
        &["-fsanitize=address"],
        Duration::from_millis(500)
    ));
    assert!(start.elapsed() < Duration::from_secs(30));
    let working = fake_compiler("exit 0");
    assert!(process::probe_sanitizer(
        &working,
        &["-fsanitize=address"],
        Duration::from_secs(30)
    ));
    let failing = fake_compiler("exit 1");
    assert!(process::probe_sanitizer(
        &failing,
        &["-fsanitize=address"],
        Duration::from_secs(30)
    ));
    let missing = common::scratch_file("absent-sanitizer-compiler", "sh");
    assert!(process::probe_sanitizer(
        &missing,
        &["-fsanitize=address"],
        Duration::from_secs(30)
    ));
    for path in [hanging, working, failing] {
        std::fs::remove_file(path).unwrap();
    }
}

#[test]
fn capped_runs_keep_the_uncapped_results() {
    use common::process::Capped;
    let output = child("fail").capped_output().unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stderr, b"child diagnostic\n");
    assert_eq!(child("fail").capped_status().unwrap().code(), Some(7));
    let absent = common::scratch_file("capped-absent-tool", "exe");
    assert_eq!(
        Command::new(absent).capped_output().unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
}

/// A Windows drive path is spelled as WSL mounts it wherever an argument
/// carries one; anything else passes through unchanged.
#[test]
fn drive_paths_are_spelled_for_the_linux_host() {
    use common::linux_host::to_linux;
    let root = to_linux(r"C:\");
    assert!(root.starts_with('/') && root.ends_with("/c/"), "{root}");
    let mnt = &root[..root.len() - 2];
    for (arg, want) in [
        (r"C:\work\a b\x.c".to_owned(), format!("{mnt}c/work/a b/x.c")),
        ("d:/tmp/y.o".to_owned(), format!("{mnt}d/tmp/y.o")),
        (r"-IC:\inc".to_owned(), format!("-I{mnt}c/inc")),
        (r"-Wl,-T,C:\x.ld".to_owned(), format!("-Wl,-T,{mnt}c/x.ld")),
        (r"--sysroot=E:\s".to_owned(), format!("--sysroot={mnt}e/s")),
    ] {
        assert_eq!(to_linux(&arg), want, "{arg}");
    }
    for arg in ["-O2", "/usr/include", "x.c", "-std=c11", r"ab:\c", "C:"] {
        assert_eq!(to_linux(arg), arg);
    }
}

/// Only a toolchain name or a Linux program is routed: kuna itself, and any
/// other Windows program, always runs natively.
#[test]
fn only_linux_programs_are_routed() {
    let mut kuna = Command::new(env!("CARGO_BIN_EXE_kuna"));
    kuna.arg("--version");
    common::linux_host::route(&mut kuna);
    assert_eq!(kuna.get_program(), env!("CARGO_BIN_EXE_kuna"));

    let mut cc = Command::new("cc");
    cc.arg("--version");
    common::linux_host::route(&mut cc);
    let program = cc.get_program().to_string_lossy().into_owned();
    if cfg!(windows) {
        assert!(program == "wsl.exe" || program == "kuna-test-wsl-lacks-cc", "{program}");
    } else {
        assert_eq!(program, "cc");
    }
}
