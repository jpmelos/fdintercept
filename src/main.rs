use serde::Deserialize;
use std::env;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

#[derive(Deserialize)]
struct Config {
    target: Option<String>,
}

struct ChildGuard {
    child: Child,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn get_config() -> Option<Config> {
    let home = env::var("HOME").ok()?;
    let config_path = PathBuf::from(home).join(".fdinterceptrc.toml");
    let config_contents = std::fs::read_to_string(config_path).ok()?;
    toml::from_str(&config_contents).ok()?
}

fn extract_program_and_args_from_target(mut target: Vec<String>) -> Option<(String, Vec<String>)> {
    if target.is_empty() {
        None
    } else if target.len() == 1 {
        Some((target.pop().unwrap(), Vec::new()))
    } else {
        let mut iter = target.into_iter();
        Some((iter.next().unwrap(), iter.collect()))
    }
}

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    let target_separator_pos = args.iter().position(|arg| arg == "--");
    let (program, program_args) = if let Some(pos) = target_separator_pos {
        extract_program_and_args_from_target(args[pos + 1..].to_vec())
            .expect("Expected target program and args after --")
    } else {
        let config = get_config();
        if let Some(target) = config.and_then(|c| c.target) {
            let program_and_args: Vec<_> = target.split_whitespace().map(String::from).collect();
            extract_program_and_args_from_target(program_and_args).expect("No target program")
        } else {
            panic!("No target program")
        }
    };

    let mut stdin_log = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open("stdin.log")?;
    let mut stdout_log = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open("stdout.log")?;
    let mut stderr_log = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open("stderr.log")?;

    let child = Command::new(program)
        .args(program_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let child_guard = Arc::new(Mutex::new(ChildGuard { child }));

    let child_guard_clone = Arc::clone(&child_guard);
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        if let Ok(mut child_guard_lock) = child_guard_clone.lock() {
            let _ = child_guard_lock.child.kill();
        }
        default_hook(panic_info);
    }));

    let mut child_guard_lock = child_guard.lock().unwrap();
    let mut child_stdin = child_guard_lock
        .child
        .stdin
        .take()
        .expect("Failed to get child stdin");
    let mut child_stdout = child_guard_lock
        .child
        .stdout
        .take()
        .expect("Failed to get child stdout");
    let mut child_stderr = child_guard_lock
        .child
        .stderr
        .take()
        .expect("Failed to get child stderr");
    drop(child_guard_lock);

    let stream_closed = Arc::new(AtomicBool::new(false));

    let stream_closed_clone = Arc::clone(&stream_closed);
    thread::spawn(move || {
        let mut buffer = [0; 1024];
        loop {
            if stream_closed_clone.load(Ordering::SeqCst) {
                break;
            }
            match io::stdin().read(&mut buffer) {
                Ok(0) => {
                    stream_closed_clone.store(true, Ordering::SeqCst);
                    break;
                }
                Ok(n) => {
                    if let Err(_) = child_stdin.write_all(&buffer[..n]) {
                        stream_closed_clone.store(true, Ordering::SeqCst);
                        break;
                    }
                    if let Err(_) = stdin_log.write_all(&buffer[..n]) {
                        stream_closed_clone.store(true, Ordering::SeqCst);
                        break;
                    }
                }
                Err(_) => {
                    stream_closed_clone.store(true, Ordering::SeqCst);
                    break;
                }
            }
        }
    });

    let stream_closed_clone = Arc::clone(&stream_closed);
    thread::spawn(move || {
        let mut buffer = [0; 1024];
        loop {
            if stream_closed_clone.load(Ordering::SeqCst) {
                break;
            }
            match child_stdout.read(&mut buffer) {
                Ok(0) => {
                    stream_closed_clone.store(true, Ordering::SeqCst);
                    break;
                }
                Ok(n) => {
                    if let Err(_) = io::stdout().write_all(&buffer[..n]) {
                        stream_closed_clone.store(true, Ordering::SeqCst);
                        break;
                    }
                    if let Err(_) = stdout_log.write_all(&buffer[..n]) {
                        stream_closed_clone.store(true, Ordering::SeqCst);
                        break;
                    }
                }
                Err(_) => {
                    stream_closed_clone.store(true, Ordering::SeqCst);
                    break;
                }
            }
        }
    });

    let stream_closed_clone = Arc::clone(&stream_closed);
    thread::spawn(move || {
        let mut buffer = [0; 1024];
        loop {
            if stream_closed_clone.load(Ordering::SeqCst) {
                break;
            }
            match child_stderr.read(&mut buffer) {
                Ok(0) => {
                    stream_closed_clone.store(true, Ordering::SeqCst);
                    break;
                }
                Ok(n) => {
                    if let Err(_) = io::stderr().write_all(&buffer[..n]) {
                        stream_closed_clone.store(true, Ordering::SeqCst);
                        break;
                    }
                    if let Err(_) = stderr_log.write_all(&buffer[..n]) {
                        stream_closed_clone.store(true, Ordering::SeqCst);
                        break;
                    }
                }
                Err(_) => {
                    stream_closed_clone.store(true, Ordering::SeqCst);
                    break;
                }
            }
        }
    });

    let mut status = None;
    while !stream_closed.load(Ordering::SeqCst) {
        if let Ok(mut guard) = child_guard.try_lock() {
            if let Ok(exit_status) = guard.child.try_wait() {
                if let Some(s) = exit_status {
                    status = Some(s);
                    break;
                }
            }
        }
        thread::sleep(std::time::Duration::from_millis(10));
    }

    if status.is_none() {
        if let Ok(mut guard) = child_guard.lock() {
            let _ = guard.child.kill();
            status = Some(guard.child.wait()?);
        }
    }

    std::process::exit(status.and_then(|s| s.code()).unwrap_or(1));
}
