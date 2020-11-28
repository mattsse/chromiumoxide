use std::borrow::Cow;
use std::collections::VecDeque;
use std::time::Instant;

use futures::channel::oneshot::Sender;
use futures::task::Poll;

use chromeoxid_types::Response;

use crate::cdp::browser_protocol::target::TargetId;
use crate::page::Page;
use std::iter::FromIterator;

pub enum PendingRequests {
    NewPage(NewPage),
    /// A request issued from a target, (e.g. as part of an initialize routine)
    InternalFromTarget(TargetId),
    External(Sender<Response>, Instant),
}

#[derive(Debug)]
pub struct NewPage {
    /// Time the creation started
    start: Instant,
    /// Sender who requested the page
    sender: Sender<Page>,
    /// State tracks the progress for a creation page
    state: NewPageState,
}

impl NewPage {}

#[derive(Debug)]
pub enum NewPageState {
    CreatingTarget,
    InitializingTarget,
    CreatingSession,
    Done,
}

#[derive(Debug)]
pub struct CommandChain {
    /// The commands to process: (method identifier, params)
    cmds: VecDeque<(Cow<'static, str>, serde_json::Value)>,
    /// The last issued command we currently waiting for its completion
    waiting: Option<Cow<'static, str>>,
}

impl CommandChain {
    /// Creates a new `CommandChain` from an `Iterator`.
    ///
    /// The order of the commands corresponds to the iterator
    pub fn new<I>(cmds: I) -> Self
    where
        I: IntoIterator<Item = (Cow<'static, str>, serde_json::Value)>,
    {
        Self {
            cmds: VecDeque::from_iter(cmds),
            waiting: None,
        }
    }

    /// queue in another request
    pub fn push_back(&mut self, method: Cow<'static, str>, params: serde_json::Value) {
        self.cmds.push_back((method, params))
    }

    /// Removes the waiting state if the identifier matches that of the last
    /// issued command
    pub fn received_response(&mut self, identifier: &str) -> bool {
        return if self.waiting.as_ref().map(|c| c.as_ref()) == Some(identifier) {
            self.waiting.take();
            true
        } else {
            false
        };
    }

    /// Return the next command to process or `None` if done
    pub fn poll(&mut self) -> Poll<Option<(Cow<'static, str>, serde_json::Value)>> {
        if self.waiting.is_some() {
            Poll::Pending
        } else {
            Poll::Ready(self.cmds.pop_front())
        }
    }
}
