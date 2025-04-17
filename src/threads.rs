use anyhow::{Context, Result};
use std::sync::mpsc;
use std::thread::{self, ScopedJoinHandle};

pub fn spawn_self_shipping_thread_in_scope<'scope, F>(
    scope: &'scope thread::Scope<'scope, '_>,
    tx: mpsc::Sender<(&'static str, ScopedJoinHandle<'scope, Result<()>>)>,
    thread_name: &'static str,
    func: F,
) -> Result<()>
where
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
        .context("Failed to create thread")?;

    // unwrap: Safe because `handle_rx` is guaranteed to be connected.
    handle_tx.send(handle).unwrap();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    mod spawn_self_shipping_thread_in_scope {
        use super::*;
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
                    Ok(())
                })
                .unwrap();

                let (thread_name, handle) = rx.recv().unwrap();
                assert_eq!(thread_name, "test_thread");
                handle.join().unwrap().unwrap();
                assert!(executed.load(Ordering::SeqCst));
            });
        }
    }
}
