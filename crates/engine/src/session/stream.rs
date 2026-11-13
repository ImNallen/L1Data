use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;

use crate::connection::{CONNECT_TIMEOUT, ConnectParams};
use crate::model::ConnectError;
use crate::tunnel::TunnelStream;

/// The channel is boxed to keep this as small as a socket: the drivers hold the stream inline in
/// every state of their connect futures, and debug builds copy those on the stack.
pub(crate) enum DbStream {
    Tcp(TcpStream),
    Ssh(Box<TunnelStream>),
}

impl ConnectParams {
    /// Through a tunnel, the SSH server resolves and dials `host`.
    pub(crate) async fn dial(&self) -> Result<DbStream, ConnectError> {
        if let Some(route) = &self.via {
            return route.open(&self.host, self.port).await.map(|s| DbStream::Ssh(Box::new(s)));
        }
        let network = |message: String| ConnectError::Network { message };
        let tcp = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((self.host.as_str(), self.port)))
            .await
            .map_err(|_| {
                network(format!("{}:{} did not answer within {} s", self.host, self.port, CONNECT_TIMEOUT.as_secs()))
            })?
            .map_err(|e| network(e.to_string()))?;
        tcp.set_nodelay(true).map_err(|e| network(e.to_string()))?;
        Ok(DbStream::Tcp(tcp))
    }
}

impl AsyncRead for DbStream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            DbStream::Tcp(s) => Pin::new(s).poll_read(cx, buf),
            DbStream::Ssh(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for DbStream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            DbStream::Tcp(s) => Pin::new(s).poll_write(cx, buf),
            DbStream::Ssh(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            DbStream::Tcp(s) => Pin::new(s).poll_flush(cx),
            DbStream::Ssh(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            DbStream::Tcp(s) => Pin::new(s).poll_shutdown(cx),
            DbStream::Ssh(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}
