/*! Fast CGI client/webserver side
 *
*/

#[cfg(feature = "con_pool")]
pub mod con_pool;
#[cfg(feature = "web_server")]
pub mod connection;

#[cfg(test)]
pub(crate) mod tests {
    use async_stream_connection::Addr;
    use std::net::SocketAddr;
    use tokio::net::TcpListener;

    pub(crate) async fn local_socket_pair() -> Result<(TcpListener, Addr), std::io::Error> {
        let a: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let app_listener = TcpListener::bind(a).await?;
        let a: Addr = app_listener.local_addr()?.into();
        Ok((app_listener, a))
    }
    use http_body::{Frame, SizeHint};
    use std::collections::VecDeque;
    use std::task::{Poll, Context};
    use std::pin::Pin;
    use bytes::{Buf, Bytes};
    
    use http_body::Body;

    pub struct TestBod {
        pub l: VecDeque<Bytes>,
    }
    impl Body for TestBod {
        type Data = Bytes;
        type Error = std::io::Error;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            let Self { ref mut l } = *self;
            match l.pop_front() {
                None => Poll::Ready(None),
                Some(i) => Poll::Ready(Some(Ok(Frame::data(i)))),
            }
        }
        fn size_hint(&self) -> SizeHint {
            let mut sh = SizeHint::default();
            let s: usize = self.l.iter().map(|b| b.remaining()).sum();
            sh.set_exact(s as u64);
            sh
        }
    }
    pub fn init_log() {
        let mut builder = pretty_env_logger::formatted_timed_builder();
        builder.is_test(true);
        if let Ok(s) = ::std::env::var("RUST_LOG") {
            builder.parse_filters(&s);
        }
        let _ = builder.try_init();
    }
}
