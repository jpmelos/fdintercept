use anyhow::{Context, Result};
use clap::Parser;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use non_empty_string::NonEmptyString;
use nonempty::NonEmpty;
use serde::Deserialize;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::{Signals, SignalsInfo};
use std::env;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use wait_timeout::ChildExt;

#[derive(Parser)]
#[command(about, version)]
struct CliArgs {
    #[arg(long)]
    stdin_log: Option<PathBuf>,

    #[arg(long)]
    stdout_log: Option<PathBuf>,

    #[arg(long)]
    stderr_log: Option<PathBuf>,

    #[arg(last = true)]
    target: Vec<String>,
}

#[derive(Deserialize)]
struct Config {
    target: Option<String>,
    stdin_log: Option<PathBuf>,
    stdout_log: Option<PathBuf>,
    stderr_log: Option<PathBuf>,
}

struct Target {
    executable: NonEmptyString,
    args: Vec<String>,
}

struct ChildGuard {
    child: Child,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Err(e) = kill_child_process_with_grace_period(&mut self.child, Signal::SIGTERM) {
            eprintln!("Error cleaning up child process: {}", e);
        }
    }
}

fn kill_child_process_with_grace_period(child: &mut Child, signal: Signal) -> Result<ExitStatus> {
    if let Some(status) = child
        .try_wait()
        .context("Error waiting for child process")?
    {
        return Ok(status);
    }

    kill(Pid::from_raw(child.id() as i32), signal)
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

fn main() -> Result<()> {
    let mut signals =
        Signals::new([SIGTERM, SIGINT, SIGHUP]).context("Failed to register signal handlers")?;

    let cli_args = CliArgs::parse();
    let env_var = env::var("FDINTERCEPT_TARGET").ok();
    let config = get_config().context("Error reading configuration")?;

    let target = get_target(&cli_args, &env_var, &config).context("Error getting target")?;

    let use_defaults = cli_args.stdin_log.is_none()
        && cli_args.stdout_log.is_none()
        && cli_args.stderr_log.is_none()
        && config.stdin_log.is_none()
        && config.stdout_log.is_none()
        && config.stderr_log.is_none();

    let stdin_log = create_log_file(
        use_defaults,
        &cli_args.stdin_log,
        &config.stdin_log,
        "stdin.log",
    )?;
    let stdout_log = create_log_file(
        use_defaults,
        &cli_args.stdout_log,
        &config.stdout_log,
        "stdout.log",
    )?;
    let stderr_log = create_log_file(
        use_defaults,
        &cli_args.stderr_log,
        &config.stderr_log,
        "stderr.log",
    )?;

    if let Some(signal) = signals.pending().next() {
        std::process::exit(128 + signal);
    }

    let mut child_guard = ChildGuard {
        child: Command::new(String::from(target.executable))
            .args(target.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Error starting child process")?,
    };
    let child = &mut child_guard.child;

    let child_stdin = child.stdin.take().context("Error taking stdin of child")?;
    let child_stdout = child
        .stdout
        .take()
        .context("Error taking stdout of child")?;
    let child_stderr = child
        .stderr
        .take()
        .context("Error taking stderr of child")?;

    let mutex_child_guard = Arc::new(Mutex::new(child_guard));

    thread::scope(|scope| {
        let threads = vec![
            (
                scope.spawn(|| process_fd(io::stdin(), child_stdin, stdin_log, "stdin")),
                "process_fd:stdin",
            ),
            (
                scope.spawn(|| process_fd(child_stdout, io::stdout(), stdout_log, "stdout")),
                "process_fd:stdout",
            ),
            (
                scope.spawn(|| process_fd(child_stderr, io::stderr(), stderr_log, "stderr")),
                "process_fd:stderr",
            ),
            (
                scope.spawn(|| process_signals(signals, mutex_child_guard.clone())),
                "process_signals",
            ),
        ];
        let _: Vec<()> = threads
            .into_iter()
            .map(|(handle, thread_name)| {
                handle
                    .join()
                    .map_err(|e| eprintln!("Error joining thread {}: {:?}", thread_name, e))
                    .and_then(|result| {
                        result.map_err(|e| eprintln!("Error in thread {}: {:?}", thread_name, e))
                    })
                    .unwrap_or(())
            })
            .collect();
    });

    std::process::exit(
        mutex_child_guard
            .lock()
            // unwrap: Safe because if we got here, the only other instance of `mutex_child_guard`
            // is dead, since it lived inside one of the threads that we already joined into.
            .unwrap()
            .child
            .wait()
            .context("Error waiting for child")?
            .code()
            .unwrap_or(1),
    );
}

fn get_config() -> Result<Config> {
    let home = env::var("HOME").context("Error getting HOME environment variable")?;
    let config_path = PathBuf::from(home).join(".fdinterceptrc.toml");

    let contents = match std::fs::read_to_string(&config_path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::from(""),
        Err(e) => {
            return Err(e).context(format!(
                "Error reading configuration file: {}",
                config_path.display()
            ));
        }
    };

    Ok(toml::from_str(&contents).context("Error parsing TOML configuration")?)
}

fn get_target(
    cli_args: &CliArgs,
    maybe_env_var: &Option<String>,
    config: &Config,
) -> Result<Target> {
    match get_target_from_cli_args(cli_args) {
        Ok(target) => return Ok(target),
        Err(CliArgsTargetParseError::NotDefined) => (),
        Err(e) => return Err(e).context("Error getting target from CLI arguments"),
    };

    if let Some(env_var) = maybe_env_var {
        return Ok(get_target_from_env_var(env_var)
            .context("Error getting target environment variable")?);
    }

    match get_target_from_config(config) {
        Ok(target) => return Ok(target),
        Err(ConfigTargetParseError::NotDefined) => (),
        Err(e) => return Err(e).context("Error getting target from configuration file"),
    }

    Err(anyhow::anyhow!(
        "Target not defined in CLI arguments, FDINTERCEPT_TARGET environment variable, or \
         configuration file"
    ))
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

#[derive(Debug)]
enum EnvVarTargetParseError {
    Empty,
    FailedToTokenize,
    EmptyExecutable,
}

impl std::fmt::Display for EnvVarTargetParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "FDINTERCEPT_TARGET cannot be empty"),
            Self::FailedToTokenize => write!(f, "Failed to tokenize FDINTERCEPT_TARGET"),
            Self::EmptyExecutable => write!(f, "FDINTERCEPT_TARGET executable cannot be empty"),
        }
    }
}

impl std::error::Error for EnvVarTargetParseError {}

fn get_target_from_env_var(env_var: &str) -> Result<Target, EnvVarTargetParseError> {
    if env_var.is_empty() {
        return Err(EnvVarTargetParseError::Empty);
    }
    let tokenized_target = shlex::split(env_var).ok_or(EnvVarTargetParseError::FailedToTokenize)?;
    // unwrap: Safe because we already ensure that target is not empty.
    let target_vec = NonEmpty::from_vec(tokenized_target).unwrap();
    Ok(Target {
        executable: NonEmptyString::new(target_vec.head)
            .map_err(|_| EnvVarTargetParseError::EmptyExecutable)?,
        args: target_vec.tail,
    })
}

#[derive(Debug)]
enum ConfigTargetParseError {
    Empty,
    NotDefined,
    FailedToTokenize,
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

fn get_target_from_config(config: &Config) -> Result<Target, ConfigTargetParseError> {
    let target = config
        .target
        .as_ref()
        .ok_or(ConfigTargetParseError::NotDefined)?;
    if target.is_empty() {
        return Err(ConfigTargetParseError::Empty);
    }
    let tokenized_target = shlex::split(target).ok_or(ConfigTargetParseError::FailedToTokenize)?;
    // unwrap: Safe because we already ensure that target is not empty.
    let target_vec = NonEmpty::from_vec(tokenized_target).unwrap();
    Ok(Target {
        executable: NonEmptyString::new(target_vec.head)
            .map_err(|_| ConfigTargetParseError::EmptyExecutable)?,
        args: target_vec.tail,
    })
}

fn create_log_file(
    use_defaults: bool,
    maybe_cli_log_name: &Option<PathBuf>,
    maybe_config_log_name: &Option<PathBuf>,
    default_name: &str,
) -> Result<Option<File>> {
    maybe_cli_log_name
        .as_ref()
        .or(maybe_config_log_name.as_ref())
        .map_or_else(
            || {
                if use_defaults {
                    return Some(PathBuf::from(default_name));
                }
                None
            },
            |p| Some(p.clone()),
        )
        .map_or(None, |path| {
            Some(
                OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&path)
                    .context(format!("Error creating log file {:?}", path)),
            )
        })
        .transpose()
}

fn process_fd(
    src_fd: impl Read + Send + 'static,
    dst_fd: impl Write + Send + 'static,
    maybe_log: Option<File>,
    log_descriptor: &'static str,
) -> Result<()> {
    if let Some(log) = maybe_log {
        return process_fd_with_logging(src_fd, dst_fd, log, log_descriptor);
    }
    process_fd_without_logging(src_fd, dst_fd, log_descriptor)
}

fn process_fd_with_logging(
    mut src_fd: impl Read + Send + 'static,
    mut dst_fd: impl Write + Send + 'static,
    mut log: File,
    log_descriptor: &'static str,
) -> Result<()> {
    let mut buffer = [0; 1024];
    let mut logging_enabled = true;

    loop {
        let bytes_read = src_fd.read(&mut buffer).context(format!(
            "Error reading from {} source stream",
            log_descriptor
        ))?;
        if bytes_read == 0 {
            return Ok(());
        }

        if logging_enabled {
            if let Err(e) = log.write_all(&buffer[..bytes_read]) {
                eprintln!(
                    "Error writing to {} log, disabling logging: {}",
                    log_descriptor, e
                );
                logging_enabled = false;
            }
        }

        match dst_fd.write_all(&buffer[..bytes_read]) {
            Ok(_) => (),
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
                return Ok(());
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Error writing to {} destination stream: {}",
                    log_descriptor,
                    e
                ));
            }
        }
    }
}

fn process_fd_without_logging(
    mut src_fd: impl Read + Send + 'static,
    mut dst_fd: impl Write + Send + 'static,
    log_descriptor: &'static str,
) -> Result<()> {
    let mut buffer = [0; 1024];
    loop {
        let bytes_read = src_fd.read(&mut buffer).context(format!(
            "Error reading from {} source stream",
            log_descriptor
        ))?;
        if bytes_read == 0 {
            return Ok(());
        }

        match dst_fd.write_all(&buffer[..bytes_read]) {
            Ok(_) => (),
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
                return Ok(());
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Error writing to {} destination stream: {}",
                    log_descriptor,
                    e
                ));
            }
        }
    }
}

fn process_signals(
    mut signals: SignalsInfo,
    mutex_child_guard: Arc<Mutex<ChildGuard>>,
) -> Result<()> {
    // unwrap: Safe because `signals.forever()` is never empty.
    // unwrap: Safe because this instance of `signals` only receives `SIGTERM`, `SIGINT`,
    // and `SIGHUP`, and they are guaranteed to parse into a valid signal.
    let signal = Signal::try_from(signals.forever().next().unwrap()).unwrap();
    // unwrap: Safe because whenever this thread is running, we're waiting for it to
    // finish, and we're never holding the lock.
    kill_child_process_with_grace_period(&mut mutex_child_guard.lock().unwrap().child, signal)?;
    Ok(())
}
