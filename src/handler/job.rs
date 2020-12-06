use std::pin::Pin;
use std::task::Context;
use std::time::{Duration};

use crate::handler::REQUEST_TIMEOUT;
use futures::Future;
use futures_timer::Delay;

/// A background job run periodically.
#[derive(Debug)]
pub(crate) struct PeriodicJob {
    interval: Duration,
    delay: Delay,
}

impl PeriodicJob {
    /// Returns `true` if the job is currently not running but ready
    /// to be run, `false` otherwise.
    pub fn is_ready(&mut self, cx: &mut Context<'_>) -> bool {
        if !Future::poll(Pin::new(&mut self.delay), cx).is_pending() {
            self.delay.reset(self.interval);
            return true;
        }
        false
    }
}

impl Default for PeriodicJob {
    fn default() -> Self {
        let interval = Duration::from_millis(REQUEST_TIMEOUT);
        Self {
            delay: Delay::new(interval),
            interval,
        }
    }
}
