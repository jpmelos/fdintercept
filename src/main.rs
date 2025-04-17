mod fd;
mod process;
mod settings;
mod signals;
mod threads;

use anyhow::{Context, Result};
use nix::unistd::pipe;
use process::ChildGuard;
use signal_hook::consts::{SIGCHLD, SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use std::io;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

fn main() -> Result<()> {
    let mut signals = Signals::new([SIGHUP, SIGINT, SIGTERM, SIGCHLD])
        .context("Failed to register signal handlers")?;

    let settings = settings::get_settings()?;

    let stdin_log = fd::create_log_file(settings.stdin_log.as_ref(), settings.recreate_logs)?;
    let stdout_log = fd::create_log_file(settings.stdout_log.as_ref(), settings.recreate_logs)?;
    let stderr_log = fd::create_log_file(settings.stderr_log.as_ref(), settings.recreate_logs)?;

    // Don't even start the child process if we were already told to terminate.
    if let Some(signum) = signals.pending().next() {
        std::process::exit(128 + signum);
    }

    // We're using a pipe here, instead of a mpsc::channel, because pipes have file descriptors
    // that we can wait on with `poll`.
    let (signal_rx, signal_tx) = pipe().context("Error creating pipe")?;

    let mut child_guard = ChildGuard {
        child: Command::new(settings.target.executable.as_str())
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

    thread::scope(move |scope| -> Result<()> {
        let (handle_tx, handle_rx) = mpsc::channel();

        threads::spawn_self_shipping_thread_in_scope(
            scope,
            handle_tx.clone(),
            "process_fd:stdin",
            move || {
                fd::process_fd(
                    io::stdin(),
                    child_stdin,
                    settings.buffer_size,
                    stdin_log,
                    "stdin",
                    Some(signal_rx),
                )
            },
        )
        .context("Failed to create thread to process stdin")?;
        threads::spawn_self_shipping_thread_in_scope(
            scope,
            handle_tx.clone(),
            "process_fd:stdout",
            move || {
                fd::process_fd(
                    child_stdout,
                    io::stdout(),
                    settings.buffer_size,
                    stdout_log,
                    "stdout",
                    None,
                )
            },
        )
        .context("Failed to create thread to process stdout")?;
        threads::spawn_self_shipping_thread_in_scope(
            scope,
            handle_tx.clone(),
            "process_fd:stderr",
            move || {
                fd::process_fd(
                    child_stderr,
                    io::stderr(),
                    settings.buffer_size,
                    stderr_log,
                    "stderr",
                    None,
                )
            },
        )
        .context("Failed to create thread to process stderr")?;
        threads::spawn_self_shipping_thread_in_scope(
            scope,
            handle_tx.clone(),
            "process_signals",
            || signals::process_signals(signals, mutex_child_guard_clone, signal_tx),
        )
        .context("Failed to create thread to process signals")?;

        // Close this `handle_tx` so that when all the self-shipping threads are finished and all
        // the `handle_tx` clones are dropped, `handle_rx` will return `Err`.
        drop(handle_tx);

        while let Ok((thread_name, handle)) = handle_rx.recv() {
            match handle.join() {
                Ok(result) => match result {
                    Ok(()) => (),
                    Err(e) => eprintln!("Error in thread {thread_name}: {e}"),
                },
                Err(e) => eprintln!("Error joining thread: {e:?}"),
            }
        }

        Ok(())
    })
    .context("Failed to create threads")?;

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
