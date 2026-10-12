//! Tests against the real binary: arguments, exit codes, and the split between
//! stdout (account CSV only) and stderr (diagnostics).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should stop with a panic and a clear message"
)]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::default_numeric_fallback,
    reason = "test data uses small literal values, where overflow and inferred types cannot matter"
)]

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn engine(args: &[&OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_payments-engine"))
        .args(args)
        .output()
        .expect("binary runs")
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// A scratch directory unique to one test, removed when dropped.
struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new(test: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("payments-engine-{}-{test}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn processes_file_with_csv_on_stdout_and_diagnostics_on_stderr() {
    let dir = fixture("withdrawal_insufficient");

    let output = engine(&[dir.join("input.csv").as_os_str()]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        fs::read_to_string(dir.join("expected.csv")).unwrap()
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("line 3: rejected [insufficient_funds]"),
        "{stderr}"
    );
    assert!(
        stderr.contains("summary: 2 rows read, 1 applied, 1 not applied"),
        "{stderr}"
    );
}

#[test]
fn no_argument_prints_usage_and_exits_2() {
    let output = engine(&[]);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8(output.stderr).unwrap().contains("usage:"));
}

#[test]
fn two_arguments_print_usage_and_exit_2() {
    let output = engine(&[OsStr::new("a.csv"), OsStr::new("b.csv")]);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
}

#[test]
fn missing_file_exits_1_with_empty_stdout() {
    let output = engine(&[OsStr::new("does-not-exist.csv")]);

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("does-not-exist.csv")
    );
}

#[test]
fn directory_path_exits_1_with_empty_stdout() {
    let scratch = ScratchDir::new("directory");

    let output = engine(&[scratch.0.as_os_str()]);

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
}

#[test]
fn invalid_header_exits_1_with_empty_stdout() {
    let scratch = ScratchDir::new("header");
    let path = scratch.0.join("input.csv");
    fs::write(&path, "type,client,tx,amount,note\ndeposit,1,1,1,x\n").unwrap();

    let output = engine(&[path.as_os_str()]);

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("unknown column \"note\"")
    );
}

#[cfg(unix)]
#[test]
fn unreadable_file_exits_1_with_empty_stdout() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = ScratchDir::new("unreadable");
    let path = scratch.0.join("input.csv");
    fs::write(&path, "type,client,tx,amount\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::File::open(&path).is_ok() {
        return; // running with privileges that ignore permissions, such as root
    }

    let output = engine(&[path.as_os_str()]);

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
}

#[cfg(unix)]
#[test]
fn non_utf8_path_is_accepted() {
    use std::os::unix::ffi::OsStrExt;

    let scratch = ScratchDir::new("non-utf8");
    let path = scratch.0.join(OsStr::from_bytes(b"input-\xff.csv"));
    if fs::write(&path, "type,client,tx,amount\ndeposit,1,1,1\n").is_err() {
        return; // the file system does not allow non-UTF-8 names
    }

    let output = engine(&[path.as_os_str()]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "client,available,held,total,locked\n1,1.0000,0.0000,1.0000,false\n"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn stdout_write_failure_exits_1() {
    let full = fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap();
    let input = fixture("deposit_credits_account").join("input.csv");

    let output = Command::new(env!("CARGO_BIN_EXE_payments-engine"))
        .arg(input)
        .stdout(full)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("failed to write output")
    );
}

#[test]
fn path_in_error_message_is_escaped() {
    let output = engine(&[OsStr::new("missing-\u{1b}[2J.csv")]);

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.contains('\u{1b}'), "{stderr:?}");
    assert!(stderr.contains(r"missing-\u{1b}[2J.csv"), "{stderr}");
}
