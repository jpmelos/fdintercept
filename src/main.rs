use anyhow::{Context, Result};
use clap::Parser;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use non_empty_string::NonEmptyString;
use nonempty::NonEmpty;
use serde::Deserialize;
use signal_hook::consts::{SIGCHLD, SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::{Signals, SignalsInfo};
use std::env;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, ScopedJoinHandle};
use std::time::Duration;
use wait_timeout::ChildExt;

#[derive(Parser)]
#[command(about, version)]
struct CliArgs {
    #[arg(long)]
    conf: Option<PathBuf>,

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
    child
        .wait_timeout(Duration::from_secs(5))
        .context("Error waiting for child process")?
        .ok_or_else(|| anyhow::anyhow!("Sent SIGKILL, child still alive"))
}

fn main() -> Result<()> {
    let mut signals = Signals::new([SIGHUP, SIGINT, SIGTERM, SIGCHLD])
        .context("Failed to register signal handlers")?;

    let cli_args = CliArgs::parse();
    let target_env_var = match env::var("FDINTERCEPT_TARGET") {
        Ok(env_var) => Some(env_var),
        Err(std::env::VarError::NotPresent) => None,
        Err(e) => {
            eprintln!(
                "Error reading FDINTERCEPT_TARGET environment variable: {:?}",
                e
            );
            None
        }
    };
    let config = get_config(&cli_args).context("Error reading configuration")?;

    let target = get_target(&cli_args, &target_env_var, &config).context("Error getting target")?;

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

    // Don't even start the child process if we were already told to terminate. We can ignore
    // `SIGCHLD` here since we don't have a child process yet.
    if let Some(signum) = signals.pending().next() {
        std::process::exit(128 + signum);
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
        let (tx, rx) = mpsc::channel();

        spawn_thread_in_scope(scope, tx.clone(), "process_fd:stdin", || {
            process_fd(io::stdin(), child_stdin, stdin_log, "stdin")
        });
        spawn_thread_in_scope(scope, tx.clone(), "process_fd:stdout", || {
            process_fd(child_stdout, io::stdout(), stdout_log, "stdout")
        });
        spawn_thread_in_scope(scope, tx.clone(), "process_fd:stderr", || {
            process_fd(child_stderr, io::stderr(), stderr_log, "stderr")
        });
        spawn_thread_in_scope(scope, tx.clone(), "process_signals", || {
            process_signals(signals, mutex_child_guard.clone())
        });

        while let Ok((thread_name, handle)) = rx.recv() {
            match handle.join() {
                Ok(result) => match result {
                    Ok(_) => (),
                    Err(e) => eprintln!("Error in thread {}: {:?}", thread_name, e),
                },
                Err(e) => eprintln!("Error joining thread: {:?}", e),
            }
        }
    });

    std::process::exit(
        mutex_child_guard
            .lock()
            // unwrap: Safe because if we got here, the only other instance of `mutex_child_guard`
            // is dead, since it lived inside one of the threads that we already joined into.
            .unwrap()
            .child
            .try_wait()
            .context("Error waiting for child")?
            .map_or(1, |status| status.code().unwrap_or(1)),
    );
}

fn get_config(cli_args: &CliArgs) -> Result<Config> {
    if let Some(path) = cli_args.conf.clone() {
        return std::fs::read_to_string(&path)
            .context(format!(
                "Error reading configuration file {}",
                path.display()
            ))
            .and_then(|contents| parse_config_contents(&contents));
    }

    match env::var("FDINTERCEPTRC") {
        Ok(path) => {
            let path = PathBuf::from(path);
            match std::fs::read_to_string(&path) {
                Ok(contents) => {
                    return parse_config_contents(&contents);
                }
                Err(e) => {
                    return Err(e).context(format!(
                        "Error reading configuration file {}",
                        path.display()
                    ));
                }
            }
        }
        Err(std::env::VarError::NotPresent) => (),
        Err(e) => {
            eprintln!("Error reading FDINTERCEPTRC environment variable: {:?}", e);
        }
    };

    match env::var("HOME") {
        Ok(home) => {
            let home_path = PathBuf::from(home).join(".fdinterceptrc.toml");
            match std::fs::read_to_string(&home_path) {
                Ok(contents) => {
                    return parse_config_contents(&contents);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => {
                    return Err(e).context(format!(
                        "Error reading configuration file {}",
                        home_path.display()
                    ));
                }
            }
        }
        Err(std::env::VarError::NotPresent) => (),
        Err(e) => {
            eprintln!("Error reading HOME environment variable: {:?}", e);
        }
    };

    match env::var("XDG_CONFIG_HOME") {
        Ok(xdg_config_home) => {
            let xdg_path = PathBuf::from(xdg_config_home)
                .join("fdintercept")
                .join("rc.toml");
            match std::fs::read_to_string(&xdg_path) {
                Ok(contents) => {
                    return parse_config_contents(&contents);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => {
                    return Err(e).context(format!(
                        "Error reading configuration file {}",
                        xdg_path.display()
                    ));
                }
            }
        }
        Err(std::env::VarError::NotPresent) => (),
        Err(e) => {
            eprintln!(
                "Error reading XDG_CONFIG_HOME environment variable: {:?}",
                e
            );
        }
    };

    parse_config_contents("")
}

fn parse_config_contents(contents: &str) -> Result<Config> {
    toml::from_str(contents).context("Error parsing TOML configuration")
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
        return get_target_from_env_var(env_var)
            .context("Error getting target environment variable");
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
    cli_path: &Option<PathBuf>,
    config_path: &Option<PathBuf>,
    default_name: &str,
) -> Result<Option<File>> {
    let path = match (cli_path, config_path) {
        (Some(p), _) => Some(p.clone()),
        (None, Some(p)) => Some(p.clone()),
        (None, None) if use_defaults => Some(PathBuf::from(default_name)),
        _ => None,
    };
    match path {
        Some(p) => Ok(Some(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&p)
                .context(format!("Failed to create/open log file: {}", p.display()))?,
        )),
        None => Ok(None),
    }
}

fn spawn_thread_in_scope<'scope, F>(
    scope: &'scope thread::Scope<'scope, '_>,
    tx: mpsc::Sender<(&'static str, ScopedJoinHandle<'scope, Result<()>>)>,
    thread_name: &'static str,
    func: F,
) where
    F: FnOnce() -> Result<()> + Send + 'scope,
{
    let (handle_tx, handle_rx) = mpsc::channel();

    let handle = scope.spawn(move || {
        let result = func();
        // unwrap: Safe because `handle_tx` is guaranteed to have sent the handle.
        let handle = handle_rx.recv().unwrap();
        // unwrap: Safe because the receiving side is guaranteed to still be connected.
        tx.send((thread_name, handle)).unwrap();
        result
    });

    // unwrap: Safe because `handle_rx` is guaranteed to be connected.
    handle_tx.send(handle).unwrap();
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

        if logging_enabled {
            if let Err(e) = log.write_all(&buffer[..bytes_read]) {
                eprintln!(
                    "Error writing to {} log, disabling logging: {}",
                    log_descriptor, e
                );
                logging_enabled = false;
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
    if let signum @ (SIGHUP | SIGINT | SIGTERM) = signals.forever().next().unwrap() {
        kill_child_process_with_grace_period(
            // unwrap: Safe because if this thread is running, the main thread is waiting for it to
            // finish, so it can't be holding this lock.
            &mut mutex_child_guard.lock().unwrap().child,
            // unwrap: Safe because this instance of `signals` only receives `SIGHUP`, `SIGINT`,
            // `SIGTERM`, and `SIGCHLD`, and they are guaranteed to parse into a valid signal.
            Signal::try_from(signum).unwrap(),
        )?;
    }
    Ok(())
}
