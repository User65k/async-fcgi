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
use std::fmt::Display;
use std::marker::Unpin;

use log::{debug, error, info, log_enabled, trace, warn, Level::Trace};

use std::future::Future;
use std::io::{Error as IoError, ErrorKind};
use std::iter::IntoIterator;
use std::ops::Drop;
use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::task::Waker;
use std::task::{Context, Poll};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

use crate::bufvec::BufList;
use crate::codec::{FCGIType, FCGIWriter};
use crate::fastcgi;
use crate::httpparse::{parse, ParseResult};
use async_stream_connection::{Addr, Stream};
use tokio::io::{AsyncBufRead, BufReader};

/// state of the body
enum ServerState {
    /// the server closed STDOUT
    Done(u16),
    /// the server is still sending answers.
    /// We can abort
    Running(ServerRequestId)
}
impl ServerState {
    pub fn id(&self) -> u16 {
        match self {
            ServerState::Done(id) => *id,
            ServerState::Running(server_request_id) => server_request_id.id,
        }
    }
    /// this is done
    pub fn mark_done(&mut self) {
        match core::mem::replace(self, ServerState::Done(self.id())) {
            ServerState::Done(_) => {},
            ServerState::Running(server_request_id) => server_request_id.mark_complete(),
        };
    }
}
/// Request stream
///
/// Manages one request from
/// `FCGI_BEGIN_REQUEST` to `FCGI_END_REQUEST`
///
struct FCGIRequest {
    //stdout for this task, read by some other task
    buf: BufList<Bytes>,
    ///wake me if needed
    waker: Option<Waker>,
    /// the FCGI server is done with this request
    ended: bool,
    /// respect FCGI setting for maximal requests on a single con
    _permit: OwnedSemaphorePermit,
}
/// The ID of a single request. Dropping it cancels the request
struct ServerRequestId {
    id: u16,
    con: Arc<Mutex<InnerConnection>>,
}
impl ServerRequestId {
    pub fn mark_complete(self) {
        core::mem::forget(self);
    }
}
impl Drop for ServerRequestId {
    fn drop(&mut self) {
        let ServerRequestId {id, con} = self;
        let con = con.clone();
        let id = *id;
        let _ = tokio::spawn(async move {
            con.lock().await.abort_req(id).await
        });
    }
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
pub(crate) struct PreparedConnection((OwnedSemaphorePermit, tokio::sync::OwnedMutexGuard<InnerConnection>));
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
    ) -> Result<Connection, IoError> {
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
    ) -> Result<Connection, IoError> {
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
        B::Error: Display,
        I: IntoIterator<Item = (P1, P2)>,
        P1: Buf,
        P2: Buf,
    {
        let con_slot = self.prep_connection().await?;
        self.send_request(req, dyn_headers, con_slot).await
    }
    /// get the connection in a state where it can start a new request
    pub(crate) async fn prep_connection(&self)
        -> Result<PreparedConnection, IoError>
    {
        info!("new request pending");
        let _permit = self
            .sem
            .clone()
            .acquire_owned()
            .await
            .map_err(|_e| IoError::new(ErrorKind::WouldBlock, ""))?;

        info!("wait for lock");
        let mut mut_inner = self.inner.clone().lock_owned().await;

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
        Ok(PreparedConnection((_permit, mut_inner)))
    }
    /// start a new request on a connection that is ready
    pub(crate) async fn send_request<B, I, P1, P2>(
        &self,
        req: Request<B>,
        dyn_headers: I,
        con_slot: PreparedConnection,
    ) -> Result<Response<impl Body<Data = Bytes, Error = IoError>>, IoError>
    where
        B: Body + Unpin,
        B::Error: Display,
        I: IntoIterator<Item = (P1, P2)>,
        P1: Buf,
        P2: Buf,
    {
        let PreparedConnection((_permit, mut mut_inner)) = con_slot;
        let rid = {
            let rr = mut_inner.running_requests.vacant_entry();
            let rid = rr.key() as u16 + 1;
            let pending = FCGIRequest {
                buf: BufList::new(),
                waker: None,
                ended: false,
                _permit,
               
            };
            rr.insert(pending);
            rid
        };
        info!("started req #{}", rid);
        //entry.insert(meta);

        let br = FCGIType::BeginRequest {
            request_id: rid,
            role: fastcgi::FastCGIRole::Responder,
            flags: fastcgi::BeginRequestBody::KEEP_CONN,
        };
        mut_inner.io.encode(br).await?;
        //cancel the request if this Fut or the returned body is dropped before it is read completely
        let transaction = ServerRequestId { id: rid, con: self.inner.clone() };
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
            if let HeaderMultilineStrategy::ReturnError = self.header_nl {//http::HeaderValue does not allow this anyway
                if value.as_ref().contains(&b'\n') {
                    drop(kvw); //stop mid stream
                    //abort request by dropping transaction
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
                tokio::try_join!(self.send_body(rid, len, body), self.create_response(transaction))?;
            Ok(res)
        } else {
            //send end of STDIN
            mut_inner
                .io
                .flush_data_chunk(Self::NULL, rid, fastcgi::RecordType::StdIn)
                .await?;
            drop(mut_inner); // close mutex before create_response
            self.create_response(transaction).await
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
        B::Error: Display
    {
        //stream as body comes in
        while let Some(chunk) = body.data().await {
            match chunk {
                Ok(data) => {
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
                },
                Err(e) => {
                    return Err(IoError::other(e.to_string()))
                }
            }
        }
        //CGI1.1 4.2 -> at least content-length data
        if len > 0 {
            //abort request by early return in try_join
            //-> dropping transaction
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
        transaction: ServerRequestId
    ) -> Result<Response<impl Body<Data = Bytes, Error = IoError>>, IoError> {
        let mut fcgibody = FCGIBody {
            con: Arc::clone(&self.inner),
            was_returned: false,
            transaction: ServerState::Running(transaction)
        };
        let mut rb = Response::builder();
        let mut rheaders = rb.headers_mut().unwrap();
        let mut status = StatusCode::OK;
        //read the headers
        let mut buf: Option<Bytes> = None;
        while let Some(rbuf) = fcgibody.data().await {
            let mut b = rbuf?;
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
                            mut_inner.running_requests[fcgibody.transaction.id() as usize -1]
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


impl Drop for FCGIRequest {
    fn drop(&mut self) {
        debug!("Req mplex id free");
    }
}


#[cfg(test)]
mod tests;
mod inner;
use inner::InnerConnection;
mod body;
use body::FCGIBody;