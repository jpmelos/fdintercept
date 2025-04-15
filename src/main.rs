mod settings;

use anyhow::{Context, Result};
use nix::fcntl::{self, OFlag};
use nix::sys::signal::{Signal, kill};
use nix::unistd::{Pid, pipe};
use signal_hook::consts::{SIGCHLD, SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::{Signals, SignalsInfo};
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

    let settings = settings::get_settings()?;

    let stdin_log = create_log_file(&settings.stdin_log, settings.recreate_logs)?;
    let stdout_log = create_log_file(&settings.stdout_log, settings.recreate_logs)?;
    let stderr_log = create_log_file(&settings.stderr_log, settings.recreate_logs)?;

    // Don't even start the child process if we were already told to terminate.
    if let Some(signum) = signals.pending().next() {
        std::process::exit(128 + signum);
    }

    // We're using a pipe here, instead of a mpsc::channel, because pipes have file
    // descriptors that we can wait on with `poll`.
    let (signal_rx, signal_tx) = pipe().context("Error creating pipe")?;

    let mut child_guard = ChildGuard {
        child: Command::new(String::from(settings.target.executable))
            .args(&settings.target.args)
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
                    settings.buffer_size,
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
                    settings.buffer_size,
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
                    settings.buffer_size,
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

fn create_log_file(maybe_path: &Option<PathBuf>, recreate_logs: bool) -> Result<Option<File>> {
    let path = match maybe_path {
        Some(p) => p,
        None => return Ok(None),
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context(format!(
            "Failed to create parent directories to log file {}",
            path.display()
        ))?;
    }

    let mut options = OpenOptions::new();
    options.create(true).write(true);
    if recreate_logs {
        options.truncate(true);
    } else {
        options.append(true);
    }
    Ok(Some(options.open(path).context(format!(
        "Failed to create/open log file: {}",
        path.display()
    ))?))
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
