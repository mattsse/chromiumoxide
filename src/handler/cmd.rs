use std::borrow::Cow;
use std::collections::VecDeque;
use std::iter::FromIterator;
use std::time::{Duration, Instant};

use futures::task::Poll;

use chromiumoxid_types::Response;

use crate::cdp::browser_protocol::target::TargetId;
use crate::error::DeadlineExceeded;
use crate::handler::REQUEST_TIMEOUT;

#[derive(Debug)]
pub struct CommandChain {
    /// The commands to process: (method identifier, params)
    cmds: VecDeque<(Cow<'static, str>, serde_json::Value)>,
    /// The last issued command we currently waiting for its completion
    waiting: Option<(Cow<'static, str>, Instant)>,
    /// The window a response after issuing a request must arrive
    timeout: Duration,
}

impl CommandChain {
    /// Creates a new `CommandChain` from an `Iterator`.
    ///
    /// The order of the commands corresponds to the iterator's
    pub fn new<I>(cmds: I) -> Self
    where
        I: IntoIterator<Item = (Cow<'static, str>, serde_json::Value)>,
    {
        Self {
            cmds: VecDeque::from_iter(cmds),
            waiting: None,
            timeout: Duration::from_millis(REQUEST_TIMEOUT),
        }
    }

    /// queue in another request
    pub fn push_back(&mut self, method: Cow<'static, str>, params: serde_json::Value) {
        self.cmds.push_back((method, params))
    }

    /// Removes the waiting state if the identifier matches that of the last
    /// issued command
    pub fn received_response(&mut self, identifier: &str) -> bool {
        return if self.waiting.as_ref().map(|(c, _)| c.as_ref()) == Some(identifier) {
            self.waiting.take();
            true
        } else {
            false
        };
    }

    /// Return the next command to process or `None` if done.
    /// If the response timeout an error is returned instead
    pub fn poll(
        &mut self,
        now: Instant,
    ) -> Poll<Option<Result<(Cow<'static, str>, serde_json::Value), DeadlineExceeded>>> {
        if let Some((_, deadline)) = self.waiting.as_ref() {
            if now > *deadline {
                Poll::Ready(Some(Err(DeadlineExceeded::new(now, *deadline))))
            } else {
                Poll::Pending
            }
        } else {
            if let Some((method, val)) = self.cmds.pop_front() {
                self.waiting = Some((method.clone(), now + self.timeout));
                Poll::Ready(Some(Ok((method, val))))
            } else {
                Poll::Ready(None)
            }
        }
    }
}

impl Default for CommandChain {
    fn default() -> Self {
        Self {
            cmds: Default::default(),
            waiting: None,
            timeout: Duration::from_millis(REQUEST_TIMEOUT),
        }
    }
}
