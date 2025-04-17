use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

#[test]
fn test_normal_termination() {
    let child_binary_dir = get_child_binary_dir();
    let mut fdintercept = run_main_process(&child_binary_dir);
    let mut stdin = fdintercept.stdin.take().unwrap();
    stdin.write_all(b"hello\nworld\nexit\n").unwrap();
    let status = fdintercept.wait().unwrap();

    assert!(status.success());
    assert_eq!(
        fs::read_to_string(
            child_binary_dir.join(format!("stdin.{:?}.log", std::thread::current().id()))
        )
        .unwrap(),
        "hello\nworld\nexit\n"
    );
    assert_eq!(
        fs::read_to_string(
            child_binary_dir.join(format!("stdout.{:?}.log", std::thread::current().id()))
        )
        .unwrap(),
        "Starting...\nEcho: hello\nEcho: world\n"
    );
    assert_eq!(
        fs::read_to_string(
            child_binary_dir.join(format!("stderr.{:?}.log", std::thread::current().id()))
        )
        .unwrap(),
        "Error message\n"
    );
}

#[test]
fn test_termination_by_signal() {
    let child_binary_dir = get_child_binary_dir();
    let mut fdintercept = run_main_process(&child_binary_dir);
    let mut stdout = fdintercept.stdout.take().unwrap();
    stdout.read_exact(&mut [0; 1]).unwrap();
    signal::kill(
        Pid::from_raw(i32::try_from(fdintercept.id()).unwrap()),
        Signal::SIGTERM,
    )
    .unwrap();
    let status = fdintercept.wait().unwrap();

    assert_eq!(status.code().unwrap(), 143); // 128 + SIGTERM (15)
    assert_eq!(
        fs::read_to_string(
            child_binary_dir.join(format!("stdin.{:?}.log", std::thread::current().id()))
        )
        .unwrap(),
        ""
    );
    assert_eq!(
        fs::read_to_string(
            child_binary_dir.join(format!("stdout.{:?}.log", std::thread::current().id()))
        )
        .unwrap(),
        "Starting...\n"
    );
    assert_eq!(
        fs::read_to_string(
            child_binary_dir.join(format!("stderr.{:?}.log", std::thread::current().id()))
        )
        .unwrap(),
        "Error message\n"
    );
}

#[test]
fn test_child_process_error() {
    let child_binary_dir = get_child_binary_dir();
    let mut fdintercept = run_main_process(&child_binary_dir);
    let mut stdin = fdintercept.stdin.take().unwrap();
    stdin.write_all(b"error\n").unwrap();
    let status = fdintercept.wait().unwrap();

    assert_eq!(status.code().unwrap(), 42);
    assert_eq!(
        fs::read_to_string(
            child_binary_dir.join(format!("stdin.{:?}.log", std::thread::current().id()))
        )
        .unwrap(),
        "error\n"
    );
    assert_eq!(
        fs::read_to_string(
            child_binary_dir.join(format!("stdout.{:?}.log", std::thread::current().id()))
        )
        .unwrap(),
        "Starting...\nExiting with error...\n"
    );
    assert_eq!(
        fs::read_to_string(
            child_binary_dir.join(format!("stderr.{:?}.log", std::thread::current().id()))
        )
        .unwrap(),
        "Error message\n"
    );
}

#[test]
fn test_very_small_buffer_size() {
    let child_binary_dir = get_child_binary_dir();
    let mut fdintercept = Command::new("target/debug/fdintercept")
        .args([
            "--buffer-size",
            "16", // Very small buffer to test chunking
            "--stdin-log",
            child_binary_dir
                .join(format!("stdin.{:?}.log", std::thread::current().id()))
                .to_str()
                .unwrap(),
            "--stdout-log",
            child_binary_dir
                .join(format!("stdout.{:?}.log", std::thread::current().id()))
                .to_str()
                .unwrap(),
            "--stderr-log",
            child_binary_dir
                .join(format!("stderr.{:?}.log", std::thread::current().id()))
                .to_str()
                .unwrap(),
            "--recreate-logs",
            "--",
            child_binary_dir.join(CHILD_BINARY_NAME).to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = fdintercept.stdin.take().unwrap();
    let test_data = "hello world this is a longer string\nexit\n";
    stdin.write_all(test_data.as_bytes()).unwrap();
    let status = fdintercept.wait().unwrap();

    assert!(status.success());
    assert_eq!(
        fs::read_to_string(
            child_binary_dir.join(format!("stdin.{:?}.log", std::thread::current().id()))
        )
        .unwrap(),
        test_data
    );
}

#[test]
fn test_nonexistent_command() {
    let child_binary_dir = get_child_binary_dir();
    let result = Command::new("target/debug/fdintercept")
        .args([
            "--stdin-log",
            child_binary_dir
                .join(format!("stdin.{:?}.log", std::thread::current().id()))
                .to_str()
                .unwrap(),
            "--",
            "nonexistent_command",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();

    assert_eq!(result.unwrap().wait().unwrap().code().unwrap(), 1);
}

#[test]
fn test_append() {
    let child_binary_dir = get_child_binary_dir();
    let stdout_log = child_binary_dir.join(format!("stdout.{:?}.log", std::thread::current().id()));

    // First run.
    let mut fdintercept = Command::new("target/debug/fdintercept")
        .args([
            "--stdout-log",
            stdout_log.to_str().unwrap(),
            "--recreate-logs",
            "--",
            child_binary_dir.join(CHILD_BINARY_NAME).to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = fdintercept.stdin.take().unwrap();
    stdin.write_all(b"hello\nexit\n").unwrap();
    fdintercept.wait().unwrap();

    // Second run without, --recreate-logs.
    let mut fdintercept = Command::new("target/debug/fdintercept")
        .args([
            "--stdout-log",
            stdout_log.to_str().unwrap(),
            "--",
            child_binary_dir.join(CHILD_BINARY_NAME).to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = fdintercept.stdin.take().unwrap();
    stdin.write_all(b"world\nexit\n").unwrap();
    fdintercept.wait().unwrap();

    assert_eq!(
        fs::read_to_string(&stdout_log).unwrap(),
        "Starting...\nEcho: hello\nStarting...\nEcho: world\n"
    );
}

const CHILD_BINARY_NAME: &str = "child_process";

fn get_child_binary_dir() -> PathBuf {
    let out_dir = PathBuf::from("/tmp/cargo-test/target");
    fs::create_dir_all(&out_dir).unwrap();

    let binary_path = out_dir.join(CHILD_BINARY_NAME);
    let lock_path = out_dir.join(format!("{CHILD_BINARY_NAME}.lock"));

    if !binary_path.exists() {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(_) => {
                Command::new("rustc")
                    .arg("tests/child_process.rs")
                    .arg("-o")
                    .arg(&binary_path)
                    .status()
                    .unwrap();

                fs::remove_file(&lock_path).unwrap();
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                while lock_path.exists() {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            Err(e) => {
                panic!("Error: {e}");
            }
        }
    }

    out_dir
}

fn run_main_process(child_binary_dir: &Path) -> Child {
    Command::new("target/debug/fdintercept")
        .args([
            "--stdin-log",
            child_binary_dir
                .join(format!("stdin.{:?}.log", std::thread::current().id()))
                .to_str()
                .unwrap(),
            "--stdout-log",
            child_binary_dir
                .join(format!("stdout.{:?}.log", std::thread::current().id()))
                .to_str()
                .unwrap(),
            "--stderr-log",
            child_binary_dir
                .join(format!("stderr.{:?}.log", std::thread::current().id()))
                .to_str()
                .unwrap(),
            "--recreate-logs",
            "--",
            child_binary_dir.join(CHILD_BINARY_NAME).to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}
