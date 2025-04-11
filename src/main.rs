use serde::Deserialize;
use std::env;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
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

    let stdin_log = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open("stdin.log")?;
    let stdout_log = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open("stdout.log")?;
    let stderr_log = OpenOptions::new()
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
    let child_stdin = child_guard_lock
        .child
        .stdin
        .take()
        .expect("Failed to get child stdin");
    let child_stdout = child_guard_lock
        .child
        .stdout
        .take()
        .expect("Failed to get child stdout");
    let child_stderr = child_guard_lock
        .child
        .stderr
        .take()
        .expect("Failed to get child stderr");
    drop(child_guard_lock);

    let stream_closed = Arc::new((Mutex::new(false), Condvar::new()));

    let stream_closed_clone = Arc::clone(&stream_closed);
    spawn_thread_for_fd(io::stdin(), child_stdin, stdin_log, stream_closed_clone);

    let stream_closed_clone = Arc::clone(&stream_closed);
    spawn_thread_for_fd(child_stdout, io::stdout(), stdout_log, stream_closed_clone);

    let stream_closed_clone = Arc::clone(&stream_closed);
    spawn_thread_for_fd(child_stderr, io::stderr(), stderr_log, stream_closed_clone);

    let (stream_closed_mutex_status, stream_closed_condvar) = &*(stream_closed);
    let mut stream_closed_mutex_status_lock = stream_closed_mutex_status.lock().unwrap();
    while !*stream_closed_mutex_status_lock {
        stream_closed_mutex_status_lock = stream_closed_condvar
            .wait(stream_closed_mutex_status_lock)
            .unwrap();
    }

    let mut status = None;
    if let Ok(mut guard) = child_guard.try_lock() {
        if let Ok(exit_status) = guard.child.try_wait() {
            if let Some(s) = exit_status {
                status = Some(s);
            }
        }
    }

    if status.is_none() {
        if let Ok(mut guard) = child_guard.lock() {
            let _ = guard.child.kill();
            status = Some(guard.child.wait()?);
        }
    }

    std::process::exit(status.and_then(|s| s.code()).unwrap_or(1));
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

fn spawn_thread_for_fd(
    mut src_fd: impl Read + Send + 'static,
    mut dst_fd: impl Write + Send + 'static,
    mut log: File,
    stream_closed: Arc<(Mutex<bool>, Condvar)>,
) {
    thread::spawn(move || {
        let (stream_closed_mutex_status, stream_closed_condvar) = &*(stream_closed);
        let mut buffer = [0; 1024];
        loop {
            if *stream_closed_mutex_status.lock().unwrap() {
                break;
            }
            match src_fd.read(&mut buffer) {
                Ok(0) => {
                    *stream_closed_mutex_status.lock().unwrap() = true;
                    stream_closed_condvar.notify_one();
                    break;
                }
                Ok(n) => {
                    if let Err(_) = dst_fd.write_all(&buffer[..n]) {
                        *stream_closed_mutex_status.lock().unwrap() = true;
                        stream_closed_condvar.notify_one();
                        break;
                    }
                    if let Err(_) = log.write_all(&buffer[..n]) {
                        *stream_closed_mutex_status.lock().unwrap() = true;
                        stream_closed_condvar.notify_one();
                        break;
                    }
                }
                Err(_) => {
                    *stream_closed_mutex_status.lock().unwrap() = true;
                    stream_closed_condvar.notify_one();
                    break;
                }
            }
        }
    });
}
