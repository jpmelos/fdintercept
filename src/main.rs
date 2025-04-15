use anyhow::{Context, Result};
use clap::Parser;
use nix::fcntl::{self, OFlag};
use nix::sys::signal::{Signal, kill};
use nix::unistd::{Pid, pipe};
use non_empty_string::NonEmptyString;
use nonempty::NonEmpty;
use serde::Deserialize;
use signal_hook::consts::{SIGCHLD, SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::{Signals, SignalsInfo};
use std::env;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::io::AsRawFd;
use std::os::unix::process::ExitStatusExt;
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
    /// Path to a configuration file. If relative, this is relative to the current working
    /// directory.
    #[arg(long)]
    conf: Option<PathBuf>,

    /// Filename of the log file that will record stdin traffic. If relative, this is relative to
    /// the current working directory. Default: stdin.log.
    #[arg(long)]
    stdin_log: Option<PathBuf>,

    /// Filename of the log file that will record stdout traffic. If relative, this is relative to
    /// the current working directory. Default: stdout.log.
    #[arg(long)]
    stdout_log: Option<PathBuf>,

    /// Filename of the log file that will record stderr traffic. If relative, this is relative to
    /// the current working directory. Default: stderr.log.
    #[arg(long)]
    stderr_log: Option<PathBuf>,

    /// Re-create log files instead of appending to them. Default: false.
    #[arg(long)]
    recreate_logs: bool,

    /// Size in bytes of the buffer used for I/O operations. Default: 8 KiB.
    #[arg(long)]
    buffer_size: Option<usize>,

    /// The target command that will be executed.
    #[arg(last = true)]
    target: Vec<String>,
}

struct EnvVars {
    conf: Option<PathBuf>,
    recreate_logs: Option<bool>,
    buffer_size: Option<usize>,
    target: Option<String>,
}

#[derive(Deserialize)]
struct Config {
    stdin_log: Option<PathBuf>,
    stdout_log: Option<PathBuf>,
    stderr_log: Option<PathBuf>,
    recreate_logs: Option<bool>,
    buffer_size: Option<usize>,
    target: Option<String>,
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
    let env_vars = get_env_vars().context("Error reading environment variables")?;
    let config = get_config(&cli_args, &env_vars).context("Error reading configuration")?;

    let recreate_logs = get_recreate_logs(&cli_args, &env_vars, &config);
    let buffer_size = get_buffer_size(&cli_args, &env_vars, &config);
    let target = get_target(&cli_args, &env_vars, &config).context("Error getting target")?;

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
        recreate_logs,
        "stdin.log",
    )?;
    let stdout_log = create_log_file(
        use_defaults,
        &cli_args.stdout_log,
        &config.stdout_log,
        recreate_logs,
        "stdout.log",
    )?;
    let stderr_log = create_log_file(
        use_defaults,
        &cli_args.stderr_log,
        &config.stderr_log,
        recreate_logs,
        "stderr.log",
    )?;

    // Don't even start the child process if we were already told to terminate.
    if let Some(signum) = signals.pending().next() {
        std::process::exit(128 + signum);
    }

    // We're using a pipe here, instead of a mpsc::channel, because pipes have file
    // descriptors that we can wait on with `poll`.
    let (signal_rx, signal_tx) = pipe().context("Error creating pipe")?;

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
    let mutex_child_guard_clone = mutex_child_guard.clone();

    thread::scope(move |scope| {
        let (handle_tx, handle_rx) = mpsc::channel();

        spawn_self_shipping_thread_in_scope(
            scope,
            handle_tx.clone(),
            "process_fd:stdin",
            move || {
                process_fd(
                    io::stdin(),
                    child_stdin,
                    buffer_size,
                    stdin_log,
                    "stdin",
                    Some(signal_rx),
                )
            },
        );
        spawn_self_shipping_thread_in_scope(
            scope,
            handle_tx.clone(),
            "process_fd:stdout",
            move || {
                process_fd(
                    child_stdout,
                    io::stdout(),
                    buffer_size,
                    stdout_log,
                    "stdout",
                    None,
                )
            },
        );
        spawn_self_shipping_thread_in_scope(
            scope,
            handle_tx.clone(),
            "process_fd:stderr",
            move || {
                process_fd(
                    child_stderr,
                    io::stderr(),
                    buffer_size,
                    stderr_log,
                    "stderr",
                    None,
                )
            },
        );
        spawn_self_shipping_thread_in_scope(scope, handle_tx.clone(), "process_signals", || {
            process_signals(signals, mutex_child_guard_clone, signal_tx)
        });

        drop(handle_tx);

        while let Ok((thread_name, handle)) = handle_rx.recv() {
            match handle.join() {
                Ok(result) => match result {
                    Ok(_) => (),
                    Err(e) => eprintln!("Error in thread {}: {}", thread_name, e),
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
            .map_or(1, |status| {
                if let Some(code) = status.code() {
                    code
                } else if let Some(signum) = status.signal() {
                    128 + signum
                } else {
                    eprintln!("Error getting child process status");
                    1
                }
            }),
    );
}

fn get_env_vars() -> Result<EnvVars> {
    Ok(EnvVars {
        conf: {
            match env::var("FDINTERCEPTRC") {
                Ok(env_var) => {
                    if env_var.is_empty() {
                        return Err(anyhow::anyhow!("FDINTERCEPTRC is empty"));
                    }
                    Some(PathBuf::from(env_var))
                }
                Err(std::env::VarError::NotPresent) => None,
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "Error reading FDINTERCEPTRC environment variable: {}",
                        e
                    ));
                }
            }
        },
        recreate_logs: {
            match env::var("FDINTERCEPT_RECREATE_LOGS") {
                Ok(env_var) => match env_var.parse() {
                    Ok(recreate_logs) => Some(recreate_logs),
                    Err(e) => {
                        return Err(anyhow::anyhow!(
                            "Error parsing FDINTERCEPT_RECREATE_LOGS environment variable: {}",
                            e
                        ));
                    }
                },
                Err(std::env::VarError::NotPresent) => None,
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "Error reading FDINTERCEPT_RECREATE_LOGS environment variable: {}",
                        e
                    ));
                }
            }
        },
        buffer_size: {
            match env::var("FDINTERCEPT_BUFFER_SIZE") {
                Ok(env_var) => match env_var.parse() {
                    Ok(buffer_size) => Some(buffer_size),
                    Err(e) => {
                        return Err(anyhow::anyhow!(
                            "Error parsing FDINTERCEPT_BUFFER_SIZE environment variable: {}",
                            e
                        ));
                    }
                },
                Err(std::env::VarError::NotPresent) => None,
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "Error reading FDINTERCEPT_BUFFER_SIZE environment variable: {}",
                        e
                    ));
                }
            }
        },
        target: {
            match env::var("FDINTERCEPT_TARGET") {
                Ok(env_var) => Some(env_var),
                Err(std::env::VarError::NotPresent) => None,
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "Error reading FDINTERCEPT_TARGET environment variable: {}",
                        e
                    ));
                }
            }
        },
    })
}

fn get_config(cli_args: &CliArgs, env_vars: &EnvVars) -> Result<Config> {
    if let Some(ref path) = cli_args.conf {
        return std::fs::read_to_string(path)
            .context(format!(
                "Error reading configuration file {}",
                path.display()
            ))
            .and_then(|contents| parse_config_contents(&contents));
    }

    if let Some(ref path) = env_vars.conf {
        return std::fs::read_to_string(path)
            .context(format!(
                "Error reading configuration file {}",
                path.display()
            ))
            .and_then(|contents| parse_config_contents(&contents));
    }

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
            eprintln!("Error reading HOME environment variable: {}", e);
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
            eprintln!("Error reading XDG_CONFIG_HOME environment variable: {}", e);
        }
    };

    parse_config_contents("")
}

fn parse_config_contents(contents: &str) -> Result<Config> {
    toml::from_str(contents).context("Error parsing TOML configuration")
}

fn get_target(cli_args: &CliArgs, env_vars: &EnvVars, config: &Config) -> Result<Target> {
    match get_target_from_cli_arg(&cli_args.target) {
        Ok(target) => return Ok(target),
        Err(CliArgsTargetParseError::NotDefined) => (),
        Err(e) => return Err(e).context("Error getting target from CLI arguments"),
    };

    if let Some(ref target) = env_vars.target {
        match get_target_from_string(target) {
            Ok(target) => return Ok(target),
            Err(e) => {
                return Err(e)
                    .context("Error getting target from FDINTERCEPT_TARGET environment variable");
            }
        }
    }

    if let Some(ref target) = config.target {
        match get_target_from_string(target) {
            Ok(target) => return Ok(target),
            Err(e) => return Err(e).context("Error getting target from configuration file"),
        }
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
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::NotDefined => write!(f, "Target is not defined"),
            Self::EmptyExecutable => write!(f, "Target executable cannot be empty"),
        }
    }
}

impl std::error::Error for CliArgsTargetParseError {}

fn get_target_from_cli_arg(cli_arg: &[String]) -> Result<Target, CliArgsTargetParseError> {
    let target_vec = NonEmpty::from_slice(cli_arg).ok_or(CliArgsTargetParseError::NotDefined)?;
    Ok(Target {
        executable: NonEmptyString::new(target_vec.head)
            .map_err(|_| CliArgsTargetParseError::EmptyExecutable)?,
        args: target_vec.tail,
    })
}

#[derive(Debug)]
enum StringTargetParseError {
    Empty,
    FailedToTokenize,
    EmptyExecutable,
}

impl std::fmt::Display for StringTargetParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::FailedToTokenize => write!(f, "Failed to tokenize target"),
            Self::Empty => write!(f, "Target cannot be empty"),
            Self::EmptyExecutable => write!(f, "Target executable cannot be empty"),
        }
    }
}

impl std::error::Error for StringTargetParseError {}

fn get_target_from_string(target: &str) -> Result<Target, StringTargetParseError> {
    if target.is_empty() {
        return Err(StringTargetParseError::Empty);
    }
    let tokenized_target = shlex::split(target).ok_or(StringTargetParseError::FailedToTokenize)?;
    // unwrap: Safe because we already ensure that target is not empty.
    let target_vec = NonEmpty::from_vec(tokenized_target).unwrap();
    Ok(Target {
        executable: NonEmptyString::new(target_vec.head)
            .map_err(|_| StringTargetParseError::EmptyExecutable)?,
        args: target_vec.tail,
    })
}

fn get_recreate_logs(cli_args: &CliArgs, env_vars: &EnvVars, config: &Config) -> bool {
    cli_args.recreate_logs
        || env_vars
            .recreate_logs
            .or(config.recreate_logs)
            .unwrap_or(false)
}

fn get_buffer_size(cli_args: &CliArgs, env_vars: &EnvVars, config: &Config) -> usize {
    cli_args
        .buffer_size
        .or(env_vars.buffer_size)
        .or(config.buffer_size)
        .unwrap_or(8192)
}

fn create_log_file(
    use_defaults: bool,
    cli_path: &Option<PathBuf>,
    config_path: &Option<PathBuf>,
    recreate_logs: bool,
    default_name: &str,
) -> Result<Option<File>> {
    let path = match (cli_path, config_path) {
        (Some(p), _) => Some(p.clone()),
        (None, Some(p)) => Some(p.clone()),
        (None, None) if use_defaults => Some(PathBuf::from(default_name)),
        _ => None,
    };
    match path {
        Some(p) => {
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).context(format!(
                    "Failed to create parent directories to log file {}",
                    p.display()
                ))?;
            }

            let mut options = OpenOptions::new();
            options.create(true).write(true);
            if recreate_logs {
                options.truncate(true);
            } else {
                options.append(true);
            }
            Ok(Some(options.open(&p).context(format!(
                "Failed to create/open log file: {}",
                p.display()
            ))?))
        }
        None => Ok(None),
    }
}

fn spawn_self_shipping_thread_in_scope<'scope, F>(
    scope: &'scope thread::Scope<'scope, '_>,
    tx: mpsc::Sender<(&'static str, ScopedJoinHandle<'scope, Result<()>>)>,
    thread_name: &'static str,
    func: F,
) where
    F: FnOnce() -> Result<()> + Send + 'scope,
{
    let (handle_tx, handle_rx) = mpsc::channel();

    let handle = std::thread::Builder::new()
        .name(thread_name.to_string())
        .spawn_scoped(scope, move || {
            let result = func();
            // unwrap: Safe because `handle_tx` is guaranteed to have sent the handle.
            let handle = handle_rx.recv().unwrap();
            // unwrap: Safe because the receiving side is guaranteed to still be connected.
            tx.send((thread_name, handle)).unwrap();
            result
        })
        .unwrap();

    // unwrap: Safe because `handle_rx` is guaranteed to be connected.
    handle_tx.send(handle).unwrap();
}

const SRC_TOKEN: usize = 0;
const SIGNAL_TOKEN: usize = 1;

fn process_fd(
    mut src_fd: impl Read + AsRawFd,
    mut dst_fd: impl Write,
    buffer_size: usize,
    mut maybe_log: Option<File>,
    log_descriptor: &'static str,
    maybe_signal_rx: Option<OwnedFd>,
) -> Result<()> {
    let mut poll =
        set_up_poll(&src_fd, &maybe_signal_rx, log_descriptor).context("Error setting up poll")?;

    let mut pending_events = mio::Events::with_capacity(2);
    let mut buffer = vec![0; buffer_size];

    loop {
        poll.poll(&mut pending_events, Some(Duration::from_millis(100)))
            .context("Error polling for events")?;
        let events = pending_events.iter().collect();

        let mut event_outcomes = process_events_for_fd(
            events,
            &mut src_fd,
            &mut dst_fd,
            &mut buffer,
            &mut maybe_log,
        );

        match event_outcomes.swap_remove(0) {
            Ok(ProcessEventsForFdSuccess::DataLogged) => (),
            Ok(ProcessEventsForFdSuccess::Eof) => return Ok(()),
            Ok(ProcessEventsForFdSuccess::Signal) => return Ok(()),
            Err(ProcessEventsForFdError::Log(e)) => {
                eprintln!(
                    "Error writing to {} log, disabling logging: {}",
                    log_descriptor, e
                );
                maybe_log.take();
            }
            Err(e) => {
                return Err(e).context(format!(
                    "Error processing event for stream {}",
                    log_descriptor
                ));
            }
        }

        if event_outcomes.len() == 1 {
            // There was a signal event, and we already processed the fd readable event
            // that happened simultaneously. We can just return.
            return Ok(());
        }
    }
}

fn set_up_poll(
    src_fd: &impl AsRawFd,
    maybe_signal_rx: &Option<OwnedFd>,
    log_descriptor: &str,
) -> Result<mio::Poll> {
    let poll = mio::Poll::new().context("Error creating poll of events")?;

    register_fd_into_poll(&poll, src_fd, SRC_TOKEN).context(format!(
        "Error registering {} source stream in poll of events",
        log_descriptor
    ))?;

    if let Some(signal_rx) = maybe_signal_rx {
        register_fd_into_poll(&poll, signal_rx, SIGNAL_TOKEN)
            .context("Error registering signal pipe in poll of events")?;
    }

    Ok(poll)
}

fn register_fd_into_poll(poll: &mio::Poll, fd: &impl AsRawFd, token: usize) -> Result<()> {
    let raw_fd = fd.as_raw_fd();

    let flags = fcntl::fcntl(raw_fd, fcntl::F_GETFL).context("Error getting flags")?;
    fcntl::fcntl(
        raw_fd,
        fcntl::F_SETFL(OFlag::from_bits_truncate(flags as i32) | OFlag::O_NONBLOCK),
    )
    .context("Error setting source fd as non-blocking")?;

    poll.registry().register(
        &mut mio::unix::SourceFd(&raw_fd),
        mio::Token(token),
        mio::Interest::READABLE,
    )?;

    Ok(())
}

enum ProcessEventsForFdSuccess {
    DataLogged,
    Eof,
    Signal,
}

#[derive(Debug)]
enum ProcessEventsForFdError {
    Read(std::io::Error),
    Write(std::io::Error),
    Log(std::io::Error),
}

impl std::fmt::Display for ProcessEventsForFdError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Read(e) => write!(f, "Failed to read data: {}", e),
            Self::Write(e) => write!(f, "Failed to write data: {}", e),
            Self::Log(e) => write!(f, "Failed to log data: {}", e),
        }
    }
}

impl std::error::Error for ProcessEventsForFdError {}

fn process_events_for_fd(
    events: Vec<&mio::event::Event>,
    src_fd: &mut impl Read,
    dst_fd: &mut impl Write,
    buffer: &mut [u8],
    maybe_log: &mut Option<File>,
) -> Vec<Result<ProcessEventsForFdSuccess, ProcessEventsForFdError>> {
    if events.is_empty() {
        // We process this as an event readable.
        return vec![inner_fd_event_readable(src_fd, dst_fd, buffer, maybe_log)];
    }

    if events.len() == 1 {
        match events[0].token() {
            mio::Token(token) if token == SRC_TOKEN => {
                return vec![inner_fd_event_readable(src_fd, dst_fd, buffer, maybe_log)];
            }
            mio::Token(token) if token == SIGNAL_TOKEN => {
                return vec![Ok(ProcessEventsForFdSuccess::Signal)];
            }
            _ => unreachable!(),
        }
    }

    // There is a readable event for the fd, and a signal. We always want to process the readable
    // event first so we don't miss anything that should be logged, and then the signal, which will
    // kill the thread.
    vec![
        inner_fd_event_readable(src_fd, dst_fd, buffer, maybe_log),
        Ok(ProcessEventsForFdSuccess::Signal),
    ]
}

fn inner_fd_event_readable(
    src_fd: &mut impl Read,
    dst_fd: &mut impl Write,
    buffer: &mut [u8],
    maybe_log: &mut Option<File>,
) -> Result<ProcessEventsForFdSuccess, ProcessEventsForFdError> {
    let bytes_read = match src_fd.read(buffer) {
        Ok(0) => {
            return Ok(ProcessEventsForFdSuccess::Eof);
        }
        Ok(bytes_read) => bytes_read,
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
            return Ok(ProcessEventsForFdSuccess::DataLogged);
        }
        Err(e) => {
            return Err(ProcessEventsForFdError::Read(e));
        }
    };

    match dst_fd.write_all(&buffer[..bytes_read]) {
        Ok(_) => (),
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
            return Ok(ProcessEventsForFdSuccess::Eof);
        }
        Err(e) => {
            return Err(ProcessEventsForFdError::Write(e));
        }
    }

    if let Some(log) = maybe_log {
        if let Err(e) = log.write_all(&buffer[..bytes_read]) {
            return Err(ProcessEventsForFdError::Log(e));
        }
    }

    Ok(ProcessEventsForFdSuccess::DataLogged)
}

fn process_signals(
    mut signals: SignalsInfo,
    mutex_child_guard: Arc<Mutex<ChildGuard>>,
    signal_tx: OwnedFd,
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
    // We don't care about an error here, because either the receiving end is still waiting to get
    // a message, or it has been already closed because the thread that owned it already died, and
    // then we don't care.
    let _ = nix::unistd::write(signal_tx, &[1]);
    Ok(())
}
