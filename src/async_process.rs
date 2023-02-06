//! Internal module providing an async child process abstraction for `async-std` or `tokio`.

use std::ffi::OsStr;
use std::pin::Pin;
pub use std::process::{ExitStatus, Stdio};
use std::task::{Context, Poll};

cfg_if::cfg_if! {
    if #[cfg(feature = "async-std-runtime")] {
        use ::async_std::process;
    } else if #[cfg(feature = "tokio-runtime")] {
        use ::tokio::process;
    }
}

#[derive(Debug)]
pub struct Command {
    inner: process::Command,
}

impl Command {
    pub fn new<S: AsRef<OsStr>>(program: S) -> Self {
        let inner = process::Command::new(program);
        Self { inner }
    }

    pub fn arg<S: AsRef<OsStr>>(&mut self, arg: S) -> &mut Self {
        self.inner.arg(arg);
        self
    }

    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.inner.args(args);
        self
    }

    pub fn envs<I, K, V>(&mut self, vars: I) -> &mut Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.inner.envs(vars);
        self
    }

    pub fn stderr<T: Into<Stdio>>(&mut self, cfg: T) -> &mut Self {
        self.inner.stderr(cfg);
        self
    }

    pub fn spawn(&mut self) -> std::io::Result<Child> {
        let inner = self.inner.spawn()?;
        Ok(Child::new(inner))
    }
}

#[derive(Debug)]
pub struct Child {
    pub stderr: Option<ChildStderr>,
    pub inner: process::Child,
}

/// Wrapper for an async child process.
///
/// The inner implementation depends on the selected async runtime (features `async-std-runtime`
/// or `tokio-runtime`).
impl Child {
    fn new(mut inner: process::Child) -> Self {
        let stderr = inner.stderr.take();
        Self {
            inner,
            stderr: stderr.map(|inner| ChildStderr { inner }),
        }
    }

    /// Kill the child process, asynchronously if possible (otherwise by blocking)
    ///
    /// - `async-std-runtime`: blocking call to `async_std::process::Child::kill`.
    /// - `tokio-runtime`: async call to `tokio::process::Child::kill`
    pub async fn kill(&mut self) -> std::io::Result<()> {
        cfg_if::cfg_if! {
            if #[cfg(feature = "async-std-runtime")] {
                self.inner.kill()
            } else if #[cfg(feature = "tokio-runtime")] {
                self.inner.kill().await
            }
        }
    }

    /// Kill the child process synchronously (blocking)
    ///
    /// - `async-std-runtime`: blocking call to `async_std::process::Child::kill`.
    /// - `tokio-runtime`: async call to `tokio::process::Child::kill`, resolved with `tokio::runtime::Handle::block_on`
    pub fn kill_sync(&mut self) -> std::io::Result<()> {
        cfg_if::cfg_if! {
            if #[cfg(feature = "async-std-runtime")] {
                self.inner.kill()
            } else if #[cfg(feature = "tokio-runtime")] {
                let fut = self.async_kill();
                let handle = tokio::runtime::Handle::current();
                handle.block_on(fut)
            }
        }
    }

    /// Asynchronously wait for the child process to exit (non-blocking)
    ///
    /// - `async-std-runtime`: async call to `async_std::process::Child::status`.
    /// - `tokio-runtime`: async call to `tokio::process::Child::wait`
    pub async fn wait(&mut self) -> std::io::Result<ExitStatus> {
        cfg_if::cfg_if! {
            if #[cfg(feature = "async-std-runtime")] {
                self.inner.status().await
            } else if #[cfg(feature = "tokio-runtime")] {
                self.inner.wait().await
            }
        }
    }

    /// Synchronously wait for the child process to exit (blocking)
    ///
    /// - `async-std-runtime`: async call to `async_std::process::Child::status`, resolved with `async_std::task::block_on`
    /// - `tokio-runtime`: async call to `tokio::process::Child::wait`, resolved with `tokio::runtime::Handle::block_on`
    pub fn wait_sync(&mut self) -> std::io::Result<ExitStatus> {
        cfg_if::cfg_if! {
            if #[cfg(feature = "async-std-runtime")] {
                let fut = self.wait();
                async_std::task::block_on(fut)
            } else if #[cfg(feature = "tokio-runtime")] {
                let fut = self.async_wait();
                let handle = tokio::runtime::Handle::current();
                handle.block_on(fut)
            }
        }
    }

    /// If the child process has exited, get its status (non-blocking)
    ///
    /// - `async-std-runtime`: call to `async_std::process::Child::try_status`
    /// - `tokio-runtime`: call to `tokio::process::Child::try_wait`
    pub fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        cfg_if::cfg_if! {
            if #[cfg(feature = "async-std-runtime")] {
                self.inner.try_status()
            } else if #[cfg(feature = "tokio-runtime")] {
                self.inner.try_wait()
            }
        }
    }

    /// Return a mutable reference to the inner process
    ///
    /// `stderr` may not be available.
    ///
    /// - `async-std-runtime`: return `&mut async_std::process::Child`
    /// - `tokio-runtime`: return `&mut tokio::process::Child`
    pub fn as_mut_inner(&mut self) -> &mut process::Child {
        &mut self.inner
    }

    /// Return the inner process
    ///
    /// - `async-std-runtime`: return `async_std::process::Child`
    /// - `tokio-runtime`: return `tokio::process::Child`
    pub fn into_inner(self) -> process::Child {
        let mut inner = self.inner;
        inner.stderr = self.stderr.map(ChildStderr::into_inner);
        inner
    }
}

#[derive(Debug)]
pub struct ChildStderr {
    pub inner: process::ChildStderr,
}

impl ChildStderr {
    pub fn into_inner(self) -> process::ChildStderr {
        self.inner
    }
}

impl futures::AsyncRead for ChildStderr {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        cfg_if::cfg_if! {
            if #[cfg(feature = "async-std-runtime")] {
                Pin::new(&mut self.inner).poll_read(cx, buf)
            } else if #[cfg(feature = "tokio-runtime")] {
                let mut buf = tokio::io::ReadBuf::new(buf);
                futures::ready!(tokio::io::AsyncRead::poll_read(
                    Pin::new(&mut self.inner),
                    cx,
                    &mut buf
                ))?;
                Poll::Ready(Ok(buf.filled().len()))
            }
        }
    }
}
