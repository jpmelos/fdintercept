use anyhow::{Context, Result};
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use std::fs;
use std::io::Read;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use tempfile::TempDir;

fn setup_test_environment() -> Result<(TempDir, PathBuf)> {
    let temp_dir = tempfile::tempdir().unwrap();

    let script_path = temp_dir.path().join("child_process.rs");
    fs::copy("tests/child_process.rs", &script_path).unwrap();
    Command::new("rustc")
        .arg(&script_path)
        .current_dir(&temp_dir)
        .status()
        .context("Error compiling child process")?;

    Ok((temp_dir, script_path.with_file_name("child_process")))
}

fn run_main_process(temp_dir: &TempDir, test_script: &PathBuf) -> Child {
    Command::new("target/debug/fdintercept")
        .args([
            "--stdin-log",
            temp_dir.path().join("stdin.log").to_str().unwrap(),
            "--stdout-log",
            temp_dir.path().join("stdout.log").to_str().unwrap(),
            "--stderr-log",
            temp_dir.path().join("stderr.log").to_str().unwrap(),
            "--",
            test_script.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

#[test]
fn test_normal_termination() {
    let (temp_dir, test_script) = setup_test_environment().unwrap();
    let mut fdintercept = run_main_process(&temp_dir, &test_script);
    let mut stdin = fdintercept.stdin.take().unwrap();
    stdin.write_all(b"hello\nworld\nexit\n").unwrap();
    let status = fdintercept.wait().unwrap();

    assert!(status.success());
    assert_eq!(
        fs::read_to_string(temp_dir.path().join("stdin.log")).unwrap(),
        "hello\nworld\nexit\n"
    );
    assert_eq!(
        fs::read_to_string(temp_dir.path().join("stdout.log")).unwrap(),
        "Starting...\nEcho: hello\nEcho: world\n"
    );
    assert_eq!(
        fs::read_to_string(temp_dir.path().join("stderr.log")).unwrap(),
        "Error message\n"
    );
}

#[test]
fn test_termination_by_signal() {
    let (temp_dir, test_script) = setup_test_environment().unwrap();
    let mut fdintercept = run_main_process(&temp_dir, &test_script);
    // Wait for child process to start by checking for writes to stdout.
    let mut stdout = fdintercept.stdout.take().unwrap();
    stdout.read(&mut [0; 1]).unwrap();
    signal::kill(Pid::from_raw(fdintercept.id() as i32), Signal::SIGTERM).unwrap();
    let status = fdintercept.wait().unwrap();

    assert_eq!(status.code().unwrap(), 143); // 128 + SIGTERM (15)
    assert_eq!(
        fs::read_to_string(temp_dir.path().join("stdin.log")).unwrap(),
        ""
    );
    assert_eq!(
        fs::read_to_string(temp_dir.path().join("stdout.log")).unwrap(),
        "Starting...\n"
    );
    assert_eq!(
        fs::read_to_string(temp_dir.path().join("stderr.log")).unwrap(),
        "Error message\n"
    );
}

#[test]
fn test_child_process_error() {
    let (temp_dir, test_script) = setup_test_environment().unwrap();
    let mut fdintercept = run_main_process(&temp_dir, &test_script);
    let mut stdin = fdintercept.stdin.take().unwrap();
    stdin.write_all(b"error\n").unwrap();
    let status = fdintercept.wait().unwrap();

    assert_eq!(status.code().unwrap(), 42);
    assert_eq!(
        fs::read_to_string(temp_dir.path().join("stdin.log")).unwrap(),
        "error\n"
    );
    assert_eq!(
        fs::read_to_string(temp_dir.path().join("stdout.log")).unwrap(),
        "Starting...\nExiting with error...\n"
    );
    assert_eq!(
        fs::read_to_string(temp_dir.path().join("stderr.log")).unwrap(),
        "Error message\n"
    );
}
