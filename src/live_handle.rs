use tokio::task::{AbortHandle, JoinHandle};

/// A handle to a tokio task that aborts the task when dropped.
pub struct LiveHandle(AbortHandle);

impl LiveHandle {
    pub fn abort(self) {
        self.0.abort();
    }
}

impl Drop for LiveHandle {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl<T> From<JoinHandle<T>> for LiveHandle {
    fn from(value: JoinHandle<T>) -> Self {
        Self(value.abort_handle())
    }
}
