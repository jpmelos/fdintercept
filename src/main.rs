use clap::Parser;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde::Deserialize;
use std::env;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;
use wait_timeout::ChildExt;

#[derive(Parser)]
#[command(about, version)]
struct CliArgs {
    #[arg(last = true)]
    target: Vec<String>,
}

#[derive(Deserialize)]
struct Config {
    target: Option<String>,
}

struct ChildGuard {
    child: Child,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.child.kill().expect("Failed to kill child process");
        self.child.wait().unwrap();
    }
}

fn main() {
    let cli_args = CliArgs::parse();

    let (program, program_args) = if !cli_args.target.is_empty() {
        extract_program_and_args_from_target(cli_args.target.clone())
            .expect("Cannot panic since `target` is never empty")
    } else {
        let config = get_config();
        if let Some(target) = config.and_then(|c| c.target) {
            extract_program_and_args_from_target(vec![target.clone()]).expect("No target program")
        } else {
            panic!("No target program")
        }
    };

    let stdin_log = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open("stdin.log")
        .expect("Failed to create log file");
    let stdout_log = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open("stdout.log")
        .expect("Failed to create log file");
    let stderr_log = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open("stderr.log")
        .expect("Failed to create log file");

    let child = Command::new(program)
        .args(program_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to start child process");
    let child_guard = Arc::new(Mutex::new(ChildGuard { child }));

    let child_guard_clone = Arc::clone(&child_guard);
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        kill_child_process_with_grace_period(&child_guard_clone);
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

    let (stream_closed_mutex_status, stream_closed_condvar) = &*stream_closed;
    let mut stream_closed_mutex_status_lock = stream_closed_mutex_status.lock().unwrap();
    while !*stream_closed_mutex_status_lock {
        stream_closed_mutex_status_lock = stream_closed_condvar
            .wait(stream_closed_mutex_status_lock)
            .expect("Mutex was poisoned")
    }

    kill_child_process_with_grace_period(&child_guard);
}

fn get_config() -> Option<Config> {
    let home = env::var("HOME").ok()?;
    let config_path = PathBuf::from(home).join(".fdinterceptrc.toml");
    let config_contents = std::fs::read_to_string(config_path).ok()?;
    toml::from_str(&config_contents).ok()?
}

fn extract_program_and_args_from_target(mut target: Vec<String>) -> Option<(String, Vec<String>)> {
    if target.len() == 1 {
        target = shlex::split(&target.pop().unwrap()).expect("Failed to parse target");
    }

    if target.is_empty() {
        None
    } else {
        let mut target_iter = target.into_iter();
        Some((target_iter.next().unwrap(), target_iter.collect()))
    }
}

fn spawn_thread_for_fd(
    mut src_fd: impl Read + Send + 'static,
    mut dst_fd: impl Write + Send + 'static,
    mut log: File,
    stream_closed: Arc<(Mutex<bool>, Condvar)>,
) {
    thread::spawn(move || {
        let (stream_closed_mutex_status, stream_closed_condvar) = &*stream_closed;
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

fn kill_child_process_with_grace_period(child_guard: &Arc<Mutex<ChildGuard>>) -> ExitStatus {
    let child = &mut child_guard.lock().unwrap().child;

    if let Some(status) = child.try_wait().expect("Failed to wait for child process") {
        return status;
    }

    let pid = Pid::from_raw(child.id() as i32);
    kill(pid, Signal::SIGTERM).expect("Failed to send signal to child");

    if let Some(status) = child
        .wait_timeout(Duration::from_secs(15))
        .expect("Failed to wait for child process")
    {
        return status;
    }

    child.kill().expect("Failed to kill child process");
    child.wait().expect("Failed to wait for child process")
}
