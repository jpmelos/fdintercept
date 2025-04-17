use anyhow::{Context, Result};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use std::process::{Child, ExitStatus};
use std::time::Duration;
use wait_timeout::ChildExt;

pub struct ChildGuard {
    pub child: Child,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Err(e) = kill_child_process_with_grace_period(
            &mut self.child,
            Signal::SIGTERM,
            Duration::from_secs(15),
            Duration::from_secs(5),
        ) {
            eprintln!("Error cleaning up child process: {e}");
        }
    }
}

pub fn kill_child_process_with_grace_period(
    child: &mut Child,
    signal: Signal,
    grace_period: Duration,
    kill_deadline: Duration,
) -> Result<ExitStatus> {
    if let Some(status) = child
        .try_wait()
        .context("Error waiting for child process")?
    {
        return Ok(status);
    }

    // unwrap: `child.id` is a PID, so it's guaranteed to be well in the range of `i32`.
    kill(Pid::from_raw(i32::try_from(child.id()).unwrap()), signal)
        .context("Error sending signal to child process")?;

    if let Some(status) = child
        .wait_timeout(grace_period)
        .context("Error waiting for child process")?
    {
        return Ok(status);
    }

    child
        .kill()
        .context("Error sending signal to child process")?;
    child
        .wait_timeout(kill_deadline)
        .context("Error waiting for child process")?
        .ok_or_else(|| anyhow::anyhow!("Sent SIGKILL, child still alive"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::thread;

    mod child_guard_trait_drop {
        use super::*;

        #[test]
        fn drop() {
            let child = Command::new("sleep").arg("30").spawn().unwrap();
            let pid = child.id();

            {
                let _guard = ChildGuard { child };
            }

            thread::sleep(Duration::from_millis(100));
            assert!(
                Command::new("kill")
                    .arg("-0")
                    .arg(pid.to_string())
                    .status()
                    .unwrap()
                    .code()
                    .unwrap()
                    != 0
            );
        }
    }

    mod kill_child_process_with_grace_period {
        use super::*;
        use std::io::Read;
        use std::{os::unix::process::ExitStatusExt, process::Stdio};

        #[test]
        fn kill_with_signal() {
            let mut child = Command::new("sleep").arg("30").spawn().unwrap();

            let status = kill_child_process_with_grace_period(
                &mut child,
                Signal::SIGTERM,
                Duration::from_millis(100),
                Duration::from_millis(100),
            )
            .unwrap();
            assert!(!status.success());
            assert_eq!(status.signal().unwrap(), Signal::SIGTERM as i32);
        }

        #[test]
        fn child_ignores_signal() {
            dbg!("0");
            let mut child = Command::new("bash")
                .arg("-c")
                .arg("trap '' TERM; echo ready; while true; do sleep 0.1; done")
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();

            let mut stdout = child.stdout.take().unwrap();
            let mut buffer = [0; 6]; // "ready\n"
            stdout.read_exact(&mut buffer).unwrap();

            let status = kill_child_process_with_grace_period(
                &mut child,
                Signal::SIGTERM,
                Duration::from_millis(1),
                Duration::from_millis(100),
            )
            .unwrap();
            assert!(!status.success());
            assert_eq!(status.signal().unwrap(), Signal::SIGKILL as i32);
        }

        #[test]
        fn child_already_dead() {
            let mut child = Command::new("true").spawn().unwrap();
            thread::sleep(Duration::from_millis(100));
            let status = kill_child_process_with_grace_period(
                &mut child,
                Signal::SIGTERM,
                Duration::from_millis(1),
                Duration::from_millis(1),
            )
            .unwrap();
            assert!(status.success());
        }
    }
}
