use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

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
    fn is_ready(&mut self, cx: &mut Context<'_>, now: Instant) -> bool {
        if !Future::poll(Pin::new(&mut self.delay), cx).is_pending() {
            self.delay.reset(self.interval);
            return true;
        }
        false
    }
}
