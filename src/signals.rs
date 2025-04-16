use crate::process::{self, ChildGuard};
use anyhow::Result;
use nix::sys::signal::Signal;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::SignalsInfo;
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub(crate) fn process_signals(
    mut signals: SignalsInfo,
    mutex_child_guard: Arc<Mutex<ChildGuard>>,
    signal_tx: OwnedFd,
) -> Result<()> {
    // unwrap: Safe because `signals.forever()` is never empty.
    if let signum @ (SIGHUP | SIGINT | SIGTERM) = signals.forever().next().unwrap() {
        process::kill_child_process_with_grace_period(
            // unwrap: Safe because if this thread is running, the main thread is waiting for it to
            // finish, so it can't be holding this lock.
            &mut mutex_child_guard.lock().unwrap().child,
            // unwrap: Safe because this instance of `signals` only receives `SIGHUP`, `SIGINT`,
            // `SIGTERM`, and `SIGCHLD`, and they are guaranteed to parse into a valid signal.
            Signal::try_from(signum).unwrap(),
            Duration::from_secs(15),
            Duration::from_secs(5),
        )?;
    }
    // We don't care about an error here, because either the receiving end is still waiting to get
    // a message, or it has been already closed because the thread that owned it already died, and
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
        use std::os::fd::AsRawFd;
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
                nix::unistd::Pid::from_raw(std::process::id() as i32),
                Signal::SIGTERM,
            )
            .unwrap();

            let result = process_signals(signals, child_guard.clone(), signal_tx);
            assert!(result.is_ok());

            let status = child_guard.lock().unwrap().child.wait().unwrap();
            assert!(!status.success());
            assert_eq!(status.signal().unwrap(), Signal::SIGTERM as i32);

            let mut buf = [0; 1];
            assert_eq!(
                nix::unistd::read(signal_rx.as_raw_fd() as i32, &mut buf).unwrap(),
                1
            );
        }

        #[test]
        fn test_process_signals_closed_pipe() {
            let (signal_rx, signal_tx) = pipe().unwrap();

            let child_guard = Arc::new(Mutex::new(ChildGuard {
                child: Command::new("sleep").arg("30").spawn().unwrap(),
            }));

            let signals = Signals::new([SIGTERM]).unwrap();
            nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(std::process::id() as i32),
                Signal::SIGTERM,
            )
            .unwrap();

            drop(signal_rx);

            let result = process_signals(signals, child_guard.clone(), signal_tx);
            assert!(result.is_ok());

            let status = child_guard.lock().unwrap().child.wait().unwrap();
            assert!(!status.success());
            assert_eq!(status.signal().unwrap(), Signal::SIGTERM as i32);
        }
    }
}
