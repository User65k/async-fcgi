/*! FCGI Application serving for [Hyper 0.13](https://github.com/hyperium/hyper).



  This Module consists of the following Objects:

 * [`ConPool`]: supports FCGI_MAX_CONNS Connections
 * [`Connection`]: handles up to FCGI_MAX_REQS concurrent Requests

[`ConPool`]: ./struct.ConPool.html
[`Connection`]: ../connection/index.html
*/

use crate::client::connection::{Connection, MultiHeaderStrategy, HeaderMultilineStrategy, PreparedConnection};
use crate::codec::FCGIWriter;
use crate::fastcgi::{Body, Record, MAX_CONNS, MAX_REQS, MPXS_CONNS, RecordType};
use async_stream_connection::{Addr, Stream};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use http::{Request, Response};
use http_body::Body as HttpBody;
use log::{info, trace};
use std::fmt::{self, Display};
use std::io::Error as IoError;
use std::iter::IntoIterator;
use tokio::io::AsyncReadExt;
use std::future::Future;
use std::pin::{Pin, pin};
use std::task::{Context, Poll};
use tokio::sync::RwLock;

#[cfg(all(unix, feature = "app_start"))]
use async_stream_connection::Listener;
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
    sock_addr: Addr,
    header_mul: MultiHeaderStrategy,
    header_nl: HeaderMultilineStrategy,
    max_cons: u8,
    /// The maximum number of concurrent transport connections this application will accept
    max_req_per_con: u16,
    /// The maximum number of concurrent requests this application will accept
    con_pool: RwLock<Vec<Connection>>,
}
impl ConPool {
    /// Connect to a FCGI server / application with [`MultiHeaderStrategy::OnlyFirst`] & [`HeaderMultilineStrategy::Ignore`].
    /// See [`ConPool::new_with_strategy`]
    #[inline]
    pub async fn new(sock_addr: &Addr) -> Result<ConPool, IoError> {
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
        sock_addr: &Addr,
        header_mul: MultiHeaderStrategy,
        header_nl: HeaderMultilineStrategy,
    ) -> Result<ConPool, IoError> {
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
        let mut c = ConPool {
            sock_addr: sock_addr.clone(),
            header_mul,
            header_nl,
            max_cons,
            max_req_per_con,
            con_pool: RwLock::new(Vec::with_capacity(max_cons as usize)),
        };
        /*let con = c.new_con().await?;
        c.con_pool.write().await.push(con);*/
        Ok(c)
    }
    /// Create a new connection to the App via [`Connection::connect_with_strategy`]
    async fn new_con(&self) -> Result<Connection, IoError> {
        Connection::connect_with_strategy(
            &self.sock_addr,
            self.max_req_per_con,
            self.header_mul,
            self.header_nl
        ).await
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
        B::Error: Display,
        I: IntoIterator<Item = (P1, P2)>,
        P1: Buf,
        P2: Buf,
    {
        //race self.con_pool.prep_connection().await and Connection::connect_with_strategy

        let rc = {
            let max_cons = self.max_cons as usize;
            let con_pool = self.con_pool.read().await;
            let nu_con = if max_cons > con_pool.len() {
                Some(Box::pin(self.new_con()))
            }else{
                None
            };
            let waiting = con_pool.iter().map(|c|Box::pin(c.prep_connection())).collect();
            RaceConnections {
                nu_con,
                waiting,
            }.await
        };
        let con_pool = self.con_pool.read().await;
        let (con, slot) = match rc {
            Ok(Raced::Prep((i, slot))) => {
                (con_pool.get(i).unwrap(), slot)
            },
            Ok(Raced::New(con)) => {
                self.con_pool.write().await.push(con);
                let con = con_pool.last().unwrap();
                let slot = con.prep_connection().await?;
                (con, slot)
            }
            Err(e) => {
                return Err(e);
            }
        };
        con.send_request(req, dyn_headers, slot).await
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
enum Raced {
    Prep((usize, PreparedConnection)),
    New(Connection)
}
struct RaceConnections<FutNew, FutWait>
where 
    FutNew: Future<Output=Result<Connection, IoError>>,
    FutWait: Future<Output=Result<PreparedConnection, IoError>>
{
    nu_con: Option<Pin<Box<FutNew>>>,
    waiting: Vec<Pin<Box<FutWait>>>
}
impl<FutNew, FutWait> Future for RaceConnections<FutNew, FutWait>
where 
    FutNew: Future<Output=Result<Connection, IoError>>,
    FutWait: Future<Output=Result<PreparedConnection, IoError>>
{
    type Output = Result<Raced, IoError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut last_err = None;
        if let Some(fut) = self.as_mut().nu_con.as_mut() {
            let fut = pin!(fut);
            match fut.poll(cx) {
                Poll::Ready(Err(e)) => {
                    log::error!("Error when connecting: {}", &e);
                    last_err = Some(e);
                    self.as_mut().nu_con = None;
                    cx.waker().wake_by_ref();
                },
                Poll::Ready(Ok(con)) => {
                    return Poll::Ready(Ok(Raced::New(con)));
                },
                Poll::Pending => {},
            }
        }
        for (x, pc) in self.as_mut().waiting.iter_mut().enumerate() {
            let pc = pin!(pc);
            match pc.poll(cx) {
                Poll::Ready(Err(e)) => {
                    log::error!("Error when preping: {}", &e);
                    last_err = Some(e);
                    cx.waker().wake_by_ref();
                },
                Poll::Ready(Ok(con)) => {
                    return Poll::Ready(Ok(Raced::Prep((x,con))));
                },
                Poll::Pending => {},
            }
        }
        if let Some(e) = last_err.take() {
            Poll::Ready(Err(e))
        }else{
            Poll::Pending
        }
    }    
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
#[cfg_attr(docsrs, doc(cfg(feature = "app_start")))]
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
    pub async fn prep_server<S>(program: S, sock_addr: &Addr) -> Result<Command, IoError>
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
    use crate::client::tests::local_socket_pair;
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
            let a: Addr = "/tmp/jo".parse().unwrap();
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
