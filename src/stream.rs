/*! FCGI Server/Clients usually support TCP as well as Unixsockets
 * 
 * 
 */

use tokio::io::{AsyncRead, AsyncWrite, Error};
use tokio::net::{TcpStream,TcpListener};
#[cfg(unix)]
use tokio::net::{UnixStream,UnixListener};
use std::pin::Pin;
use std::task::{Context, Poll};

use std::net;
use std::fmt;
use std::str::FromStr;
use std::io;
use bytes::Buf;
#[cfg(unix)]
use std::path::{Path,PathBuf};
#[cfg(unix)]
use std::os::unix::net as unix;
#[cfg(unix)]
use std::os::unix::io::{RawFd,AsRawFd};

#[derive(Debug,Clone,PartialEq,Eq,Hash)]
pub enum FCGIAddr {
    Inet(net::SocketAddr),
    #[cfg(unix)]
    Unix(PathBuf)
}

impl From<net::SocketAddr> for FCGIAddr {
    fn from(s: net::SocketAddr) -> FCGIAddr {
        FCGIAddr::Inet(s)
    }
}

#[cfg(unix)]
impl From<&Path> for FCGIAddr {
    fn from(s: &Path) -> FCGIAddr {
        FCGIAddr::Unix(s.to_path_buf())
    }
}
#[cfg(unix)]
impl From<PathBuf> for FCGIAddr {
    fn from(s: PathBuf) -> FCGIAddr {
        FCGIAddr::Unix(s)
    }
}
#[cfg(unix)]
impl From<unix::SocketAddr> for FCGIAddr {
    fn from(s: unix::SocketAddr) -> FCGIAddr {
        FCGIAddr::Unix(match s.as_pathname() {
            None => Path::new("unnamed").to_path_buf(),
            Some(p) => p.to_path_buf()
        })
    }
}

impl fmt::Display for FCGIAddr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            FCGIAddr::Inet(n) => write!(f, "{}", n),
            #[cfg(unix)]
            FCGIAddr::Unix(n) => write!(f, "{}", n.to_string_lossy())
        }
    }
}

impl FromStr for FCGIAddr {
    type Err = net::AddrParseError;

    #[cfg(unix)]
    fn from_str(s: &str) -> Result<FCGIAddr, net::AddrParseError> {
        if s.starts_with("/") {
            Ok(FCGIAddr::Unix(Path::new(s).to_path_buf()))
        } else {
            s.parse().map(FCGIAddr::Inet)
        }
    }

    #[cfg(not(unix))]
    fn from_str(s: &str) -> Result<FCGIAddr, net::AddrParseError> {
        s.parse().map(FCGIAddr::Inet)
    }
}
#[derive(Debug)]
pub enum Stream {
    Inet(TcpStream),
    #[cfg(unix)]
    Unix(UnixStream)
}

impl From<TcpStream> for Stream {
    fn from(s: TcpStream) -> Stream {
        Stream::Inet(s)
    }
}

#[cfg(unix)]
impl From<UnixStream> for Stream {
    fn from(s: UnixStream) -> Stream {
        Stream::Unix(s)
    }
}

impl Stream {
    pub async fn connect(s: &FCGIAddr) -> io::Result<Stream> {
        match s {
            FCGIAddr::Inet(s) => TcpStream::connect(s).await.map(Stream::Inet),
            #[cfg(unix)]
            FCGIAddr::Unix(s) => UnixStream::connect(s).await.map(Stream::Unix)
        }
    }

    pub fn local_addr(&self) -> io::Result<FCGIAddr> {
        match self {
            Stream::Inet(s) => s.local_addr().map(FCGIAddr::Inet),
            #[cfg(unix)]
            Stream::Unix(s) => s.local_addr().map(|e| e.into())
        }
    }

    pub fn peer_addr(&self) -> io::Result<FCGIAddr> {
        match self {
            Stream::Inet(s) => s.peer_addr().map(FCGIAddr::Inet),
            #[cfg(unix)]
            Stream::Unix(s) => s.peer_addr().map(|e| e.into())
        }
    }

}
impl AsyncRead for Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &mut [u8]
    ) -> Poll<Result<usize, Error>> {
        match &mut *self {
            Stream::Inet(s) => Pin::new(s).as_mut().poll_read(cx, buf),
            #[cfg(unix)]
            Stream::Unix(s) => Pin::new(s).as_mut().poll_read(cx, buf)
        }
    }

}
impl AsyncWrite for Stream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &[u8]
    ) -> Poll<Result<usize, Error>> {
        match &mut *self {
            Stream::Inet(s) => Pin::new(s).as_mut().poll_write(cx, buf),
            #[cfg(unix)]
            Stream::Unix(s) => Pin::new(s).as_mut().poll_write(cx, buf)
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Error>> {
        match &mut *self {
            Stream::Inet(s) => Pin::new(s).as_mut().poll_flush(cx),
            #[cfg(unix)]
            Stream::Unix(s) => Pin::new(s).as_mut().poll_flush(cx)
        }
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context
    ) -> Poll<Result<(), Error>> {
        match &mut *self {
            Stream::Inet(s) => Pin::new(s).as_mut().poll_shutdown(cx),
            #[cfg(unix)]
            Stream::Unix(s) => Pin::new(s).as_mut().poll_shutdown(cx)
        }
    }
    fn poll_write_buf<B: Buf>(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut B,
    ) -> Poll<io::Result<usize>> {
        match &mut *self {
            Stream::Inet(s) => Pin::new(s).as_mut().poll_write_buf(cx, buf),
            #[cfg(unix)]
            Stream::Unix(s) => Pin::new(s).as_mut().poll_write_buf(cx, buf)
        }
    }

}
pub enum Listener {
    Inet(TcpListener),
    #[cfg(unix)]
    Unix(UnixListener)
}
impl Listener {
    pub async fn bind(s: &FCGIAddr) -> io::Result<Listener> {
        match s {
            FCGIAddr::Inet(s) => TcpListener::bind(s).await.map(Listener::Inet),
            #[cfg(unix)]
            FCGIAddr::Unix(s) => UnixListener::bind(s).map(Listener::Unix)
        }
    }
    pub async fn accept(&mut self) -> io::Result<(Stream, FCGIAddr)> {
        match &mut *self {
            Listener::Inet(s) => s.accept().await.map(|(s,a)|(Stream::Inet(s),FCGIAddr::Inet(a))),
            #[cfg(unix)]
            Listener::Unix(s) => s.accept().await.map(|(s,a)|(Stream::Unix(s),FCGIAddr::from(a)))
        }
    }
}
#[cfg(unix)]
impl AsRawFd for Listener {
    fn as_raw_fd(&self) -> RawFd {
        match self {
            Listener::Inet(s) => s.as_raw_fd(),
            #[cfg(unix)]
            Listener::Unix(s) => s.as_raw_fd()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::runtime::Runtime;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use std::net::SocketAddr;
    use bytes::{BytesMut, Bytes};
    use tokio::net::TcpListener;
    #[cfg(unix)]
    use tokio::net::UnixListener;

    #[test]
    fn tcp_connect() {
        let mut rt = Runtime::new().unwrap();
        async fn mock_app(mut app_listener: TcpListener) {
            let (mut app_socket, _) = app_listener.accept().await.unwrap();
            let mut buf = BytesMut::with_capacity(4096);
            app_socket.read_buf(&mut buf).await.unwrap();
            app_socket.write_buf(&mut buf.freeze()).await.unwrap();
        }

        async fn con() {
            let a: SocketAddr = "127.0.0.1:59003".parse().unwrap();
            let app_listener = TcpListener::bind(a).await.unwrap();
            tokio::spawn(mock_app(app_listener));

            let a: FCGIAddr = "127.0.0.1:59003".parse().expect("tcp parse failed");
            let mut s = Stream::connect(&a).await.expect("tcp connect failed");

            let data = b"1234";
            s.write_buf(&mut Bytes::from(&data[..])).await.expect("tcp write failed");

            let mut buf = BytesMut::with_capacity(4096);
            s.read_buf(&mut buf).await.expect("tcp read failed");
            assert_eq!(buf.to_bytes(), &data[..]);
        }
        rt.block_on(con());
    }
    #[cfg(unix)]
    #[test]
    fn unix_connect() {
        let mut rt = Runtime::new().unwrap();
        async fn mock_app(mut app_listener: UnixListener) {
            let (mut app_socket, _) = app_listener.accept().await.unwrap();
            let mut buf = BytesMut::with_capacity(4096);
            app_socket.read_buf(&mut buf).await.unwrap();
            app_socket.write_buf(&mut buf.freeze()).await.unwrap();
        }

        async fn con() {
            let a: &Path = Path::new("/tmp/afcgi.sock");
            let app_listener = UnixListener::bind(a).unwrap();
            tokio::spawn(mock_app(app_listener));

            let a: FCGIAddr = "/tmp/afcgi.sock".parse().expect("unix parse failed");
            println!("unix: {}", &a);
            let mut s = Stream::connect(&a).await.expect("unix connect failed");

            let data = b"1234";
            s.write_buf(&mut Bytes::from(&data[..])).await.expect("unix write failed");

            let mut buf = BytesMut::with_capacity(4096);
            s.read_buf(&mut buf).await.expect("unix read failed");
            assert_eq!(buf.to_bytes(), &data[..]);
        }
        rt.block_on(con());
    }
}