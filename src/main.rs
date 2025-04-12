use anyhow::{Context, Result};
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
        if let Err(e) = self.child.kill() {
            eprintln!("Error sending signal to child process: {}", e);
        }
        if let Err(e) = self.child.wait() {
            eprintln!("Error waiting for child process: {}", e);
        }
    }
}

fn main() -> Result<()> {
    let cli_args = CliArgs::parse();
    let config = get_config().context("Error reading configuration")?;

    let target =
        get_target_from_cli_args_or_config(&cli_args, &config).context("Error getting target")?;

    let stdin_log = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open("stdin.log")
        .context("Error creating stdin log file")?;
    let stdout_log = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open("stdout.log")
        .context("Error creating stdout log file")?;
    let stderr_log = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open("stderr.log")
        .context("Error creating stderr log file")?;

    let child = Command::new(String::from(target.executable))
        .args(target.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Error starting child process")?;
    let child_guard = Arc::new(Mutex::new(ChildGuard { child }));

    let child_guard_clone = Arc::clone(&child_guard);
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        if let Err(e) = kill_child_process_with_grace_period(&child_guard_clone) {
            eprintln!("Error while cleaning up child process during panic: {}", e);
        }
        default_hook(panic_info);
    }));

    let (stdin, stdout, stderr) = {
        let mut child_guard_lock = child_guard
            .lock()
            .map_err(|e| anyhow::anyhow!("Error acquiring lock for child process: {}", e))?;
        let child = &mut child_guard_lock.child;
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    };
    let child_stdin = stdin.context("Error taking stdin of child")?;
    let child_stdout = stdout.context("Error taking stdout of child")?;
    let child_stderr = stderr.context("Error taking stderr of child")?;

    let threads = vec![
        spawn_thread_for_fd(io::stdin(), child_stdin, stdin_log),
        spawn_thread_for_fd(child_stdout, io::stdout(), stdout_log),
        spawn_thread_for_fd(child_stderr, io::stderr(), stderr_log),
    ];
    for thread in threads {
        thread
            .join()
            .map_err(|e| anyhow::anyhow!("Error joining thread: {:?}", e))?
            .context("Error in stream threads")?;
    }

    std::process::exit(
        kill_child_process_with_grace_period(&child_guard)
            .context("Error killing child")?
            .code()
            .unwrap_or(1),
    );
}

fn get_config() -> Result<Option<Config>> {
    let home = env::var("HOME").context("Error getting HOME environment variable")?;
    let config_path = PathBuf::from(home).join(".fdinterceptrc.toml");

    let config_contents = match std::fs::read_to_string(&config_path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(e).context(format!(
                "Error reading configuration file: {}",
                config_path.display()
            ));
        }
    };

    Ok(Some(
        toml::from_str(&config_contents).context("Error parsing TOML configuration")?,
    ))
}

fn get_target_from_cli_args_or_config(
    cli_args: &CliArgs,
    config: &Option<Config>,
) -> Result<Target> {
    match get_target_from_cli_args(cli_args) {
        Ok(target) => return Ok(target),
        Err(CliArgsTargetParseError::NotDefined) => (),
        Err(e) => return Err(e).context("Error getting target from CLI arguments"),
    };
    match config {
        Some(cfg) => {
            Ok(get_target_from_config(cfg)
                .context("Error getting target from configuration file")?)
        }
        None => Err(anyhow::anyhow!(
            "Target not defined in CLI arguments and configuration file"
        )),
    }
}

fn get_target_from_cli_args(cli_args: &CliArgs) -> Result<Target, CliArgsTargetParseError> {
    if cli_args.target.is_empty() {
        return Err(CliArgsTargetParseError::NotDefined);
    }
    // unwrap: Cannot fail because we have already checked that the vector is not empty.
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
) -> JoinHandle<Result<()>> {
    thread::spawn(move || {
        let mut buffer = [0; 1024];
        loop {
            let bytes_read = src_fd
                .read(&mut buffer)
                .context("Error reading from source stream")?;
            if bytes_read == 0 {
                return Ok(());
            }

            log.write_all(&buffer[..bytes_read])
                .context("Error writing to log file")?;

            match dst_fd.write_all(&buffer[..bytes_read]) {
                Ok(_) => (),
                Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
                    return Ok(());
                }
                Err(e) => return Err(anyhow::anyhow!("Error writing to destination fd: {}", e)),
            }
        }
    })
}

fn kill_child_process_with_grace_period(
    child_guard: &Arc<Mutex<ChildGuard>>,
) -> Result<ExitStatus> {
    let mut child_guard_lock = child_guard
        .lock()
        .map_err(|e| anyhow::anyhow!("Error acquiring lock for child process: {}", e))?;
    let child = &mut child_guard_lock.child;

    if let Some(status) = child
        .try_wait()
        .context("Error waiting for child process")?
    {
        return Ok(status);
    }

    kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM)
        .context("Error sending signal to child process")?;

    if let Some(status) = child
        .wait_timeout(Duration::from_secs(15))
        .context("Error waiting for child process")?
    {
        return Ok(status);
    }

    child
        .kill()
        .context("Error sending signal to child process")?;
    child.wait().context("Error waiting for child process")
}
