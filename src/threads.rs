//! Thread management utilities for self-reporting thread handles.
//!
//! This module provides functionality for spawning threads that can report their own handles back
//! to the parent thread, enabling better control and monitoring of thread lifecycle.

use anyhow::{Context, Result};
use std::sync::mpsc;
use std::thread::{self, ScopedJoinHandle};

/// Spawns a thread that sends its own handle back through a channel.
///
/// This function creates a scoped thread that executes the provided function and sends its own
/// handle back through a channel, allowing the parent thread to track and manage it. The thread is
/// created within the provided scope, ensuring it doesn't outlive its parent thread.
///
/// # Arguments
///
/// * `scope` - The thread scope in which the new thread will be created.
/// * `tx` - Channel sender for reporting the thread handle and name back to the parent.
/// * `thread_name` - Static string identifier for the thread.
/// * `func` - The function to be executed in the new thread.
///
/// # Returns
///
/// Returns `Ok(())` if the thread was successfully spawned, or an error if thread creation failed.
///
/// # Type Parameters
///
/// * `'scope` - Lifetime of the thread scope.
/// * `F` - Type of the function to be executed in the thread.
///
/// # Generic Constraints
///
/// * `F: FnOnce() -> Result<()>` - The function must take no arguments and return a Result.
/// * `F: Send` - The function must be safe to send between threads.
/// * `F: 'scope` - The function must live at least as long as the scope.
pub fn spawn_self_shipping_thread_in_scope<'scope, 'thread_name, F, R>(
    scope: &'scope thread::Scope<'scope, '_>,
    tx: mpsc::Sender<(&'thread_name str, ScopedJoinHandle<'scope, R>)>,
    thread_name: &'thread_name str,
    func: F,
) -> Result<()>
where
    F: FnOnce() -> R + Send + 'scope,
    R: Send + 'scope,
    'thread_name: 'scope,
{
    let (handle_tx, handle_rx) = mpsc::channel();

    let handle = std::thread::Builder::new()
        .name(thread_name.to_string())
        .spawn_scoped(scope, move || {
            // unwrap: Safe because `handle_tx` is guaranteed to have sent the handle.
            let handle = handle_rx.recv().unwrap();

            // This will send the thread handle to the caller of this function when this stackframe
            // is destroyed, even if that happens due to a panic.
            SendOnDrop {
                handle: Some(handle),
                // It is responsibility of the caller to make sure that the `rx` side of this
                // channel is alive until after this thread is finished.
                tx,
                thread_name,
            };

            func()
        })
        .context("Failed to create thread")?;

    // unwrap: Safe because `handle_rx` is guaranteed to be connected.
    handle_tx.send(handle).unwrap();

    Ok(())
}

// A struct with a `Drop` implementation to ensure the thread handle is sent to the caller of
// `spawn_self_shipping_thread_in_scope` even if the closure running in the thread panics.
struct SendOnDrop<'scope, 'thread_name, R> {
    handle: Option<ScopedJoinHandle<'scope, R>>,
    tx: mpsc::Sender<(&'thread_name str, ScopedJoinHandle<'scope, R>)>,
    thread_name: &'thread_name str,
}

impl<R> Drop for SendOnDrop<'_, '_, R> {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            // unwrap: Safe because the receiving side is guaranteed to still be connected.
            self.tx.send((self.thread_name, handle)).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod spawn_self_shipping_thread_in_scope {
        use super::*;
        use anyhow::Error;
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        #[test]
        fn basic() {
            let executed = Arc::new(AtomicBool::new(false));
            let executed_clone = executed.clone();

            thread::scope(|scope| {
                let (tx, rx) = mpsc::channel();

                spawn_self_shipping_thread_in_scope(scope, tx, "test_thread", move || {
                    executed_clone.store(true, Ordering::SeqCst);
                    Result::<(), Error>::Ok(())
                })
                .unwrap();

                let (thread_name, handle) = rx.recv().unwrap();
                assert_eq!(thread_name, "test_thread");
                handle.join().unwrap().unwrap();
                assert!(executed.load(Ordering::SeqCst));
            });
        }

        #[test]
        fn handles_panic() {
            thread::scope(|scope| {
                let (tx, rx) = mpsc::channel();

                spawn_self_shipping_thread_in_scope(scope, tx, "panicking_thread", || {
                    panic!("Thread is panicking on purpose for testing");
                })
                .unwrap();

                let (thread_name, handle) = rx.recv().unwrap();
                assert_eq!(thread_name, "panicking_thread");
                let join_result = handle.join();
                assert!(join_result.is_err());
            });
        }
    }
}
