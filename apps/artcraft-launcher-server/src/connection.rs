use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::extract::connect_info::Connected;
use axum::serve::{IncomingStream, Listener};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{Sleep, sleep};

const WRITE_IDLE: Duration = Duration::from_secs(30);

pub(crate) struct DeadlineListener(pub(crate) TcpListener);

#[derive(Clone, Copy)]
pub(crate) struct PeerAddr(pub(crate) SocketAddr);

impl Connected<IncomingStream<'_, DeadlineListener>> for PeerAddr {
    fn connect_info(stream: IncomingStream<'_, DeadlineListener>) -> Self {
        Self(*stream.remote_addr())
    }
}

impl Listener for DeadlineListener {
    type Io = WriteDeadline<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        let (io, address) = Listener::accept(&mut self.0).await;
        (WriteDeadline { io, idle: WRITE_IDLE, stalled: None }, address)
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.0.local_addr()
    }
}

pub(crate) struct WriteDeadline<T> {
    io: T,
    idle: Duration,
    stalled: Option<Pin<Box<Sleep>>>,
}

impl<T> WriteDeadline<T> {
    fn enforce<U>(&mut self, cx: &mut Context<'_>, operation: Poll<io::Result<U>>) -> Poll<io::Result<U>> {
        if operation.is_ready() {
            self.stalled = None;
            return operation;
        }
        let timer = self.stalled.get_or_insert_with(|| Box::pin(sleep(self.idle)));
        if timer.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(io::Error::new(io::ErrorKind::TimedOut, "HTTP response write stalled")));
        }
        Poll::Pending
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for WriteDeadline<T> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for WriteDeadline<T> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, data: &[u8]) -> Poll<io::Result<usize>> {
        let operation = Pin::new(&mut self.io).poll_write(cx, data);
        self.enforce(cx, operation)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let operation = Pin::new(&mut self.io).poll_flush(cx);
        self.enforce(cx, operation)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let operation = Pin::new(&mut self.io).poll_shutdown(cx);
        self.enforce(cx, operation)
    }
}

#[cfg(test)]
mod tests {
    use super::{Duration, WriteDeadline, io};
    use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};

    #[tokio::test]
    async fn writing_to_a_full_transport_times_out() -> io::Result<()> {
        let (io, mut peer) = duplex(1);
        let mut stream = WriteDeadline { io, idle: Duration::from_millis(5), stalled: None };
        stream.write_all(b"a").await?;
        assert_eq!(peer.read_u8().await?, b'a');
        let error = stream.write_all(b"bc").await.err().ok_or_else(|| io::Error::other("expected stalled write"))?;
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        Ok(())
    }
}
