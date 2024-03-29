/*! A single connection (TCP or Unix) to an FCGI application.

Multiple Requests can be multiplexed on it.

# Example
```
# use std::collections::HashMap;
# use std::error::Error;
# use tokio::net::TcpListener;
# use bytes::BytesMut;
# use tokio::io::{AsyncReadExt, AsyncWriteExt};
# use std::net::SocketAddr;
use http::{Request, StatusCode};
use http_body::{Body};
use bytes::Bytes;
use async_fcgi::client::connection::Connection;

# #[tokio::main(flavor = "current_thread")]
# async fn main() -> Result<(),Box<dyn Error>> {
#    let sa: SocketAddr = "127.0.0.1:59000".parse()?;
#    let app_listener = TcpListener::bind(sa).await?;
#    tokio::spawn(async move {
#        let (mut app_socket, _) = app_listener.accept().await.unwrap();
#        let mut buf = BytesMut::with_capacity(4096);
#        app_socket.read_buf(&mut buf).await.unwrap();
#        let from_php = b"\x01\x06\0\x01\x00\x38\0\0Status: 404 Not Found\r\nX-Powered-By: PHP/7.3.16\r\n\r\ntest!\x01\x03\0\x01\0\x08\0\0\0\0\0\0\0\0\0\0";
#        app_socket.write_buf(&mut Bytes::from(&from_php[..])).await.unwrap();
#    });
    let mut fcgi_con = Connection::connect(&"127.0.0.1:59000".parse()?, 1).await?;
    let req = Request::get("/test?lol=1").header("Accept", "text/html").body(String::new())?;
    let mut params = HashMap::new();
    params.insert(
        Bytes::from(&b"SCRIPT_FILENAME"[..]),
        Bytes::from(&b"/home/daniel/Public/test.php"[..]),
    );
    let mut res = fcgi_con.forward(req, params).await?;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(res.headers().get("X-Powered-By").unwrap(), "PHP/7.3.16");
    # Ok(())
# }
```
*/
use bytes::{Buf, BufMut, Bytes, BytesMut};
use http::{
    header::AUTHORIZATION, header::CONTENT_LENGTH, header::CONTENT_TYPE,
    Request, Response, StatusCode,
};
use http_body::{Body, Frame};
use slab::Slab;
use std::marker::Unpin;

use log::{debug, error, info, log_enabled, trace, warn, Level::Trace};

use std::error::Error;
use std::future::Future;
use std::io::{Error as IoError, ErrorKind};
use std::iter::IntoIterator;
use std::ops::Drop;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Waker;
use std::task::{Context, Poll};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

use crate::bufvec::BufList;
use crate::codec::{FCGIType, FCGIWriter};
use crate::fastcgi;
use crate::httpparse::{parse, ParseResult};
use async_stream_connection::{Addr, Stream};
use tokio::io::{AsyncBufRead, BufReader};

/// [http_body](https://docs.rs/http-body/0.3.1/http_body/trait.Body.html) type for FCGI.
///
/// This is the STDOUT of an FastCGI Application.
/// STDERR is logged using [log::error](https://doc.rust-lang.org/1.1.0/log/macro.error!.html)
struct FCGIBody {
    con: Arc<Mutex<InnerConnection>>, //where to read
    rid: u16,                         //my id
    done: bool,                       //no more data
    was_returned: bool,               //request is no longer polled by us
}
/// Request stream
///
/// Manages one request from
/// `FCGI_BEGIN_REQUEST` to `FCGI_END_REQUEST`
///
struct FCGIRequest {
    buf: BufList<Bytes>,           //stdout read by some task
    waker: Option<Waker>,          //wake me if needed
    /// the FCGI server is done with this request
    ended: bool,                   //fin reading
    /// was aborted (by dropping the FCGIBody)
    aborted: bool,
    _permit: OwnedSemaphorePermit, //block a multiplex slot
}
/// Shared object to read from a `Connection`
///
/// Manages all requests on it and distributes data to them
struct InnerConnection {
    io: FCGIWriter<BufReader<Stream>>,
    running_requests: Slab<FCGIRequest>,
    fcgi_parser: fastcgi::RecordReader,
}
/// Single transport connection to a FCGI application
///
/// Can multiplex `max_req_per_con` simultaneous request streams
pub struct Connection {
    inner: Arc<Mutex<InnerConnection>>,
    sem: Arc<Semaphore>,
    addr: Addr,
    header_mul: MultiHeaderStrategy,
    header_nl: HeaderMultilineStrategy,
}
/// Specifies how to handle multiple HTTP Headers
#[derive(Copy, Clone)]
pub enum MultiHeaderStrategy {
    /// RFC 3875: Combine then by joining them separated by `,`
    Combine,
    /// Only forward the first occurrence
    OnlyFirst,
    /// Only forward the last occurrence
    OnlyLast,
}
/// Specifies how to handle HTTP Headers that contain `\n`
#[derive(Copy, Clone)]
pub enum HeaderMultilineStrategy {
    /// Forward it to the FCGI server
    Ignore,
    /// RFC 7230: Return [`std::io::ErrorKind::InvalidData`]
    ReturnError,
}
impl Connection {
    /// Connect to a peer with [`MultiHeaderStrategy::OnlyFirst`] & [`HeaderMultilineStrategy::Ignore`].
    #[inline]
    pub async fn connect(
        addr: &Addr,
        max_req_per_con: u16,
    ) -> Result<Connection, Box<dyn Error>> {
        Self::connect_with_strategy(
            addr,
            max_req_per_con,
            MultiHeaderStrategy::OnlyFirst,
            HeaderMultilineStrategy::Ignore,
        )
        .await
    }
    /// Connect to a peer
    pub async fn connect_with_strategy(
        addr: &Addr,
        max_req_per_con: u16,
        header_mul: MultiHeaderStrategy,
        header_nl: HeaderMultilineStrategy,
    ) -> Result<Connection, Box<dyn Error>> {
        Ok(Connection {
            inner: Arc::new(Mutex::new(InnerConnection {
                io: FCGIWriter::new(BufReader::new(Stream::connect(addr).await?)),
                running_requests: Slab::with_capacity(max_req_per_con as usize),
                fcgi_parser: fastcgi::RecordReader::new(),
            })),
            sem: Arc::new(Semaphore::new(max_req_per_con as usize)),
            addr: addr.clone(),
            header_mul,
            header_nl,
        })
    }

    /// true if the next call to forward does not need to
    /// wait for the end of some previous request
    pub fn is_ready(&self) -> bool {
        self.sem.available_permits() > 0
    }

    pub async fn close(self) -> Result<(), IoError> {
        let mut mut_inner = self.inner.lock().await;
        mut_inner.io.shutdown().await?;
        mut_inner.notify_everyone();
        Ok(())
    }

    const QUERY_STRING: &'static [u8] = b"QUERY_STRING";
    const REQUEST_METHOD: &'static [u8] = b"REQUEST_METHOD";
    const CONTENT_TYPE: &'static [u8] = b"CONTENT_TYPE";
    const CONTENT_LENGTH: &'static [u8] = b"CONTENT_LENGTH";
    const NULL: &'static [u8] = b"";
    /// Forwards an HTTP request to a FGCI Application
    /// ```no_run
    /// # use std::error::Error;
    /// # use http::Request;
    /// # use async_fcgi::client::connection::Connection;
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> Result<(),Box<dyn Error>> {
    /// # let mut fcgi_con = Connection::connect(&"127.0.0.1:59000".parse()?, 1).await?;
    /// let req = Request::get("/test?lol=1").header("Accept", "text/html").body(String::new())?;
    /// let mut params = [(
    ///     &b"SCRIPT_FILENAME"[..],
    ///     &b"/home/daniel/Public/test.php"[..]
    /// )];
    /// let mut res = fcgi_con.forward(req, params).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Fills `QUERY_STRING`, `REQUEST_METHOD`, `CONTENT_TYPE` and `CONTENT_LENGTH`
    /// from the corresponding values in the Request.
    /// Headers from the Request will be added with the `HTTP_` prefix. (CGI/1.1 4.1.18)
    ///
    /// Additional Params might be expected from the application (at least the url path):
    /// 
    /// |Param             |Specification           |Info                       |
    /// |------------------|------------------------|---------------------------|
    /// | SCRIPT_NAME      |**must** CGI/1.1 4.1.13 | **required** in any case |
    /// | SERVER_NAME      |**must** CGI/1.1 4.1.14 | **required** by flup |
    /// | SERVER_PORT      |**must** CGI/1.1 4.1.15 | **required** by flup |
    /// | SERVER_PROTOCOL  |**must** CGI/1.1 4.1.16 | **required** by flup |
    /// | SERVER_SOFTWARE  |**must** CGI/1.1 4.1.17 | |
    /// | REMOTE_ADDR      |**must** CGI/1.1 4.1.8  | |
    /// | GATEWAY_INTERFACE|**must** CGI/1.1 4.1.4  | `"CGI/1.1"` |
    /// | REMOTE_HOST      |should CGI/1.1  4.1.9 | |
    /// | REMOTE_IDENT     |may CGI/1.1  4.1.10 | |
    /// | REMOTE_USER      |opt CGI/1.1 | |
    /// | AUTH_TYPE        |opt CGI/1.1 | |
    /// | PATH_INFO        |opt CGI/1.1   4.1.5 |extra-path|
    /// | PATH_TRANSLATED  |opt CGI/1.1   4.1.6|
    /// | SCRIPT_FILENAME  | | **required** by PHP |
    /// | REMOTE_PORT      | | common |
    /// | SERVER_ADDR      | | common |
    /// | REQUEST_URI      | | common |
    /// | DOCUMENT_URI     | | common |
    /// | DOCUMENT_ROOT    | | common |
    pub async fn forward<B, I, P1, P2>(
        &self,
        req: Request<B>,
        dyn_headers: I,
    ) -> Result<Response<impl Body<Data = Bytes, Error = IoError>>, IoError>
    where
        B: Body + Unpin,
        I: IntoIterator<Item = (P1, P2)>,
        P1: Buf,
        P2: Buf,
    {
        info!("new request pending");
        let _permit = self
            .sem
            .clone()
            .acquire_owned()
            .await
            .map_err(|_e| IoError::new(ErrorKind::WouldBlock, ""))?;
        let meta = FCGIRequest {
            buf: BufList::new(),
            waker: None,
            ended: false,
            aborted: false,
            _permit,
        };

        info!("wait for lock");
        let mut mut_inner = self.inner.lock().await;

        if mut_inner.check_alive().await? == false {
            // we need to connect again
            info!("reconnect...");
            if let Err(e) = mut_inner.io.shutdown().await {
                error!("shutdown old con: {}", e);
            }
            mut_inner.notify_everyone();
            mut_inner.io = FCGIWriter::new(BufReader::new(Stream::connect(&self.addr).await?));
            mut_inner.fcgi_parser = fastcgi::RecordReader::new();
            info!("reconnected");
        }

        let rid = (mut_inner.running_requests.insert(meta) + 1) as u16;
        info!("started req #{}", rid);
        //entry.insert(meta);

        let br = FCGIType::BeginRequest {
            request_id: rid,
            role: fastcgi::FastCGIRole::Responder,
            flags: fastcgi::BeginRequestBody::KEEP_CONN,
        };
        mut_inner.io.encode(br).await?;
        //Prepare the CGI headers
        let mut kvw = mut_inner.io.kv_stream(rid, fastcgi::RecordType::Params);

        kvw.extend(dyn_headers).await?;

        match req.uri().query() {
            Some(query) => kvw.add_kv(Self::QUERY_STRING, query.as_bytes()).await?, //must CGI1.1 4.1.7
            None => kvw.add_kv(Self::QUERY_STRING, Self::NULL).await?, //must CGI1.1 4.1.7
        }

        kvw.add_kv(Self::REQUEST_METHOD, req.method().as_str().as_bytes())
            .await?; //must CGI1.1 4.1.12

        let (parts, body) = req.into_parts();
        let headers = parts.headers;

        if let Some(value) = headers.get(CONTENT_TYPE) {
            //if client CGI1.1 4.1.3.
            kvw.add_kv(Self::CONTENT_TYPE, value.as_bytes()).await?;
        }

        let len: Option<usize> = if Some(0) == body.size_hint().upper() {
            //if exact and 0 -> no body
            None
        } else {
            //if exact (content len present) -> lower==upper
            //if unknown -> at least lower, maybe 0
            let value = body.size_hint().lower(); //if body CGI1.1 4.1.2.
            kvw.add_kv(Self::CONTENT_LENGTH, value.to_string().as_bytes())
                .await?;
            Some(value as usize)
        };
        let skip = [AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE];
        //append all HTTP headers
        for key in headers.keys() {
            if skip.iter().find(|x| x == key).is_some() {
                //CGI1.1 4.1.18.
                continue;
            }
            /*rfc3875
            The HTTP header field name is converted to upper case, has all
            occurrences of "-" replaced with "_" and has "HTTP_" prepended to
            give the meta-variable name.
            */
            let mut k = BytesMut::with_capacity(key.as_str().len() + 5);
            k.put(&b"HTTP_"[..]);
            for &c in key.as_str().as_bytes() {
                let upper = match c {
                    b'-' => b'_',
                    lower_acii if b'a' <= lower_acii && lower_acii <= b'z' => {
                        lower_acii - (b'a' - b'A')
                    } //a ... z
                    s => s,
                };
                k.put_u8(upper);
            }
            /*rfc3875
            If multiple header fields with the same field-name
            are received then the server MUST rewrite them as a single value
            having the same semantics.  Similarly, a header field that spans
            multiple lines MUST be merged onto a single line.

            RFC 7230, Section 3.2.2, Field Order: Set-Cookie is special
            -> but not part of a request

            RFC 7230, Section 3.2.4, Field Parsing: multiline header -> 400/502
            */
            let mut value_buf;
            let value = match self.header_mul {
                MultiHeaderStrategy::Combine => {
                    value_buf = BytesMut::with_capacity(512);
                    let mut first = false;
                    for v in headers.get_all(key).iter() {
                        if !first {
                            first = true;
                        } else {
                            value_buf.put_u8(b',');
                        }
                        let v = v.as_bytes();
                        value_buf.put_slice(v); //copy
                    }
                    value_buf.as_ref()
                }
                MultiHeaderStrategy::OnlyFirst => match headers.get(key) {
                    Some(v) => v.as_bytes(),
                    None => Self::NULL,
                },
                MultiHeaderStrategy::OnlyLast => match headers.get_all(key).iter().next_back() {
                    Some(v) => v.as_bytes(),
                    None => Self::NULL,
                },
            };
            if let HeaderMultilineStrategy::ReturnError = self.header_nl {
                if value.as_ref().contains(&b'\n') {
                    drop(kvw); //stop mid stream
                    mut_inner.abort_req(rid).await?; //abort request
                    return Err(IoError::new(
                        ErrorKind::InvalidData,
                        "multiline headers are not allowed",
                    ));
                }
            }
            kvw.add_kv(k, value).await?;
        }
        //send all headers to the FCGI App
        kvw.flush().await?;
        trace!("sent header");
        //Note: Responses might arrive from this point on

        if let Some(len) = len {
            drop(mut_inner); // close mutex before create_response or/and send_body
                             // send the body to the FCGI App
                             // and read responses
            let (_, res) =
                tokio::try_join!(self.send_body(rid, len, body), self.create_response(rid))?;
            Ok(res)
        } else {
            //send end of STDIN
            mut_inner
                .io
                .flush_data_chunk(Self::NULL, rid, fastcgi::RecordType::StdIn)
                .await?;
            drop(mut_inner); // close mutex before create_response
            self.create_response(rid).await
        }
    }
    /// send the body to the FCGI App as STDIN
    async fn send_body<B>(
        &self,
        request_id: u16,
        mut len: usize,
        mut body: B,
    ) -> Result<(), IoError>
    where
        B: Body + Unpin,
    {
        //stream as body comes in
        while let Some(chunk) = body.data().await {
            if let Ok(data) = chunk {
                let s = data.remaining();
                debug!("sent {} body bytes to app", s);
                if s == 0 {
                    continue;
                }
                len -= s;
                self.inner
                    .lock()
                    .await
                    .io
                    .flush_data_chunk(data, request_id, fastcgi::RecordType::StdIn)
                    .await?;
            }
        }
        //CGI1.1 4.2 -> at least content-length data
        if len > 0 {
            self.inner
                .lock()
                .await
                .abort_req(request_id).await?;
            return Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "body too short",
            ));
        }
        //empty record to end STDIN steam FCGI1.0
        self.inner
            .lock()
            .await
            .io
            .flush_data_chunk(Self::NULL, request_id, fastcgi::RecordType::StdIn)
            .await?;

        debug!("sent req body");
        Ok(())
    }
    /// Poll the STDOUT response of the FCGI Server
    /// Parse the Headers and return a body that streams the rest
    async fn create_response(
        &self,
        rid: u16,
    ) -> Result<Response<impl Body<Data = Bytes, Error = IoError>>, IoError> {
        let mut fcgibody = FCGIBody {
            con: Arc::clone(&self.inner),
            rid: (rid - 1),
            done: false,
            was_returned: false,
        };
        let mut rb = Response::builder();
        let mut rheaders = rb.headers_mut().unwrap();
        let mut status = StatusCode::OK;
        //read the headers
        let mut buf: Option<Bytes> = None;
        while let Some(rbuf) = fcgibody.data().await {
            if let Ok(mut b) = rbuf {
                if let Some(left) = buf.take() {
                    //we have old data -> concat
                    let mut c = BytesMut::with_capacity(left.len() + b.len());
                    c.put(left);
                    c.put(b);
                    b = c.freeze();
                }
                match parse(b.clone(), &mut rheaders) {
                    ParseResult::Ok(bodydata) => {
                        trace!("read body fragment: {:?}", &bodydata);
                        if bodydata.has_remaining() {
                            let mut mut_inner = self.inner.lock().await;
                            //was_returned prevents: request might already be done and gone
                            mut_inner.running_requests[fcgibody.rid as usize]
                                .buf
                                .push(bodydata);
                        }

                        if let Some(stat) = rheaders.get("Status") {
                            //CGI1.1
                            //info!("Status header: {:?}", stat);
                            if stat.len() >= 3 {
                                if let Ok(s) = StatusCode::from_bytes(&stat.as_bytes()[..3][..]) {
                                    status = s;
                                }
                            }
                        }
                        //Location header for local URIs (starting with "/") -> must be done in Webserver
                        break;
                    }
                    ParseResult::Pending => {
                        //read more
                        buf = Some(b);
                        trace!("header pending");
                    }
                    ParseResult::Err => {
                        status = StatusCode::INTERNAL_SERVER_ERROR;
                        break;
                    }
                }
            } else {
                error!("{:?}", rbuf);
            }
        }
        fcgibody.was_returned = true;
        debug!("resp header parsing done");

        match rb.status(status).body(fcgibody) {
            Ok(v) => Ok(v),
            Err(_) => {
                //all headers are parsed ok, so they should be fine
                unreachable!();
            }
        }
    }
}

/// `Frame` from `http-body-util` but only returns data frames
pub(crate) struct BodyDataFrame<'a, T: ?Sized>(pub(crate) &'a mut T);
impl<'a, T: Body + Unpin + ?Sized> Future for BodyDataFrame<'a, T> {
    type Output = Option<Result<T::Data, T::Error>>;

    fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.0).poll_frame(ctx) {
            Poll::Ready(Some(Ok(a))) => {
                if let Ok(d) = a.into_data() {
                    Poll::Ready(Some(Ok(d)))
                }else{
                    ctx.waker().wake_by_ref();
                    Poll::Pending
                }
            },
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending
        }
    }
}
pub(crate) trait BodyExt: Body {
    /// Returns a future that resolves to the next [`Frame`], if any.
    ///
    /// [`Frame`]: combinators::Frame
    fn data(&mut self) -> BodyDataFrame<'_, Self>
    where
        Self: Unpin,
    {
        BodyDataFrame(self)
    }
}
impl<T: ?Sized> BodyExt for T where T: Body {}

impl Future for InnerConnection {
    type Output = Option<Result<(), IoError>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<(), IoError>>> {
        self.poll_resp(cx)
    }
}
struct CheckAlive<'a>(&'a mut InnerConnection);

impl<'a> Future for CheckAlive<'a> {
    type Output = Result<bool, IoError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<bool, IoError>> {
        Poll::Ready(match Pin::new(&mut *self.0).poll_resp(cx) {
            Poll::Ready(None) => Ok(false),
            Poll::Ready(Some(Err(e))) => {
                error!("allive: {:?}", e);
                if e.kind() == ErrorKind::NotConnected {
                    Ok(false)
                } else {
                    Err(e)
                }
            }
            _ => Ok(true),
        })
    }
}

impl InnerConnection {
    ///returns true if the connection is still alive
    fn check_alive(&mut self) -> CheckAlive {
        CheckAlive(self)
    }
    async fn abort_req(&mut self, request_id: u16) -> Result<(), IoError> {
        self.io.encode(FCGIType::AbortRequest { request_id }).await
    }
    /// drive this connection
    /// Read, parse and distribute data from the socket.
    /// return None if the connection was closed
    fn poll_resp(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Result<(), IoError>>> {
        let Self {
            ref mut io,
            ref mut running_requests,
            ref mut fcgi_parser,
        } = *self;
        /*
        1. Read from Socket
        2. Parse all the Data and put it in the corresponding OutBuffer
        3. Notify those with new Data
        */
        let read = match Pin::new(io).poll_fill_buf(cx) {
            Poll::Ready(Ok(rbuf)) => {
                let data_available = rbuf.len();
                if data_available == 0 {
                    info!("connection closed");
                    0
                } else {
                    let mut data = Bytes::copy_from_slice(rbuf);
                    if log_enabled!(Trace) {
                        let print = if data.len() > 50 {
                            format!(
                                "({}) {:?}...{:?}",
                                data.len(),
                                data.slice(..21),
                                data.slice(data.len() - 21..)
                            )
                        } else {
                            format!("{:?}", data)
                        };
                        trace!("read conn data {}", print);
                    }
                    InnerConnection::parse_and_distribute(&mut data, running_requests, fcgi_parser);
                    let read = data_available - data.remaining();
                    read
                }
            }
            Poll::Ready(Err(e)) => {
                error!("Err {}", e);
                self.notify_everyone();
                return Poll::Ready(Some(Err(e)));
            }
            Poll::Pending => return Poll::Pending,
        };
        if read == 0 {
            self.notify_everyone();
            Poll::Ready(None)
        } else {
            Pin::new(&mut (*self).io).consume(read);
            Poll::Ready(Some(Ok(())))
        }
    }
}
impl Drop for FCGIRequest {
    fn drop(&mut self) {
        debug!("Req mplex id free");
    }
}
impl Drop for FCGIBody {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        debug!("Dropping FCGIBody #{}!", self.rid + 1);
        match self.con.try_lock() {
            Ok(mut mut_inner) => {
                let rid = self.rid as usize;
                if let Some(req) = mut_inner.running_requests.get_mut(rid) {
                    req.aborted = true;
                    req.waker = None;
                    //TODO drop the data already
                }
            }
            Err(e) => error!("{}", e),
        }
    }
}

impl Body for FCGIBody {
    type Data = Bytes;
    type Error = IoError;
    /// Get a chunk of STDOUT data from this FCGI application request stream
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        /*
        We need to read the socket because we
        a. are the only request
        b. have to wake another task

        1. Read InnerConnection
        4. Check if we now have data
        */
        let Self {
            ref con,
            rid,
            ref mut done,
            was_returned,
        } = *self;

        if *done {
            debug!("body #{} is already done", rid + 1);
            return Poll::Ready(None);
        }

        trace!("read resp body");
        let fut = con.lock();
        match Box::pin(fut).as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(mut mut_inner) => {
                // mut_inner: InnerConnection<S>

                //poll connection and distribute new data
                let _con_stat = Pin::new(&mut *mut_inner).poll_resp(cx);

                //work with slab buffer
                let slab = match mut_inner.running_requests.get_mut(rid as usize) {
                    Some(slab) => slab,
                    None => {
                        warn!("#{} not in slab", rid + 1);
                        *done = true;
                        return Poll::Ready(None);
                    }
                };

                /*
                if let Poll::Ready(Some(Err(e))) = con_stat {
                    error!("body #{} (done: {}) err {}", rid, slab.ended, e);
                    if !slab.ended {//unreachable
                        //request is not done but an error occured
                        return Poll::Ready(Some(Err(e)));
                    }
                }*/

                if slab.buf.remaining() >= 1 {
                    trace!("body #{} has data and is {} closed", rid + 1, slab.ended);
                    let retdata = Poll::Ready(Some(Ok(Frame::data(slab.buf.oldest().unwrap()))));
                    if was_returned && slab.ended && slab.buf.remaining() < 1 {
                        //ret rid of this as fast as possible,
                        //it blocks us and clients might stop reading
                        trace!("next read on #{} will not have data -> release", rid + 1);
                        mut_inner.running_requests.remove(rid as usize);
                        *done = true;
                    }
                    retdata
                } else {
                    //data buffer empty
                    let req_done = slab.ended;
                    if req_done {
                        debug!("body #{} is done", rid + 1);
                        if was_returned {
                            mut_inner.running_requests.remove(rid as usize);
                            *done = true;
                        } else {
                            warn!("#{} closed before handover", rid + 1);
                        }
                        Poll::Ready(None)
                    } else {
                        trace!("body waits");
                        //store waker
                        slab.waker = Some(cx.waker().clone());
                        Poll::Pending
                    }
                }
            }
        }
    }
}

impl InnerConnection {
    /// Something happened. We are done with everything
    fn notify_everyone(&mut self) {
        for (rid, mpxs) in self.running_requests.iter_mut() {
            if let Some(waker) = mpxs.waker.take() {
                waker.wake()
            }
            if mpxs.aborted {
                continue;
            }
            if !mpxs.ended {
                error!("body #{} not done", rid + 1);
            }
            mpxs.ended = true;
        }
    }
    fn parse_and_distribute(
        data: &mut Bytes,
        running_requests: &mut Slab<FCGIRequest>,
        fcgi_parser: &mut fastcgi::RecordReader,
    ) {
        //trace!("parse {:?}", &data);
        while let Some(r) = fcgi_parser.read(data) {
            let (req_no, ovr) = r.get_request_id().overflowing_sub(1);
            if ovr {
                //req id 0
                error!("got mgmt record");
                continue;
            }
            debug!("record for #{}", req_no + 1);
            if let Some(mpxs) = running_requests.get_mut(req_no as usize) {
                match r.body {
                    fastcgi::Body::EndRequest(status) => {
                        match status.protocol_status {
                            fastcgi::ProtoStatus::Complete => {
                                info!("Req #{} ended with {}", req_no + 1, status.app_status)
                            }
                            //CANT_MPX_CONN => ,
                            //TODO handle OVERLOADED
                            _ => error!(
                                "Req #{} ended with fcgi error {}",
                                req_no + 1,
                                status.protocol_status
                            ),
                        };
                        mpxs.ended = true;
                        if let Some(waker) = mpxs.waker.take() {
                            waker.wake()
                        }
                        if mpxs.aborted {
                            // fcgi server is also done with it
                            running_requests.remove(req_no as usize);
                        }
                    }
                    _ if mpxs.aborted => {
                        //TODO send abort
                        continue;
                    }
                    fastcgi::Body::StdOut(s) => {                        
                        if log_enabled!(Trace) {
                            let print = if s.len() > 50 {
                                format!(
                                    "({}) {:?}...{:?}",
                                    s.len(),
                                    s.slice(..21),
                                    s.slice(s.len() - 21..)
                                )
                            } else {
                                format!("{:?}", s)
                            };
                            trace!("FCGI stdout: {}", print);
                        }
                        if s.has_remaining() {
                            mpxs.buf.push(s);
                            if let Some(waker) = mpxs.waker.take() {
                                waker.wake();
                            }
                        }
                    }
                    fastcgi::Body::StdErr(s) => {
                        error!("FCGI #{} Err: {:?}", req_no + 1, s);
                    }
                    _ => {
                        warn!("type?");
                    }
                }
            } else {
                debug!("not a pending req ID");
                //TODO send abort
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::tests::local_socket_pair;
    use http_body::SizeHint;
    use std::collections::{HashMap, VecDeque};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        runtime::Builder,
    };

    struct TestBod {
        l: VecDeque<Bytes>,
    }
    impl Body for TestBod {
        type Data = Bytes;
        type Error = IoError;
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
    fn init_log() {
        let mut builder = pretty_env_logger::formatted_timed_builder();
        builder.is_test(true);
        if let Ok(s) = ::std::env::var("RUST_LOG") {
            builder.parse_filters(&s);
        }
        let _ = builder.try_init();
    }

    #[test]
    fn simple_get() {
        init_log();
        // Create the runtime
        let rt = Builder::new_current_thread().enable_all().build().unwrap();
        async fn mock_app(app_listener: TcpListener) {
            let (mut app_socket, _) = app_listener.accept().await.unwrap();
            let mut buf = BytesMut::with_capacity(4096);
            app_socket.read_buf(&mut buf).await.unwrap();
            trace!("app read {:?}", buf);
            let to_php = b"\x01\x01\0\x01\0\x08\0\0\0\x01\x01\0\0\0\0\0\x01\x04\0\x01\0i\x07\0\x0f\x1cSCRIPT_FILENAME/home/daniel/Public/test.php\x0c\x05QUERY_STRINGlol=1\x0e\x03REQUEST_METHODGET\x0b\tHTTP_ACCEPTtext/html\x01\x04\0\x01\0i\x07\x01\x04\0\x01\0\0\0\0\x01\x05\0\x01\0\0\0\0";
            assert_eq!(buf, Bytes::from(&to_php[..]));
            trace!("app answers on get");
            let from_php = b"\x01\x07\0\x01\0W\x01\0PHP Fatal error:  Kann nicht durch 0 teilen in /home/daniel/Public/test.php on line 14\n\0\x01\x06\0\x01\x01\xf7\x01\0Status: 404 Not Found\r\nX-Powered-By: PHP/7.3.16\r\nX-Authenticate: NTLM\r\nContent-type: text/html; charset=UTF-8\r\n\r\n<html><body>\npub\n<pre>Array\n(\n)\nArray\n(\n    [lol] => 1\n)\nArray\n(\n    [lol] => 1\n)\nArray\n(\n    [HTTP_accept] => text/html\n    [REQUEST_METHOD] => GET\n    [QUERY_STRING] => lol=1\n    [SCRIPT_NAME] => /test\n    [SCRIPT_FILENAME] => /home/daniel/Public/test.php\n    [FCGI_ROLE] => RESPONDER\n    [PHP_SELF] => /test\n    [REQUEST_TIME_FLOAT] => 1587740954.2741\n    [REQUEST_TIME] => 1587740954\n)\n\0\x01\x03\0\x01\0\x08\0\0\0\0\0\0\0\0\0\0";
            app_socket
                .write_buf(&mut Bytes::from(&from_php[..]))
                .await
                .unwrap();
        }

        async fn con() {
            let (app_listener, a) = local_socket_pair().await.unwrap();
            let m = tokio::spawn(mock_app(app_listener));

            let fcgi_con = Connection::connect(&a, 1).await.unwrap();
            trace!("new connection obj");
            let b = TestBod { l: VecDeque::new() };
            let req = Request::get("/test?lol=1")
                .header("Accept", "text/html")
                .body(b)
                .unwrap();
            trace!("new req obj");
            let mut params = HashMap::new();
            params.insert(
                &b"SCRIPT_FILENAME"[..],
                &b"/home/daniel/Public/test.php"[..],
            );
            let mut res = fcgi_con.forward(req, params).await.expect("forward failed");
            trace!("got res obj");
            assert_eq!(res.status(), StatusCode::NOT_FOUND);
            assert_eq!(
                res.headers()
                    .get("X-Powered-By")
                    .expect("powered by header missing"),
                "PHP/7.3.16"
            );
            let read1 = res.data().await;
            assert!(read1.is_some());
            let read1 = read1.unwrap();
            assert!(read1.is_ok());
            if let Ok(d) = read1 {
                let body = b"<html><body>\npub\n<pre>Array\n(\n)\nArray\n(\n    [lol] => 1\n)\nArray\n(\n    [lol] => 1\n)\nArray\n(\n    [HTTP_accept] => text/html\n    [REQUEST_METHOD] => GET\n    [QUERY_STRING] => lol=1\n    [SCRIPT_NAME] => /test\n    [SCRIPT_FILENAME] => /home/daniel/Public/test.php\n    [FCGI_ROLE] => RESPONDER\n    [PHP_SELF] => /test\n    [REQUEST_TIME_FLOAT] => 1587740954.2741\n    [REQUEST_TIME] => 1587740954\n)\n";
                assert_eq!(d, &body[..]);
            }
            let read2 = res.data().await;
            assert!(read2.is_none());
            m.await.unwrap();
        }
        rt.block_on(con());
    }
    #[test]
    fn app_answer_split_mid_record() {
        //flup did this once
        init_log();
        // Create the runtime
        let rt = Builder::new_current_thread().enable_all().build().unwrap();
        async fn mock_app(app_listener: TcpListener) {
            let (mut app_socket, _) = app_listener.accept().await.unwrap();
            let mut buf = BytesMut::with_capacity(4096);
            app_socket.read_buf(&mut buf).await.unwrap();
            trace!("app read {:?}", buf);
            trace!("app answers on get");
            let from_flup = b"\x01\x06\0\x01\0@\0\0Status: 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 13\r\n\r\n\x01\x06\0\x01\0\r\x03\0Hello World!\n";
            app_socket
                .write_buf(&mut Bytes::from(&from_flup[..]))
                .await
                .unwrap();
        }

        async fn con() {
            let (app_listener, a) = local_socket_pair().await.unwrap();
            let m = tokio::spawn(mock_app(app_listener));

            let fcgi_con = Connection::connect(&a, 1).await.unwrap();
            trace!("new connection obj");
            let b = TestBod { l: VecDeque::new() };
            let req = Request::get("/").body(b).unwrap();
            trace!("new req obj");
            let params: HashMap<Bytes, Bytes> = HashMap::new();
            let mut res = fcgi_con.forward(req, params).await.expect("forward failed");
            trace!("got res obj");
            let read1 = res.data().await;
            assert!(read1.is_some());
            let read1 = read1.unwrap();
            assert!(read1.is_ok());
            if let Ok(d) = read1 {
                let body = b"Hello World!\n";
                assert_eq!(d, &body[..]);
            }
            m.await.unwrap();
        }
        rt.block_on(con());
    }

    #[test]
    fn app_http_headers_split() {
        init_log();
        // Create the runtime
        let rt = Builder::new_current_thread().enable_all().build().unwrap();
        async fn mock_app(app_listener: TcpListener) {
            let (mut app_socket, _) = app_listener.accept().await.unwrap();
            let mut buf = BytesMut::with_capacity(4096);
            app_socket.read_buf(&mut buf).await.unwrap();
            trace!("app read {:?}", buf);
            trace!("app answers on get");
            let from_flup = b"\x01\x06\0\x01\0\x1e\0\0Status: 200 OK\r\nContent-Type: ";
            app_socket
                .write_buf(&mut Bytes::from(&from_flup[..]))
                .await
                .unwrap();
            let from_flup = b"\x01\x06\0\x01\0\"\0\0text/plain\r\nContent-Length: 13\r\n\r\n\x01\x03\0\x01\0\x08\0\0\0\0\0\0\0\0\0\0";
            app_socket
                .write_buf(&mut Bytes::from(&from_flup[..]))
                .await
                .unwrap();
        }

        async fn con() {
            let (app_listener, a) = local_socket_pair().await.unwrap();
            let m = tokio::spawn(mock_app(app_listener));

            let fcgi_con = Connection::connect(&a, 1).await.unwrap();
            trace!("new connection obj");
            let b = TestBod { l: VecDeque::new() };
            let req = Request::get("/").body(b).unwrap();
            trace!("new req obj");
            let params: HashMap<Bytes, Bytes> = HashMap::new();
            let mut res = fcgi_con.forward(req, params).await.expect("forward failed");
            trace!("got res obj");
            assert_eq!(res.status(), StatusCode::OK);
            assert_eq!(
                res.headers()
                    .get("Content-Length")
                    .expect("len header missing"),
                "13"
            );
            assert_eq!(
                res.headers()
                    .get("Content-Type")
                    .expect("type header missing"),
                "text/plain"
            );

            let read1 = res.data().await;
            assert!(read1.is_none());
            m.await.unwrap();
        }
        rt.block_on(con());
    }
    #[test]
    fn simple_post() {
        init_log();
        // Create the runtime
        let rt = Builder::new_current_thread().enable_all().build().unwrap();
        async fn mock_app(app_listener: TcpListener) {
            let (mut app_socket, _) = app_listener.accept().await.unwrap();
            let mut buf = BytesMut::with_capacity(4096);
            app_socket.read_buf(&mut buf).await.unwrap();
            trace!("app read {:?}", buf);
            let to_php = b"\x01\x01\0\x01\0\x08\0\0\0\x01\x01\0\0\0\0\0\x01\x04\0\x01\0\x81\x07\0\x0f\x1cSCRIPT_FILENAME/home/daniel/Public/test.php\x0c\0QUERY_STRING\x0e\x04REQUEST_METHODPOST\x0c\x13CONTENT_TYPEmultipart/form-data\x0e\x01CONTENT_LENGTH8\x01\x04\0\x01\0\x81\x07\x01\x04\0\x01\0\0\0\0\x01\x05\0\x01\0\x08\0\0test=123\x01\x05\0\x01\0\0\0\0";
            assert_eq!(buf, Bytes::from(&to_php[..]));
            trace!("app answers on get");
            let from_php = b"\x01\x06\0\x01\x00\x23\x05\0Status: 201 Created\r\n\r\n<html><body>#+#+#\x01\x03\0\x01\0\x08\0\0\0\0\0\0\0\0\0\0";
            app_socket
                .write_buf(&mut Bytes::from(&from_php[..]))
                .await
                .unwrap();
        }

        async fn con() {
            let (app_listener, a) = local_socket_pair().await.unwrap();
            let m = tokio::spawn(mock_app(app_listener));

            let fcgi_con = Connection::connect(&a, 1).await.unwrap();
            trace!("new connection obj");
            let mut l = VecDeque::new();
            l.push_back(Bytes::from(&"test=123"[..]));
            let b = TestBod { l };

            let req = Request::post("/test")
                .header("Content-Length", "8")
                .header("Content-Type", "multipart/form-data")
                .body(b)
                .unwrap();
            trace!("new req obj");
            let mut params = HashMap::new();
            params.insert(
                &b"SCRIPT_FILENAME"[..],
                &b"/home/daniel/Public/test.php"[..],
            );
            let mut res = fcgi_con.forward(req, params).await.expect("forward failed");
            trace!("got res obj");
            assert_eq!(res.status(), StatusCode::CREATED);
            let read1 = res.data().await;
            assert!(read1.is_some());
            let read1 = read1.unwrap();
            assert!(read1.is_ok());
            if let Ok(d) = read1 {
                let body = b"<html><body>";
                assert_eq!(d, &body[..]);
            }
            let read2 = res.data().await;
            assert!(read2.is_none());
            m.await.unwrap();
        }
        rt.block_on(con());
    }
    #[test]
    fn long_header() {
        init_log();
        // Create the runtime
        let rt = Builder::new_current_thread().enable_all().build().unwrap();
        async fn mock_app(app_listener: TcpListener) {
            let (mut app_socket, _) = app_listener.accept().await.unwrap();
            let mut buf = BytesMut::with_capacity(4096);
            app_socket.read_buf(&mut buf).await.unwrap();
            trace!("app read {:?}", buf);
            let to_php = b"\x01\x01\0\x01\0\x08\0\0\0\x01\x01\0\0\0\0\0\x01\x04\0\x01\0\xb8\0\0\x0c\0QUERY_STRING\x0e\x03REQUEST_METHODGET\x0b\x80\0\0\x87HTTP_ACCEPTtext/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7\x01\x04\0\x01\0\0\0\0\x01\x05\0\x01\0\0\0\0";
            assert_eq!(buf, Bytes::from(&to_php[..]));
            trace!("app answers on get");
            let from_php = b"\x01\x06\0\x01\0\x1b\x05\0Status: 404 Not Found\r\n\r\n\r\n\x01\x06\0\x01\0\x01\x03\0\x01\0\x08\0\0\0\0\0\0\0\0\0\0";
            app_socket
                .write_buf(&mut Bytes::from(&from_php[..]))
                .await
                .unwrap();
        }

        async fn con() {
            let (app_listener, a) = local_socket_pair().await.unwrap();
            let m = tokio::spawn(mock_app(app_listener));

            let fcgi_con = Connection::connect(&a, 1).await.unwrap();
            trace!("new connection obj");
            let b = TestBod { l: VecDeque::new() };
            let req = Request::get("/")
                .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7")
                .body(b)
                .unwrap();
            trace!("new req obj");
            let params: HashMap<Bytes, Bytes> = HashMap::new();
            let res = fcgi_con.forward(req, params).await.expect("forward failed");
            trace!("got res obj");
            assert_eq!(res.status(), StatusCode::NOT_FOUND);
            m.await.unwrap();
        }
        rt.block_on(con());
    }
}
