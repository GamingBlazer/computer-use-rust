//! A mutex that ignores poisoning. A tool that panics is caught (see `tools::Registry::call`) and
//! the server keeps running; with the standard mutex every later lock of a state the panicking
//! thread held would panic too. The guarded data here is plain state that stays usable.

use std::convert::Infallible;
use std::sync::MutexGuard;

pub struct Mutex<T>(std::sync::Mutex<T>);

impl<T> Mutex<T> {
    pub const fn new(v: T) -> Mutex<T> {
        Mutex(std::sync::Mutex::new(v))
    }

    /// Always `Ok`; the `Result` keeps the usual `.lock().unwrap()` call sites unchanged.
    pub fn lock(&self) -> Result<MutexGuard<'_, T>, Infallible> {
        Ok(self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
}
