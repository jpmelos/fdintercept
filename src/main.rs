use clap::Parser;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use non_empty_string::NonEmptyString;
use nonempty::NonEmpty;
use serde::Deserialize;
use std::env;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use wait_timeout::ChildExt;

#[derive(Parser)]
#[command(about, version)]
struct CliArgs {
    #[arg(last = true)]
    target: Vec<String>,
}

#[derive(Debug)]
enum CliArgsTargetParseError {
    NotDefined,
    EmptyExecutable,
}

impl std::fmt::Display for CliArgsTargetParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotDefined => write!(f, "Target is not defined"),
            Self::EmptyExecutable => write!(f, "Target executable cannot be empty"),
        }
    }
}

impl std::error::Error for CliArgsTargetParseError {}

#[derive(Deserialize)]
struct Config {
    target: Option<String>,
}

#[derive(Debug)]
enum ConfigTargetParseError {
    NotDefined,
    FailedToTokenize,
    Empty,
    EmptyExecutable,
}

impl std::fmt::Display for ConfigTargetParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotDefined => write!(f, "Target is not defined"),
            Self::FailedToTokenize => write!(f, "Failed to tokenize target"),
            Self::Empty => write!(f, "Target cannot be empty"),
            Self::EmptyExecutable => write!(f, "Target executable cannot be empty"),
        }
    }
}

impl std::error::Error for ConfigTargetParseError {}

struct Target {
    executable: NonEmptyString,
    args: Vec<String>,
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
    let config = get_config().unwrap_or_else(|e| {
        eprintln!("Error reading config: {}", e);
        std::process::exit(1);
    });

    let target = get_target_from_cli_args_or_config(&cli_args, &config).unwrap_or_else(|e| {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    });

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

    let child = Command::new(String::from(target.executable))
        .args(target.args)
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

    let threads = vec![
        spawn_thread_for_fd(io::stdin(), child_stdin, stdin_log),
        spawn_thread_for_fd(child_stdout, io::stdout(), stdout_log),
        spawn_thread_for_fd(child_stderr, io::stderr(), stderr_log),
    ];
    for thread in threads {
        thread.join().expect("Thread panicked");
    }

    std::process::exit(
        kill_child_process_with_grace_period(&child_guard)
            .code()
            .unwrap_or(1),
    );
}

fn get_config() -> Result<Option<Config>, Box<dyn std::error::Error>> {
    let home = env::var("HOME")?;
    let config_path = PathBuf::from(home).join(".fdinterceptrc.toml");

    let config_contents = match std::fs::read_to_string(config_path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Box::new(e)),
    };

    match toml::from_str(&config_contents) {
        Ok(config) => Ok(config),
        Err(e) => Err(Box::new(e)),
    }
}

fn get_target_from_cli_args_or_config(
    cli_args: &CliArgs,
    config: &Option<Config>,
) -> Result<Target, Box<dyn std::error::Error>> {
    match get_target_from_cli_args(cli_args) {
        Ok(target) => return Ok(target),
        Err(CliArgsTargetParseError::NotDefined) => (),
        Err(e) => return Err(e.into()),
    };
    match config {
        Some(cfg) => Ok(get_target_from_config(cfg)?),
        None => Err(ConfigTargetParseError::NotDefined.into()),
    }
}

fn get_target_from_cli_args(cli_args: &CliArgs) -> Result<Target, CliArgsTargetParseError> {
    if cli_args.target.is_empty() {
        return Err(CliArgsTargetParseError::NotDefined);
    }

    let target_vec = NonEmpty::from_vec(cli_args.target.clone()).unwrap();
    Ok(Target {
        executable: NonEmptyString::new(target_vec.head)
            .map_err(|_| CliArgsTargetParseError::EmptyExecutable)?,
        args: target_vec.tail,
    })
}

fn get_target_from_config(config: &Config) -> Result<Target, ConfigTargetParseError> {
    let tokenized_target = shlex::split(
        config
            .target
            .as_ref()
            .ok_or(ConfigTargetParseError::NotDefined)?,
    )
    .ok_or(ConfigTargetParseError::FailedToTokenize)?;
    let target_vec = NonEmpty::from_vec(tokenized_target).ok_or(ConfigTargetParseError::Empty)?;
    Ok(Target {
        executable: NonEmptyString::new(target_vec.head)
            .map_err(|_| ConfigTargetParseError::EmptyExecutable)?,
        args: target_vec.tail,
    })
}

fn spawn_thread_for_fd(
    mut src_fd: impl Read + Send + 'static,
    mut dst_fd: impl Write + Send + 'static,
    mut log: File,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut buffer = [0; 1024];
        loop {
            let bytes_read = src_fd
                .read(&mut buffer)
                .expect("Failed to read from source fd");
            if bytes_read == 0 {
                return;
            }

            log.write_all(&buffer[..bytes_read])
                .expect("Failed to write to log file");

            match dst_fd.write_all(&buffer[..bytes_read]) {
                Ok(_) => (),
                Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
                    return;
                }
                Err(e) => panic!("Failed to write to destination fd: {}", e),
            }
        }
    })
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
