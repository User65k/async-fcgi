/*! FCGI Application serving for [Hyper 0.13](https://github.com/hyperium/hyper).



  This Module consists of the following Objects:

 * [`ConPool`]: supports FCGI_MAX_CONNS Connections
 * [`Connection`]: handles up to FCGI_MAX_REQS concurrent Requests

[`ConPool`]: ./struct.ConPool.html
[`Connection`]: ../connection/index.html
*/

use crate::client::connection::{Connection, MultiHeaderStrategy, HeaderMultilineStrategy};
use crate::codec::FCGIWriter;
use crate::fastcgi::{Body, Record, MAX_CONNS, MAX_REQS, MPXS_CONNS, RecordType};
use crate::stream::{FCGIAddr, Stream};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use http::{Request, Response};
use http_body::Body as HttpBody;
use log::{info, trace};
use std::error::Error;
use std::fmt;
use std::io::Error as IoError;
use std::iter::IntoIterator;
use tokio::io::AsyncReadExt;

#[cfg(all(unix, feature = "app_start"))]
use crate::stream::Listener;
#[cfg(feature = "app_start")]
use std::ffi::OsStr;
#[cfg(all(unix, feature = "app_start"))]
use std::os::unix::io::{AsRawFd, FromRawFd};
#[cfg(feature = "app_start")]
use std::process::Stdio;
#[cfg(feature = "app_start")]
use tokio::process::Command;

/// manage a pool of [`Connection`]s to an Server.
pub struct ConPool {
    /*
    sock_addr: String,*/
    max_cons: u8,
    /// The maximum number of concurrent transport connections this application will accept
    max_req_per_con: u16,
    /// The maximum number of concurrent requests this application will accept
    con_pool: Connection,
}
impl ConPool {
    /// Connect to a FCGI server / application with [`MultiHeaderStrategy::OnlyFirst`] & [`HeaderMultilineStrategy::Ignore`].
    /// See [`ConPool::new_with_strategy`]
    #[inline]
    pub async fn new(sock_addr: &FCGIAddr) -> Result<ConPool, Box<dyn Error>> {
        Self::new_with_strategy(
            sock_addr,
            MultiHeaderStrategy::OnlyFirst,
            HeaderMultilineStrategy::Ignore,
        )
        .await
    }

    /// Connect to a FCGI server / application.
    /// Queries [`MAX_CONNS`],
    /// [`MAX_REQS`]
    /// and [`MPXS_CONNS`] from the server
    /// and uses the values to create a [`Connection`].
    pub async fn new_with_strategy(
        sock_addr: &FCGIAddr,
        header_mul: MultiHeaderStrategy,
        header_nl: HeaderMultilineStrategy,
    ) -> Result<ConPool, Box<dyn Error>> {
        // query VALUES from connection
        let stream = Stream::connect(sock_addr).await?;
        let mut stream = FCGIWriter::new(stream);
        let mut kvw = stream.kv_stream(Record::MGMT_REQUEST_ID, RecordType::GetValues);
        kvw.add_kv(MAX_CONNS, Bytes::new()).await?;
        kvw.add_kv(MAX_REQS, Bytes::new()).await?;
        kvw.add_kv(MPXS_CONNS, Bytes::new()).await?;
        kvw.flush().await?;
        let mut max_cons = 1;
        let mut max_req_per_con = 1;
        for rec in send_and_receive(&mut stream).await? {
            if let Body::GetValuesResult(kvs) = rec.body {
                for kv in kvs.drain() {
                    match kv.name_data.chunk() {
                        MAX_CONNS => {
                            if let Some(v) = parse_int::<u8>(kv.value_data) {
                                max_cons = v;
                            }
                        }
                        MAX_REQS => {
                            if let Some(v) = parse_int::<u16>(kv.value_data) {
                                max_req_per_con = v;
                            }
                        }
                        MPXS_CONNS => {
                            if kv.value_data == "0" {
                                max_req_per_con = 1;
                                break;
                            }
                        }
                        _ => {}
                    };
                }
            }
        }
        info!(
            "App supports {} connections with {} requests",
            max_cons, max_req_per_con
        );
        let c = Connection::connect_with_strategy(
            &sock_addr,
            max_req_per_con,
            header_mul,
            header_nl
        ).await?;

        Ok(ConPool {
            /*
            sock_addr,*/
            max_cons,
            max_req_per_con,
            con_pool: c,
        })
    }
    /// Forwards an HTTP request to a FGCI Application.
    /// Calls [`Connection::forward`] on an available connection.
    pub async fn forward<B, I, P1, P2>(
        &self,
        req: Request<B>,
        dyn_headers: I,
    ) -> Result<Response<impl HttpBody<Data = Bytes, Error = IoError>>, IoError>
    where
        B: HttpBody + Unpin,
        I: IntoIterator<Item = (P1, P2)>,
        P1: Buf,
        P2: Buf,
    {
        self.con_pool.forward(req, dyn_headers).await
    }
}
impl fmt::Debug for ConPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConPool")
            .field("max_cons", &self.max_cons)
            .field("max_req_per_con", &self.max_req_per_con)
            .finish()
    }
}

fn parse_int<I: std::str::FromStr>(bytes: Bytes) -> Option<I> {
    if let Ok(s) = std::str::from_utf8(bytes.chunk()) {
        if let Ok(i) = s.parse() {
            return Some(i);
        }
    }
    return None;
}

/// Note: only use this if there are no requests pending
async fn send_and_receive(stream: &mut FCGIWriter<Stream>) -> Result<Vec<Record>, IoError> {
    let mut recs = Vec::new();

    trace!("prep 4 read");
    let mut rbuf = BytesMut::with_capacity(4096);
    loop {
        stream.read_buf(&mut rbuf).await?;
        trace!("got {:?}", rbuf);
        let mut pbuf = rbuf.freeze();
        while let Some(r) = Record::read(&mut pbuf) {
            recs.push(r);
        }
        if !pbuf.has_remaining() {
            break;
        }
        rbuf = BytesMut::with_capacity(pbuf.len() + 4096);
        rbuf.put(pbuf);
    }

    Ok(recs)
}

#[cfg(feature = "app_start")]
impl ConPool {
    /// Setup a [`Command`] to spin up a FCGI server / application
    /// and make it listen on `sock_addr`.
    /// ```no_run
    /// # use async_fcgi::{client::con_pool::ConPool,FCGIAddr};
    /// # use std::collections::HashMap;
    /// # use std::error::Error;
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> Result<(),Box<dyn Error>> {
    /// let mut env = HashMap::new();
    /// env.insert("PHP_FCGI_CHILDREN", "16");
    /// env.insert("PHP_FCGI_MAX_REQUESTS", "10000");
    /// let addr: FCGIAddr = "127.0.0.1:1236".parse()?;
    /// let php = ConPool::prep_server("/usr/bin/php-cgi7.4", &addr)
    ///             .await?
    ///             .env_clear().envs(env)
    ///             .spawn()?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn prep_server<S>(program: S, sock_addr: &FCGIAddr) -> Result<Command, IoError>
    where
        S: AsRef<OsStr>,
    {
        // The Web server leaves a single file descriptor, FCGI_LISTENSOCK_FILENO, open when the application begins execution.
        // This descriptor refers to a listening socket created by the Web server.
        #[cfg(not(unix))]
        let stdin = Stdio::null();
        #[cfg(unix)]
        let stdin = {
            let l = Listener::bind(sock_addr).await?;
            let fd = unsafe { Stdio::from_raw_fd(l.as_raw_fd()) };
            std::mem::forget(l); // FCGI App closes this - at least php-cgi7.4 does it
            fd
        };

        let mut command = Command::new(program);
        command
                .stdin(stdin) // FCGI_LISTENSOCK_FILENO equals STDIN_FILENO.
                //.stdout(Stdio::null()).stderr(Stdio::null()) // The standard descriptors STDOUT_FILENO and STDERR_FILENO are closed when the application begins execution.
                ;
        Ok(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::tests::local_socket_pair;
    use std::collections::HashMap;
    use std::iter::FromIterator;
    use std::process::ExitStatus;
    use tokio::io::AsyncWriteExt;
    use tokio::runtime::Builder;

    #[cfg(feature = "app_start")]
    #[test]
    fn start_app() {
        let rt = Builder::new_current_thread().enable_all().build().unwrap();
        async fn spawn() {
            let mut env = HashMap::new();
            env.insert("PATH", "/usr/bin");
            let a: FCGIAddr = "/tmp/jo".parse().unwrap();
            let s: ExitStatus = ConPool::prep_server("ls", &a)
                .await
                .expect("prep_server error")
                .args(&["-l", "-a"])
                .env_clear()
                .envs(env)
                .status()
                .await
                .expect("ls failed");
            assert!(s.success())
        }
        rt.block_on(spawn());
        std::fs::remove_file("/tmp/jo").unwrap();
    }
    #[test]
    fn no_vals() {
        //extern crate pretty_env_logger;
        //pretty_env_logger::init();
        use tokio::net::TcpListener;
        // Create the runtime
        let rt = Builder::new_current_thread().enable_all().build().unwrap();
        async fn mock_app(app_listener: TcpListener) {
            let (mut app_socket, _) = app_listener.accept().await.unwrap();
            let mut buf = BytesMut::with_capacity(4096);
            info!("accepted");

            //app_socket.read_buf(&mut buf).await.unwrap();
            if let Err(e) = app_socket.read_buf(&mut buf).await {
                info!("{}", e);
                panic!("could not read");
            }

            let mut buf = buf.freeze();
            trace!("app read {:?}", buf);
            let rec = Record::read(&mut buf).unwrap(); //val stream
            assert_eq!(rec.get_request_id(), 0);
            let v = match rec.body {
                Body::GetValues(v) => v,
                _ => panic!("wrong body"),
            };
            let names = Vec::from_iter(v.drain());
            assert_eq!(names.len(), 3);

            let _ = Record::read(&mut buf).unwrap(); //val stream end

            assert!(!buf.has_remaining());

            trace!("app answers on get");
            let from_php =
                b"\x01\x0a\0\0\0!\x07\0\n\0MPXS_CONNS\x08\0MAX_REQS\t\0MAX_CONNS\0\0\0\0\0\0\0";
            app_socket
                .write_buf(&mut Bytes::from(&from_php[..]))
                .await
                .unwrap();

            let _ = app_listener.accept().await.unwrap();
            info!("accepted2");
        }

        async fn con() {
            let (app_listener, a) = local_socket_pair().await.unwrap();
            info!("bound");
            let m = tokio::spawn(async move {
                let a = a.into();
                let cp = ConPool::new(&a).await.unwrap();
                assert_eq!(cp.max_cons, 1);
                assert_eq!(cp.max_req_per_con, 1);
            });
            mock_app(app_listener).await;
            m.await.unwrap();
        }
        rt.block_on(con());
    }
}
