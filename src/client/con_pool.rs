/*! FCGI Application serving for [Hyper 0.13](https://github.com/hyperium/hyper).



  This Module consists of the following Objects:

 * [`ConPool`]: supports FCGI_MAX_CONNS Connections
 * [`Connection`]: handles up to FCGI_MAX_REQS concurrent Requests

[`ConPool`]: ./struct.ConPool.html
[`Connection`]: ../connection/index.html
*/
use crate::{
    client::connection::{
        Connection, HeaderMultilineStrategy, MultiHeaderStrategy, PreparedConnection,
    },
    codec::FCGIWriter,
    fastcgi::{Body, Record, RecordType, MAX_CONNS, MAX_REQS, MPXS_CONNS},
};
use async_stream_connection::{Addr, Stream};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use http::{Request, Response};
use http_body::Body as HttpBody;
use log::{info, trace};
use std::{
    fmt::{self, Display}, future::Future, io::Error as IoError, iter::IntoIterator, pin::{Pin, pin}, sync::{Arc, atomic::{AtomicUsize, Ordering}}, task::{Context, Poll}
};
use tokio::{io::AsyncReadExt, sync::{Notify, RwLock}};

#[cfg(feature = "app_start")]
mod app_start;
#[cfg(test)]
mod tests;

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
    /// no of new connections pending
    connecting: AtomicUsize,
    pending: Arc<Notify>
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
        let c = ConPool {
            sock_addr: sock_addr.clone(),
            header_mul,
            header_nl,
            max_cons,
            max_req_per_con,
            con_pool: RwLock::new(Vec::with_capacity(max_cons as usize)),
            connecting: AtomicUsize::new(0),
            pending: Arc::new(Notify::new())
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
            self.header_nl,
        )
        .await
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
            let con_pool_len = con_pool.len();
            let nu_con = if let Ok(_) = self.connecting.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |x| {
                if max_cons > con_pool_len + x {
                    Some(x + 1)
                }else{
                    None
                }
            }) {
                trace!(
                    "opening additional connection #{}/{} {:?}, {}",
                    con_pool_len + 1,
                    max_cons,
                    req.uri().query(),
                    self.connecting.load(Ordering::Relaxed)
                );
                Some(Box::pin(self.new_con()))
            } else {
                None
            };
            if con_pool_len == 0 {
                drop(con_pool);
                if let Some(nu) = nu_con {
                    //just create a new one
                    nu.await.map(Raced::New)
                }else {
                    //wait for someone to do something (add a connection)
                    trace!("all is use. wait for someone to finish");
                    let p = self.pending.clone();
                    p.notified_owned().await;
                    //now the pool should have at least one
                    let con_pool = self.con_pool.read().await;
                    let waiting = con_pool
                    .iter()
                    .map(|c| Box::pin(c.prep_connection()))
                    .collect();
                    RaceConnections { nu_con, waiting }.await
                }
            }else{
                let waiting = con_pool
                .iter()
                .map(|c| Box::pin(c.prep_connection()))
                .collect();
                RaceConnections { nu_con, waiting }.await
            }
        };
        let (con, slot) = match rc {
            Ok(Raced::Prep((i, slot))) => {
                trace!("using con {} {:?}", i, req.uri().query());
                self.connecting.fetch_add(1, Ordering::Relaxed);
                let con = self.con_pool.write().await.swap_remove(i);
                (con, slot)
            }
            Ok(Raced::New(con)) => {
                trace!("new con estab {:?}", req.uri().query());
                let slot = con.prep_connection().await?;
                trace!("using new con {:?}", req.uri().query());
                (con, slot)
            }
            Err(e) => {
                return Err(e);
            }
        };
        let res = con.send_request(req, dyn_headers, slot).await;
        self.con_pool.write().await.push(con);
        self.connecting.fetch_sub(1, Ordering::Relaxed);
        self.pending.notify_one();
        res
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
//mod pool;

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
    New(Connection),
}
struct RaceConnections<FutNew, FutWait>
where
    FutNew: Future<Output = Result<Connection, IoError>>,
    FutWait: Future<Output = Result<PreparedConnection, IoError>>,
{
    nu_con: Option<Pin<Box<FutNew>>>,
    waiting: Vec<Pin<Box<FutWait>>>,
}
impl<FutNew, FutWait> Future for RaceConnections<FutNew, FutWait>
where
    FutNew: Future<Output = Result<Connection, IoError>>,
    FutWait: Future<Output = Result<PreparedConnection, IoError>>,
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
                }
                Poll::Ready(Ok(con)) => {
                    return Poll::Ready(Ok(Raced::New(con)));
                }
                Poll::Pending => {}
            }
        }
        for (x, pc) in self.as_mut().waiting.iter_mut().enumerate() {
            let pc = pin!(pc);
            match pc.poll(cx) {
                Poll::Ready(Err(e)) => {
                    log::error!("Error when preping: {}", &e);
                    last_err = Some(e);
                    cx.waker().wake_by_ref();
                }
                Poll::Ready(Ok(con)) => {
                    return Poll::Ready(Ok(Raced::Prep((x, con))));
                }
                Poll::Pending => {}
            }
        }
        if let Some(e) = last_err.take() {
            Poll::Ready(Err(e))
        } else {
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