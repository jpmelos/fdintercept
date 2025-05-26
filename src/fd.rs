//! File descriptor handling and event processing functionality.
//!
//! This module provides utilities for managing file descriptors, including:
//! - Creating log files with specific permissions and modes,
//! - Processing file descriptor events in a non-blocking manner, and
//! - Handling signals and data transfer between file descriptors.

use anyhow::{Context, Result};
use nix::fcntl::{self, OFlag};
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::time::Duration;

/// Mio token that represents that the source file descriptor is ready to be read.
const SRC_TOKEN: usize = 0;
/// Mio token that represents that a signal has arrived.
const SIGNAL_TOKEN: usize = 1;

/// Represents different types of events that can occur during file descriptor polling.
#[derive(Debug)]
enum Event {
    /// Indicates that data is ready to be read from the file descriptor.
    FdReady,
    /// Indicates that a signal has been received.
    SignalReady,
}

impl Event {
    /// Converts a Mio token into the corresponding Event variant.
    ///
    /// # Arguments
    ///
    /// * `token` - The Mio token to convert.
    ///
    /// # Panics
    ///
    /// Panics if the token value is neither `SRC_TOKEN` nor `SIGNAL_TOKEN`.
    fn from_mio_token(token: mio::Token) -> Self {
        match token.0 {
            SRC_TOKEN => Self::FdReady,
            SIGNAL_TOKEN => Self::SignalReady,
            _ => unreachable!(),
        }
    }
}

/// Represents successful outcomes of processing file descriptor events.
#[derive(Debug)]
enum ProcessEventsForFdSuccess {
    /// Data was successfully read and written
    DataLogged,
    /// End of file was reached
    Eof,
    /// A signal was received
    Signal,
}

/// Represents errors that can occur during file descriptor event processing.
#[derive(Debug)]
enum ProcessEventsForFdError {
    /// Error occurred while reading from the source.
    Read(std::io::Error),
    /// Error occurred while writing to the destination.
    Write(std::io::Error),
    /// Error occurred while writing to the log file.
    Log(std::io::Error),
}

impl std::fmt::Display for ProcessEventsForFdError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Read(e) => write!(f, "Failed to read data: {e}"),
            Self::Write(e) => write!(f, "Failed to write data: {e}"),
            Self::Log(e) => write!(f, "Failed to log data: {e}"),
        }
    }
}

impl std::error::Error for ProcessEventsForFdError {}

/// Creates a log file with specified options.
///
/// # Arguments
///
/// * `maybe_path` - Optional path where the log file should be created.
/// * `recreate_logs` - If true, truncates existing log file; if false, appends to it.
///
/// # Returns
///
/// Returns `Ok(Some(impl Write))` if a path was provided and the file was successfully created, or
/// `Ok(None)` if no path was provided.
///
/// # Errors
///
/// Returns an error if:
/// - Failed to create parent directories,
/// - Failed to create or open the log file, or
/// - Insufficient permissions.
pub fn create_log_file(
    maybe_path: Option<&PathBuf>,
    recreate_logs: bool,
) -> Result<Option<impl Write>> {
    let Some(path) = maybe_path else {
        return Ok(None);
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

/// Processes a file descriptor, handling data transfer and optional logging.
///
/// # Arguments
///
/// * `src_fd` - Source file descriptor implementing `Read + AsRawFd`.
/// * `dst_fd` - Destination file descriptor implementing `Write`.
/// * `buffer_size` - Size of the buffer in bytes used for data transfer.
/// * `maybe_log` - Optional writer for logging the transferred data.
/// * `log_descriptor` - Static string describing the log for error messages.
/// * `maybe_signal_rx` - Optional owned file descriptor for signal handling.
///
/// # Returns
///
/// Returns `Ok(())` when processing completes successfully, either due to EOF or signal.
///
/// # Errors
///
/// Returns an error if:
/// - Failed to set up polling,
/// - Error occurred during polling, or
/// - Error processing events (except for log write errors, which disable logging).
pub fn process_fd(
    mut src_fd: impl Read + AsFd + AsRawFd,
    mut dst_fd: impl Write,
    buffer_size: usize,
    mut maybe_log: Option<impl Write>,
    log_descriptor: &'static str,
    maybe_signal_rx: Option<OwnedFd>,
) -> Result<()> {
    let mut poll = set_up_poll(&src_fd, maybe_signal_rx.as_ref(), log_descriptor)
        .context("Error setting up poll")?;

    let mut pending_events = mio::Events::with_capacity(2);
    let mut buffer = vec![0; buffer_size];

    loop {
        // If interrupted by a signal, just continue.
        match poll.poll(&mut pending_events, Some(Duration::from_millis(100))) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => (),
            Err(e) => return Err(e).context("Error polling for events"),
            _ => (),
        }

        let events = pending_events
            .iter()
            .map(|e| Event::from_mio_token(e.token()))
            .collect();

        let mut event_outcomes = process_events_for_fd(
            events,
            &mut src_fd,
            &mut dst_fd,
            &mut buffer,
            &mut maybe_log,
        );

        match event_outcomes.remove(0) {
            Ok(ProcessEventsForFdSuccess::DataLogged) => (),
            // If we got an EOF, this means that the stream is not open anymore and there will be
            // no more data flowing. Just let the thread die. If we got a signal, this means we
            // want to end the process.
            Ok(ProcessEventsForFdSuccess::Eof | ProcessEventsForFdSuccess::Signal) => return Ok(()),
            Err(ProcessEventsForFdError::Log(e)) => {
                eprintln!("Error writing to {log_descriptor} log, disabling logging: {e}");
                maybe_log.take();
            }
            Err(e) => {
                return Err(e).context(format!(
                    "Error processing event for stream {log_descriptor}"
                ));
            }
        }

        if event_outcomes.len() == 1 {
            // There was a signal event, and we already processed the fd readable event that
            // happened simultaneously. We can just return.
            return Ok(());
        }
    }
}

/// Sets up a poll instance for monitoring file descriptors.
///
/// # Arguments
///
/// * `src_fd` - Source file descriptor to monitor.
/// * `maybe_signal_rx` - Optional signal receiver file descriptor.
/// * `log_descriptor` - Description string for error messages.
///
/// # Returns
///
/// Returns the configured poll instance on success.
///
/// # Errors
///
/// Returns an error if:
/// - Failed to create new poll instance, or
/// - Failed to register file descriptors.
fn set_up_poll(
    src_fd: &(impl AsFd + AsRawFd),
    maybe_signal_rx: Option<&OwnedFd>,
    log_descriptor: &str,
) -> Result<mio::Poll> {
    let poll = mio::Poll::new().context("Error creating poll of events")?;

    register_fd_into_poll(&poll, src_fd, SRC_TOKEN).context(format!(
        "Error registering {log_descriptor} source stream in poll of events"
    ))?;

    if let Some(signal_rx) = maybe_signal_rx {
        register_fd_into_poll(&poll, signal_rx, SIGNAL_TOKEN)
            .context("Error registering signal pipe in poll of events")?;
    }

    Ok(poll)
}

/// Registers a file descriptor with a poll instance.
///
/// # Arguments
///
/// * `poll` - The poll instance to register with.
/// * `fd` - The file descriptor to register.
/// * `token` - The token to associate with this file descriptor.
///
/// # Returns
///
/// Returns `Ok(())` on successful registration.
///
/// # Errors
///
/// Returns an error if:
/// - Failed to get or set file descriptor flags, or
/// - Failed to register with poll instance.
fn register_fd_into_poll(poll: &mio::Poll, fd: &(impl AsFd + AsRawFd), token: usize) -> Result<()> {
    // All file descriptors that are used with Mio should be in non-blocking mode.
    let flags = fcntl::fcntl(fd, fcntl::F_GETFL).context("Error getting flags")?;
    fcntl::fcntl(
        fd,
        fcntl::F_SETFL(OFlag::from_bits_truncate(flags as i32) | OFlag::O_NONBLOCK),
    )
    .context("Error setting source fd as non-blocking")?;

    poll.registry().register(
        &mut mio::unix::SourceFd(&fd.as_raw_fd()),
        mio::Token(token),
        mio::Interest::READABLE,
    )?;

    Ok(())
}

/// Processes events for a file descriptor.
///
/// # Arguments
///
/// * `events` - Vector of events to process.
/// * `src_fd` - Source to read from.
/// * `dst_fd` - Destination to write to.
/// * `buffer` - Buffer for data transfer.
/// * `maybe_log` - Optional log writer.
///
/// # Returns
///
/// Returns a vector of results, one for each processed event.
fn process_events_for_fd(
    events: Vec<Event>,
    src_fd: &mut impl Read,
    dst_fd: &mut impl Write,
    buffer: &mut [u8],
    maybe_log: &mut Option<impl Write>,
) -> Vec<Result<ProcessEventsForFdSuccess, ProcessEventsForFdError>> {
    match events.len() {
        0 => vec![inner_fd_event_readable(src_fd, dst_fd, buffer, maybe_log)],
        // unwrap: Safe because we just checked whether `events.len()` is zero in the arm above.
        1 => match events.first().unwrap() {
            Event::FdReady => vec![inner_fd_event_readable(src_fd, dst_fd, buffer, maybe_log)],
            Event::SignalReady => vec![Ok(ProcessEventsForFdSuccess::Signal)],
        },
        // There is a readable event for the fd, and a signal. We always want to process the
        // readable event first so we don't miss anything that should be logged, and then the
        // signal, which will kill the thread.
        2 => vec![
            inner_fd_event_readable(src_fd, dst_fd, buffer, maybe_log),
            Ok(ProcessEventsForFdSuccess::Signal),
        ],
        _ => unreachable!("Poll can only return up to 2 events"),
    }
}

/// Handles a readable event for a file descriptor.
///
/// # Arguments
///
/// * `src_fd` - Source to read from.
/// * `dst_fd` - Destination to write to.
/// * `buffer` - Buffer for data transfer.
/// * `maybe_log` - Optional log writer.
///
/// # Returns
///
/// Returns the result of processing the readable event.
fn inner_fd_event_readable(
    src_fd: &mut impl Read,
    dst_fd: &mut impl Write,
    buffer: &mut [u8],
    maybe_log: &mut Option<impl Write>,
) -> Result<ProcessEventsForFdSuccess, ProcessEventsForFdError> {
    // Keep reading from the source fd until we get a `WouldBlock`.
    loop {
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
            Ok(()) => (),
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::BorrowedFd;
    use std::os::unix;

    struct MockRead {
        responses: Vec<io::Result<usize>>,
        current: usize,
    }

    impl Read for MockRead {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            match self.responses.get(self.current) {
                Some(Ok(n)) => {
                    self.current += 1;
                    Ok(*n)
                }
                Some(Err(e)) => {
                    self.current += 1;
                    Err(io::Error::new(e.kind(), e.to_string()))
                }
                None => Ok(0), // EOF when no more responses.
            }
        }
    }

    impl AsFd for MockRead {
        fn as_fd(&self) -> BorrowedFd {
            // SAFETY: This is not actually ever used to read from stdin in this test. This is here
            // just because we need to implement this trait for `MockRead` so we can call
            // `process_fd` with it.
            unsafe { BorrowedFd::borrow_raw(1) }
        }
    }

    impl AsRawFd for MockRead {
        fn as_raw_fd(&self) -> unix::io::RawFd {
            1
        }
    }

    struct MockWrite {
        responses: Vec<io::Result<()>>,
        current: usize,
        written_data: Vec<Vec<u8>>,
    }

    impl Write for MockWrite {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Ok(0)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
            self.written_data.push(buf.to_vec());
            match self.responses.get(self.current) {
                Some(Ok(())) => {
                    self.current += 1;
                    Ok(())
                }
                Some(Err(e)) => {
                    self.current += 1;
                    Err(io::Error::new(e.kind(), e.to_string()))
                }
                None => Ok(()),
            }
        }
    }

    mod create_log_file {
        use super::*;
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use tempfile::TempDir;

        #[test]
        fn none() {
            assert!(create_log_file(None, false).unwrap().is_none());
        }

        #[test]
        fn new_file_with_parent_dirs() {
            let temp_dir = TempDir::new().unwrap();
            let log_path = temp_dir.path().join("nested/dirs/test.log");

            let result = create_log_file(Some(&log_path), false).unwrap();

            assert!(result.is_some());
            assert!(log_path.exists());
        }

        #[test]
        fn existing_file_appends() {
            let temp_dir = TempDir::new().unwrap();
            let log_path = temp_dir.path().join("test.log");

            fs::write(&log_path, "initial content").unwrap();

            let mut file = create_log_file(Some(&log_path), false).unwrap().unwrap();
            file.write_all(b"appended content").unwrap();
            drop(file);

            let content = fs::read_to_string(&log_path).unwrap();
            assert_eq!(content, "initial contentappended content");
        }

        #[test]
        fn recreate() {
            let temp_dir = TempDir::new().unwrap();
            let log_path = temp_dir.path().join("test.log");

            fs::write(&log_path, "initial content").unwrap();

            let mut file = create_log_file(Some(&log_path), true).unwrap().unwrap();
            file.write_all(b"new content").unwrap();
            drop(file);

            let content = fs::read_to_string(&log_path).unwrap();
            assert_eq!(content, "new content");
        }

        #[test]
        fn permission_error() {
            let temp_dir = TempDir::new().unwrap();
            fs::set_permissions(temp_dir.path(), fs::Permissions::from_mode(0o444)).unwrap();
            let log_path = temp_dir.path().join("test.log");

            match create_log_file(Some(&log_path), false) {
                Ok(_) => panic!("Expected an error"),
                Err(e) => assert!(e.to_string().contains("Failed to create/open log file")),
            }
        }
    }

    mod process_fd {
        use super::*;
        use std::{cell::RefCell, rc::Rc};

        struct RefCellWriter(Rc<RefCell<MockWrite>>);

        impl Write for RefCellWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.0.borrow_mut().write(buf)
            }

            fn flush(&mut self) -> io::Result<()> {
                self.0.borrow_mut().flush()
            }

            fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
                self.0.borrow_mut().write_all(buf)
            }
        }

        #[test]
        fn success() {
            let src = MockRead {
                responses: vec![Ok(5), Ok(3), Ok(0)], // Some data then EOF.
                current: 0,
            };
            let dst = MockWrite {
                responses: vec![Ok(()), Ok(())],
                current: 0,
                written_data: vec![],
            };
            let log_file = MockWrite {
                responses: vec![Ok(()), Ok(())],
                current: 0,
                written_data: vec![],
            };

            let dst = Rc::new(RefCell::new(dst));
            let log_file = Rc::new(RefCell::new(log_file));

            process_fd(
                src,
                RefCellWriter(dst.clone()),
                1024,
                Some(RefCellWriter(log_file.clone())),
                "test",
                None,
            )
            .unwrap();

            assert_eq!(dst.borrow().written_data.len(), 2);
            assert_eq!(dst.borrow().written_data[0].len(), 5);
            assert_eq!(dst.borrow().written_data[1].len(), 3);
            assert_eq!(log_file.borrow().written_data.len(), 2);
            assert_eq!(log_file.borrow().written_data[0].len(), 5);
            assert_eq!(log_file.borrow().written_data[1].len(), 3);
        }
    }

    mod set_up_poll {
        use super::*;
        use nix::unistd::pipe;
        use std::fs::File;
        use std::os::fd::IntoRawFd;
        use std::os::unix::io::FromRawFd;

        #[test]
        fn success_without_signal() {
            let file = create_file_from_pipe();
            set_up_poll(&file, None, "test").unwrap();
        }

        #[test]
        fn success_with_signal() {
            let file = create_file_from_pipe();
            let (signal_rx, _signal_tx) = pipe().unwrap();
            set_up_poll(&file, Some(&signal_rx), "test").unwrap();
        }

        fn create_file_from_pipe() -> File {
            let (_, tx) = pipe().unwrap();
            // Convert to `File`.
            unsafe { File::from_raw_fd(tx.into_raw_fd()) }
        }
    }

    mod register_fd_into_poll {
        use super::*;
        use nix::unistd::pipe;
        use std::fs::File;
        use std::os::fd::IntoRawFd;
        use std::os::unix::io::FromRawFd;

        #[test]
        fn success() {
            register_fd_into_poll(&mio::Poll::new().unwrap(), &create_file_from_pipe(), 42)
                .unwrap();
        }

        fn create_file_from_pipe() -> File {
            let (_, tx) = pipe().unwrap();
            // Convert to `File`.
            unsafe { File::from_raw_fd(tx.into_raw_fd()) }
        }
    }

    mod process_events_for_fd {
        use super::*;
        use std::io::{Error, ErrorKind};

        #[test]
        fn no_events() {
            // These cases are probably EOF or WouldBlock.
            let mut src = MockRead {
                responses: vec![Ok(0)],
                current: 0,
            };
            let mut dst = MockWrite {
                responses: vec![Ok(())],
                current: 0,
                written_data: vec![],
            };

            let mut buffer = vec![0; 1024];
            let mut log_file: Option<MockWrite> = None;

            let events: Vec<Event> = vec![];
            let results =
                process_events_for_fd(events, &mut src, &mut dst, &mut buffer, &mut log_file);

            assert_eq!(results.len(), 1);
            assert!(matches!(results[0], Ok(ProcessEventsForFdSuccess::Eof)));
        }

        #[test]
        fn single_event_fd_ready() {
            let mut src = MockRead {
                responses: vec![Ok(5), Err(Error::new(ErrorKind::WouldBlock, "would block"))],
                current: 0,
            };
            let mut dst = MockWrite {
                responses: vec![Ok(())],
                current: 0,
                written_data: vec![],
            };

            let mut buffer = vec![0; 1024];
            let mut log_file: Option<MockWrite> = None;

            let events = vec![Event::FdReady];
            let results =
                process_events_for_fd(events, &mut src, &mut dst, &mut buffer, &mut log_file);

            assert_eq!(results.len(), 1);
            assert!(matches!(
                results[0],
                Ok(ProcessEventsForFdSuccess::DataLogged)
            ));
        }

        #[test]
        fn single_event_signal_ready() {
            let mut src = MockRead {
                responses: vec![],
                current: 0,
            };
            let mut dst = MockWrite {
                responses: vec![],
                current: 0,
                written_data: vec![],
            };
            let mut buffer = vec![0; 1024];
            let mut log_file: Option<MockWrite> = None;

            let events = vec![Event::SignalReady];
            let results =
                process_events_for_fd(events, &mut src, &mut dst, &mut buffer, &mut log_file);

            assert_eq!(results.len(), 1);
            assert!(matches!(results[0], Ok(ProcessEventsForFdSuccess::Signal)));
        }

        #[test]
        fn both_events_fd_first() {
            let mut src = MockRead {
                responses: vec![Ok(5), Err(Error::new(ErrorKind::WouldBlock, "would block"))],
                current: 0,
            };
            let mut dst = MockWrite {
                responses: vec![Ok(())],
                current: 0,
                written_data: vec![],
            };

            let mut buffer = vec![0; 1024];
            let mut log_file: Option<MockWrite> = None;

            let events = vec![Event::FdReady, Event::SignalReady];
            let results =
                process_events_for_fd(events, &mut src, &mut dst, &mut buffer, &mut log_file);

            assert_eq!(results.len(), 2);
            assert!(matches!(
                results[0],
                Ok(ProcessEventsForFdSuccess::DataLogged)
            ));
            assert!(matches!(results[1], Ok(ProcessEventsForFdSuccess::Signal)));
        }

        #[test]
        fn both_events_signal_first() {
            let mut src = MockRead {
                responses: vec![Ok(5), Err(Error::new(ErrorKind::WouldBlock, "would block"))],
                current: 0,
            };
            let mut dst = MockWrite {
                responses: vec![Ok(())],
                current: 0,
                written_data: vec![],
            };

            let mut buffer = vec![0; 1024];
            let mut log_file: Option<MockWrite> = None;

            let events = vec![Event::SignalReady, Event::FdReady];
            let results =
                process_events_for_fd(events, &mut src, &mut dst, &mut buffer, &mut log_file);

            assert_eq!(results.len(), 2);
            assert!(matches!(
                results[0],
                Ok(ProcessEventsForFdSuccess::DataLogged)
            ));
            assert!(matches!(results[1], Ok(ProcessEventsForFdSuccess::Signal)));
        }
    }

    mod inner_fd_event_readable {
        use super::*;
        use std::io::{Error, ErrorKind};

        #[test]
        fn success_with_log() {
            let mut src = MockRead {
                responses: vec![Ok(5), Err(Error::new(ErrorKind::WouldBlock, "would block"))],
                current: 0,
            };
            let mut dst = MockWrite {
                responses: vec![Ok(())],
                current: 0,
                written_data: vec![],
            };

            let mut buffer = vec![0; 1024];
            let mut log_file = Some(MockWrite {
                responses: vec![Ok(())],
                current: 0,
                written_data: vec![],
            });

            assert!(matches!(
                inner_fd_event_readable(&mut src, &mut dst, &mut buffer, &mut log_file),
                Ok(ProcessEventsForFdSuccess::DataLogged)
            ));
            assert_eq!(dst.written_data.len(), 1);
            assert_eq!(dst.written_data[0].len(), 5);
            assert_eq!(log_file.as_ref().unwrap().written_data.len(), 1);
            assert_eq!(log_file.as_ref().unwrap().written_data[0].len(), 5);
        }

        #[test]
        fn success_without_log() {
            let mut src = MockRead {
                responses: vec![Ok(5), Err(Error::new(ErrorKind::WouldBlock, "would block"))],
                current: 0,
            };
            let mut dst = MockWrite {
                responses: vec![Ok(())],
                current: 0,
                written_data: vec![],
            };

            let mut buffer = vec![0; 1024];
            let mut log_file: Option<MockWrite> = None;

            assert!(matches!(
                inner_fd_event_readable(&mut src, &mut dst, &mut buffer, &mut log_file),
                Ok(ProcessEventsForFdSuccess::DataLogged)
            ));
            assert_eq!(dst.written_data.len(), 1);
            assert_eq!(dst.written_data[0].len(), 5);
        }

        #[test]
        fn eof_on_read() {
            let mut src = MockRead {
                responses: vec![Ok(0)],
                current: 0,
            };
            let mut dst = MockWrite {
                responses: vec![],
                current: 0,
                written_data: vec![],
            };

            let mut buffer = vec![0; 1024];
            let mut log_file: Option<MockWrite> = None;

            assert!(matches!(
                inner_fd_event_readable(&mut src, &mut dst, &mut buffer, &mut log_file),
                Ok(ProcessEventsForFdSuccess::Eof)
            ));
        }

        #[test]
        fn error_on_read() {
            let mut src = MockRead {
                responses: vec![Err(Error::other("read error"))],
                current: 0,
            };
            let mut dst = MockWrite {
                responses: vec![],
                current: 0,
                written_data: vec![],
            };

            let mut buffer = vec![0; 1024];
            let mut log_file: Option<MockWrite> = None;

            assert!(matches!(
                inner_fd_event_readable(&mut src, &mut dst, &mut buffer, &mut log_file),
                Err(ProcessEventsForFdError::Read(_))
            ));
        }

        #[test]
        fn broken_pipe_on_write() {
            let mut src = MockRead {
                responses: vec![Ok(5)],
                current: 0,
            };
            let mut dst = MockWrite {
                responses: vec![Err(Error::new(ErrorKind::BrokenPipe, "broken pipe"))],
                current: 0,
                written_data: vec![],
            };

            let mut buffer = vec![0; 1024];
            let mut log_file: Option<MockWrite> = None;

            assert!(matches!(
                inner_fd_event_readable(&mut src, &mut dst, &mut buffer, &mut log_file),
                Ok(ProcessEventsForFdSuccess::Eof)
            ));
        }

        #[test]
        fn error_on_write() {
            let mut src = MockRead {
                responses: vec![Ok(5)],
                current: 0,
            };
            let mut dst = MockWrite {
                responses: vec![Err(Error::other("write error"))],
                current: 0,
                written_data: vec![],
            };

            let mut buffer = vec![0; 1024];
            let mut log_file: Option<MockWrite> = None;

            assert!(matches!(
                inner_fd_event_readable(&mut src, &mut dst, &mut buffer, &mut log_file),
                Err(ProcessEventsForFdError::Write(_))
            ));
        }

        #[test]
        fn error_on_log_write() {
            let mut src = MockRead {
                responses: vec![Ok(5)],
                current: 0,
            };
            let mut dst = MockWrite {
                responses: vec![Ok(())],
                current: 0,
                written_data: vec![],
            };

            let mut buffer = vec![0; 1024];
            let mut log_file = Some(MockWrite {
                responses: vec![Err(Error::other("log write error"))],
                current: 0,
                written_data: vec![],
            });

            assert!(matches!(
                inner_fd_event_readable(&mut src, &mut dst, &mut buffer, &mut log_file),
                Err(ProcessEventsForFdError::Log(_))
            ));
        }
    }
}
