//! A `Notify` type that acts like tokio's `Notify`, which is a simple semaphore for
//! notifying a single task of an event.

use smol::lock::{Semaphore, SemaphoreGuard};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct Notify {
    inner: Arc<Semaphore>,
}

impl Notify {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Semaphore::new(0)),
        }
    }

    /// Notify a single task of an event.
    ///
    /// This method always sets the amount of permits in the semaphore to 1.
    pub fn notify(&self) {
        let None = self.inner.try_acquire() else {
            // Managed to acquire, so we need to add that permit back.
            self.inner.add_permits(1);
            return;
        };

        self.inner.add_permits(1);
    }

    /// Wait for a notification. This method returns a future.
    pub async fn acquire(&self) {
        let semaphore_guard = self.inner.acquire_arc().await;
        semaphore_guard.forget(); // Prevents the permit from returning
    }

    pub fn acquire_blocking(&self) -> SemaphoreGuard<'_> {
        self.inner.acquire_blocking()
    }
}
