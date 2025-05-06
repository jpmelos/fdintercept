//! Signal handling functionality for managing child process termination.
//!
//! This module provides functionality for handling Unix signals (`SIGHUP`, `SIGINT`, `SIGTERM`) and
//! gracefully terminating child processes when these signals are received.

use crate::process::{self, ChildGuard};
use anyhow::Result;
use nix::sys::signal::Signal;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::SignalsInfo;
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Processes incoming Unix signals and handles child process termination.
///
/// This function waits for signals (`SIGHUP`, `SIGINT`, or `SIGTERM`) and attempts to gracefully
/// terminate the child process when one is received. After signal processing, it notifies the main
/// thread through a file descriptor.
///
/// # Arguments
///
/// * `signals` - Signal iterator providing incoming Unix signals.
/// * `mutex_child_guard` - Thread-safe reference to the child process guard.
/// * `signal_tx` - File descriptor for notifying the main thread of signal processing completion.
///
/// # Returns
///
/// Returns `Ok(())` if signal processing and child termination are successful, or an error if the
/// child process cannot be terminated properly.
///
/// # Signal Handling
///
/// The function handles these signals:
/// - `SIGHUP`: Terminal disconnect.
/// - `SIGINT`: Interrupt (usually Ctrl+C).
/// - `SIGTERM`: Termination request.
///
/// When any of these signals are received, the function:
/// 1. Attempts to gracefully terminate the child process, and
/// 2. Notifies the main thread through the `signal_tx` file descriptor.
pub fn process_signals(
    mut signals: SignalsInfo,
    mutex_child_guard: Arc<Mutex<ChildGuard>>,
    signal_tx: OwnedFd,
) -> Result<()> {
    // If we got a SIGCHLD, there's no need to run `process::kill_child_process_with_grace_period`
    // since the child process is already dead.
    // unwrap: Safe because `signals.forever()` is never empty.
    if let signum @ (SIGHUP | SIGINT | SIGTERM) = signals.forever().next().unwrap() {
        process::kill_child_process_with_grace_period(
            // unwrap: Safe because if this thread is running, the main thread is waiting for it to
            // finish, so it can't be holding this lock.
            &mut mutex_child_guard.lock().unwrap().child,
            // unwrap: Safe because this if statement only processes `SIGHUP`, `SIGINT`, and
            // `SIGTERM`, and they are guaranteed to parse into a valid signal.
            Signal::try_from(signum).unwrap(),
            Duration::from_secs(15),
            Duration::from_secs(5),
        )?;
    }
    // We don't care about an error here, because either the receiving end is still waiting to get
    // a message, or it has been already closed because the thread that owns it already died, and
    // then we don't care.
    let _ = nix::unistd::write(signal_tx, &[1]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    mod process_signals {
        use super::*;
        use nix::unistd::pipe;
        use signal_hook::iterator::Signals;
        use std::os::fd::AsFd;
        use std::os::unix::process::ExitStatusExt;
        use std::process::Command;

        #[test]
        fn process_signal() {
            let (signal_rx, signal_tx) = pipe().unwrap();

            let child_guard = Arc::new(Mutex::new(ChildGuard {
                child: Command::new("sleep").arg("30").spawn().unwrap(),
            }));

            let signals = Signals::new([SIGTERM]).unwrap();
            nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(i32::try_from(std::process::id()).unwrap()),
                Signal::SIGTERM,
            )
            .unwrap();

            process_signals(signals, child_guard.clone(), signal_tx).unwrap();

            let status = child_guard.lock().unwrap().child.wait().unwrap();
            assert!(!status.success());
            assert_eq!(status.signal().unwrap(), Signal::SIGTERM as i32);

            let mut buf = [0; 1];
            assert_eq!(nix::unistd::read(signal_rx.as_fd(), &mut buf).unwrap(), 1);
        }

        #[test]
        fn process_signals_closed_pipe() {
            let (signal_rx, signal_tx) = pipe().unwrap();

            let child_guard = Arc::new(Mutex::new(ChildGuard {
                child: Command::new("sleep").arg("30").spawn().unwrap(),
            }));

            let signals = Signals::new([SIGTERM]).unwrap();
            nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(i32::try_from(std::process::id()).unwrap()),
                Signal::SIGTERM,
            )
            .unwrap();

            drop(signal_rx);

            process_signals(signals, child_guard.clone(), signal_tx).unwrap();

            let status = child_guard.lock().unwrap().child.wait().unwrap();
            assert!(!status.success());
            assert_eq!(status.signal().unwrap(), Signal::SIGTERM as i32);
        }
    }
}
