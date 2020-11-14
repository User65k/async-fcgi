/*! A single connection (TCP or Unix) to an FCGI application.

Multiple Requests can be multiplexed on it.

# Example
```
use std::collections::HashMap;
use http::{Request, StatusCode};
use http_body::Body;
use tokio::net::TcpStream;
use bytes::Bytes;
use async_fcgi::client::connection::Connection;

#[tokio::main]
async fn main() {
    let mut fcgi_con = Connection::connect(&"127.0.0.1:59000".parse().unwrap(), 1).await.unwrap();
    let req = Request::get("/test?lol=1").header("Accept", "text/html").body(()).unwrap();
    let mut params = HashMap::new();
    params.insert(
        Bytes::from(&b"SCRIPT_FILENAME"[..]),
        Bytes::from(&b"/home/daniel/Public/test.php"[..]),
    );
    let mut res = fcgi_con.forward(req,params).await.expect("forward failed");
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(res.headers().get("X-Powered-By").expect("powered by header missing"), "PHP/7.3.16");
    let read1 = res.data().await;
    assert!(read1.is_some());
}
```
*/

use tokio::io::AsyncRead;
use tokio::prelude::*;
use std::marker::Unpin;
use bytes::{BytesMut, Bytes, BufMut, Buf};
use http::{Request, Response, StatusCode, header::HeaderMap, header::CONTENT_LENGTH, header::AUTHORIZATION, header::CONTENT_TYPE};
use http_body::Body;
use slab::Slab;

use log::{trace, info, error, debug, warn, log_enabled, Level::Trace};

use std::pin::Pin;
use std::task::{Context, Poll};
use std::io::{Error as IoError, ErrorKind};
use std::error::Error;
use std::task::Waker;
use tokio::sync::{Mutex, Semaphore, OwnedSemaphorePermit};
use std::sync::Arc;
use std::future::Future;
use std::iter::{Extend, IntoIterator};
use std::ops::Drop;

use crate::stream::{FCGIAddr, Stream};
use crate::bufvec::BufList;
use crate::fastcgi;
use crate::httpparse::{parse, ParseResult};

/// [http_body](https://docs.rs/http-body/0.3.1/http_body/trait.Body.html) type for FCGI.
/// 
/// This is the STDOUT of an FastCGI Application.
/// STDERR is logged using [log::error](https://doc.rust-lang.org/1.1.0/log/macro.error!.html)
struct FCGIBody
{
    con: Arc<Mutex<InnerConnection>>,    //where to read
    rid: u16,                               //my id
    done: bool,                             //no more data
    was_returned: bool                      //request is no longer polled by us
}
/// Request stream
/// 
/// Manages one request from
/// `FCGI_BEGIN_REQUEST` to `FCGI_END_REQUEST`
/// 
struct FCGIRequest
{
    buf: BufList<Bytes>,                    //stdout read by some task
    waker: Option<Waker>,                   //wake me if needed
    done: bool,                             //fin reading
    _permit: OwnedSemaphorePermit           //block a multiplex slot
}
/// Shared object to read from a `Connection`
/// 
/// Manages all requests on it and distributes data to them
struct InnerConnection
{
    io: Stream,
    running_requests: Slab<FCGIRequest>,
    con_buf: Option<Bytes>,                     //half records
    fcgi_parser: fastcgi::RecordReader
}
/// Single transport connection to a FCGI application
/// 
/// Can multiplex `max_req_per_con` simultaneous request streams
pub struct Connection
{
    inner: Arc<Mutex<InnerConnection>>,
    sem: Arc<Semaphore>
}
impl Connection
{
    /// Connect to a peer
    pub async fn connect(addr: &FCGIAddr, max_req_per_con: u16) -> Result<Connection, Box<dyn Error>> {
        Ok(Connection {
            inner: Arc::new(Mutex::new(InnerConnection{
                io: Stream::connect(addr).await?,
                running_requests: Slab::with_capacity(max_req_per_con as  usize),
                con_buf: None,
                fcgi_parser: fastcgi::RecordReader::new()
            })),
            sem: Arc::new(Semaphore::new(max_req_per_con as  usize))
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

    /// Forwards an HTTP request to a FGCI Application
    ///
    /// Fills QUERY_STRING, REQUEST_METHOD, CONTENT_TYPE and CONTENT_LENGTH
    /// from the corresponding values in the Request.
    /// Headers in the Request will be added with the "HTTP_" prefix. (CGI/1.1 4.1.18)
    ///
    /// Additional Params might be expected from the application (at least the url path):
    /// - SCRIPT_NAME       must CGI/1.1  4.1.13, everybody cares
    /// - SERVER_NAME       must CGI/1.1  4.1.14, flup cares for this
    /// - SERVER_PORT       must CGI/1.1  4.1.15, flup cares for this
    /// - SERVER_PROTOCOL   must CGI/1.1  4.1.16, flup cares for this
    /// - SERVER_SOFTWARE   must CGI/1.1  4.1.17
    /// - REMOTE_ADDR       must CGI/1.1  4.1.8
    /// - GATEWAY_INTERFACE must CGI/1.1  4.1.4
    /// - REMOTE_HOST       should CGI/1.1  4.1.9
    /// - REMOTE_IDENT      may CGI/1.1  4.1.10
    /// - REMOTE_USER       opt CGI/1.1
    /// - AUTH_TYPE         opt CGI/1.1
    /// - PATH_INFO         opt CGI/1.1   4.1.5 extra-path
    /// - PATH_TRANSLATED   opt CGI/1.1   4.1.6
    /// - SCRIPT_FILENAME   PHP cares for this
    /// - REMOTE_PORT       common
    /// - SERVER_ADDR       common
    /// - REQUEST_URI       common
    /// - DOCUMENT_URI      common
    /// - DOCUMENT_ROOT     common
    pub async fn forward<B, I>(&self,
                        req: Request<B>,
                        dyn_headers: I)
                    -> Result<Response<impl Body<Data = Bytes,Error = IoError>>, IoError>
    where   B: Body+Unpin,
            I: IntoIterator<Item = (Bytes, Bytes)>
    {
        let rid: u16;
        {
            info!("new request pending");
            let _permit = self.sem.clone().acquire_owned().await;
            let meta = FCGIRequest {
                buf: BufList::new(),
                waker: None,
                done: false,
                _permit
            };
            
            info!("wait for lock");
            let mut mut_inner = self.inner.lock().await;

            if mut_inner.check_alive().await?==false {
                // we need to connect again
                let addr = mut_inner.io.peer_addr()?;
                mut_inner.io.shutdown().await?;
                mut_inner.notify_everyone();
                mut_inner.io = Stream::connect(&addr).await?;
                info!("reconnected");
            }

            rid = (mut_inner.running_requests.insert(meta)+1) as u16;
            info!("started req #{}", rid);
            //entry.insert(meta);


            //Prepare the CGI headers
            let mut wbuf = BufList::new();
            fastcgi::BeginRequestBody::new(fastcgi::BeginRequestBody::RESPONDER,
                                           fastcgi::BeginRequestBody::KEEP_CONN,
                                           rid).append(&mut wbuf);
            let mut nv = fastcgi::NVBodyList::new();
            nv.extend(dyn_headers);

            let query = match req.uri().query() {
                Some(query) => BytesMut::from(query.as_bytes()).freeze(),
                None => Bytes::new()
            };
            nv.add(fastcgi::NameValuePair::new(Bytes::from(&b"QUERY_STRING"[..]),query)); //must CGI1.1 4.1.7
        
            let method = BytesMut::from(req.method().as_str().as_bytes()).freeze();
            nv.add(fastcgi::NameValuePair::new(Bytes::from(&b"REQUEST_METHOD"[..]),method)); //must CGI1.1 4.1.12
            
            if let Some(value) = req.headers().get(CONTENT_TYPE) { //if client CGI1.1 4.1.3.
                nv.add(fastcgi::NameValuePair::new(
                        BytesMut::from(CONTENT_TYPE.as_str().as_bytes()).freeze(),
                        BytesMut::from(value.as_bytes()).freeze()
                    ));
            }
            let len = req.headers().get(CONTENT_LENGTH); //if body CGI1.1 4.1.2.
            if let Some(value) = len {
                nv.add(fastcgi::NameValuePair::new(
                        BytesMut::from(CONTENT_LENGTH.as_str().as_bytes()).freeze(),
                        BytesMut::from(value.as_bytes()).freeze()
                    ));
            }
            let skip = [AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE];
            //append all HTTP headers
            for (key, value) in req.headers().iter() {
                if skip.iter().find(|x| x == key).is_some() { //CGI1.1 4.1.18.
                    continue;
                }
                let mut k = BytesMut::with_capacity(key.as_str().len()+5);
                k.put(&b"HTTP_"[..]);
                k.put(key.as_str().as_bytes()); //FIXME copy
                let v = BytesMut::from(value.as_bytes()).freeze();
                let p = fastcgi::NameValuePair::new(k.freeze(),v);
                nv.add(p);
            }
            nv.append_records(fastcgi::Record::PARAMS, rid, &mut wbuf);
            fastcgi::NVBody::new().to_record(fastcgi::Record::PARAMS, rid).append(&mut wbuf); //empty record to end PARAMS steam FCGI1.0
            //send all headers to the FCGI App
            mut_inner.io.write_buf(&mut wbuf).await?;
            trace!("sent header");
            //Note: Responses might arrive from this point on

            //send the body to the FCGI App
            if let Some(value) = len {
                //CGI1.1 4.2 -> at least content-length data
                let mut len: usize = if let Ok(vstr) = value.to_str() {
                    vstr.parse().unwrap_or(0)
                }else{
                    0
                };
                let mut body: B = req.into_body();
                while let Some(buf) = body.data().await {
                    if let Ok(mut b) = buf { //b: Buf
                        let mut b = b.to_bytes();
                        len = len.saturating_sub(b.len());
                        while b.remaining() > 0 {
                            fastcgi::STDINBody::new(rid, &mut b).append(&mut wbuf);
                        }
                        mut_inner.io.write_buf(&mut wbuf).await?;
                    }
                }
                
                if len > 0 {
                    //fix broken connection?
                    error!("body to short. abort");
                    fastcgi::Record::abort(rid).append(&mut wbuf);
                }
            }
            fastcgi::STDINBody::new(rid, &mut Bytes::new()).append(&mut wbuf); //empty record to end STDIN steam FCGI1.0
            mut_inner.io.write_buf(&mut wbuf).await?;
            debug!("sent req body");
        }
        //free mutex

        let mut fcgibody = FCGIBody
        {
            con: Arc::clone(&self.inner),
            rid: (rid-1),
            done: false,
            was_returned: false
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
                    let mut c = BytesMut::with_capacity(left.len()+b.len());
                    c.put(left);
                    c.put(b);
                    b = c.freeze();
                }
                match parse(b.clone(), &mut rheaders){
                    ParseResult::Ok(bodydata) => {
                        trace!("read body fragment: {:?}", &bodydata);
                        if bodydata.has_remaining() {
                            let mut mut_inner = self.inner.lock().await;
                            //was_returned prevents: request might already be done and gone
                            mut_inner.running_requests[fcgibody.rid as usize].buf.push(bodydata);
                        }

                        if let Some(stat) = rheaders.get("Status") { //CGI1.1
                            //info!("Status header: {:?}", stat);
                            if stat.len() >= 3 {
                                if let Ok(s) = StatusCode::from_bytes(&stat.as_bytes()[..3][..]) {
                                    status = s;
                                }
                            }
                        }
                        //Location header for local URIs (starting with "/") -> must be done in Webserver
                        break;
                    },
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
            }else{
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

impl Future for InnerConnection {
    type Output = Option<Result<(), IoError>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Option<Result<(), IoError>>>
    {
        self.poll_resp(cx)
    }
}
struct CheckAlive<'a>(&'a mut InnerConnection);

impl<'a> Future for CheckAlive<'a> {
    type Output = Result<bool, IoError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Result<bool, IoError>>
    {
        Poll::Ready(match Pin::new(&mut *self.0).poll_resp(cx) {
            Poll::Ready(None) => Ok(false),
            Poll::Ready(Some(Err(e))) => Err(e),
            _ => Ok(true)
        })
    }
}


impl InnerConnection
{
    ///returns true if the connection is still alive
    fn check_alive(&mut self) -> CheckAlive {
        CheckAlive(self)
    }
    /// drive this connection
    /// Read, parse and distribute data from the socket.
    /// return None if the connection was closed
    fn poll_resp(
        mut self: Pin<&mut Self>, 
        cx: &mut Context
    ) -> Poll<Option<Result<(), IoError>>>
    {
        let Self {
            ref mut io,
            ref mut running_requests,
            ref mut con_buf,
            ref mut fcgi_parser,
        } = *self;
        /*
        1. Read from Socket
        2. Parse all the Data and put it in the corresponding OutBuffer
        3. Notify those with new Data
        */

        let mut rbuf = BytesMut::with_capacity(4096);

        match Pin::new(io).poll_read_buf(cx, &mut rbuf) {
            Poll::Ready(Ok(0)) => {info!("connection closed");self.notify_everyone();Poll::Ready(None)},
            Poll::Ready(Ok(size)) => {
                let mut data = rbuf.freeze().slice(..size);
                if log_enabled!(Trace) {
                    let print = if data.len() > 50 {
                        format!("read conn data ({}) {:?}...{:?}", data.len(), data.slice(..21), data.slice(data.len()-21..))
                    }else{
                        format!("read conn data {:?}", data)
                    };
                    trace!("read conn data {}", print);
                }
                if let Some(left) = con_buf.take() {
                    //we have old data -> concat
                    let mut c = BytesMut::with_capacity(left.len()+data.len());
                    c.put(left);
                    c.put(data);
                    data = c.freeze();
                    trace!("data with leftover {:?}", data);
                }
                InnerConnection::parse_and_distribute(&mut data, running_requests, fcgi_parser);

                if data.remaining() > 0{
                    //some data is left unparsed - had to be a header fragment (unlikely)
                    *con_buf = Some(data);
                }
                Poll::Ready(Some(Ok(())))
            },
            Poll::Ready(Err(e)) => {error!("Err {}",e);self.notify_everyone();Poll::Ready(Some(Err(e)))},
            Poll::Pending => Poll::Pending,
        }
    }
}
impl Drop for FCGIBody {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        debug!("Dropping FCGIBody #{}!", self.rid);
        match self.con.try_lock() {
            Ok(mut mut_inner) => {
                let rid = self.rid as usize;
                if mut_inner.running_requests.contains(rid) {
                    mut_inner.running_requests.remove(rid);
                }
            },
            Err(e) => error!("{}",e),
        }
    }
}

impl Body for FCGIBody
{
    type Data = Bytes;
    type Error = IoError;
    /// Get a chunk of STDOUT data from this FCGI application request stream
    fn poll_data(
        mut self: Pin<&mut Self>, 
        cx: &mut Context
    ) -> Poll<Option<Result<Self::Data, Self::Error>>>
    {
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
            was_returned
        } = *self;
        
        if *done {
            debug!("body #{} is already done", rid);
            return Poll::Ready(None);
        }

        trace!("read resp body");
        let fut = con.lock();
        match Box::pin(fut).as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(mut mut_inner) => {  // mut_inner: InnerConnection<S>
                
                match Pin::new(&mut *mut_inner).poll_resp(cx) {
                    Poll::Ready(Some(Ok(_))) | Poll::Ready(None) | Poll::Pending => {
                        if !mut_inner.running_requests.contains(rid as usize) {
                            trace!("#{} not in slab", rid);
                            *done = true;
                            return Poll::Ready(None);
                        }
                        let mut slab = &mut mut_inner.running_requests[rid as usize];

                        if slab.buf.remaining() >= 1 {
                            trace!("body #{} has data and is {} closed", rid, slab.done);
                            let retdata = Poll::Ready(Some(Ok(slab.buf.oldest().unwrap())));
                            if was_returned && slab.done && slab.buf.remaining() < 1 {
                                //ret rid of this as fast as possible,
                                //it blocks us and clients might stop reading
                                trace!("next read on #{} will not have data -> release", rid);
                                mut_inner.running_requests.remove(rid as usize);
                                *done = true;
                            }
                            retdata
                        }else{
                            let req_done = slab.done;
                            if req_done {
                                debug!("body #{} is done", rid);
                                if was_returned {
                                    mut_inner.running_requests.remove(rid as usize);
                                    *done = true;
                                }else{
                                    warn!("#{} closed before handover", rid);
                                }
                                Poll::Ready(None)
                            }else{
                                trace!("body waits");
                                //store waker
                                slab.waker = Some(cx.waker().clone());
                                Poll::Pending
                            }
                        } 
                    },
                    //Poll::Pending => Poll::Pending,
                    //Poll::Ready(None) => Poll::Ready(None),
                    Poll::Ready(Some(Err(e))) => {error!("body #{} err {}", rid, e);Poll::Ready(Some(Err(e)))},
                }
            }
        }
    }    
    fn poll_trailers(
        self: Pin<&mut Self>, 
        _cx: &mut Context
    ) -> Poll<Result<Option<HeaderMap>, Self::Error>>
    {
        Poll::Ready(Ok(None))
    }
}

impl InnerConnection {
    fn notify_everyone(&mut self) {
        for (_, mpxs) in self.running_requests.iter_mut() {
            if let Some(waker) = mpxs.waker.take() {
                waker.wake()
            }
            mpxs.done = true;
        }
    }
    fn parse_and_distribute(data: &mut Bytes, running_requests: &mut Slab<FCGIRequest>, fcgi_parser: &mut fastcgi::RecordReader) -> Option<Bytes> {
        //trace!("parse {:?}", &data);
        while let Some(r) = fcgi_parser.read(data) {
                let (req_no, ovr) = r.get_request_id().overflowing_sub(1);
                if ovr {
                    //req id 0
                    error!("got mgmt record");
                    continue;
                }
                debug!("record for #{}", req_no);
                if let Some(mpxs) = running_requests.get_mut(req_no as usize) {
                    match r.body {
                        fastcgi::Body::StdOut(s) => {
                            if log_enabled!(Trace) {
                                let print = if s.len() > 50 {
                                    format!("FCGI stdout: ({}) {:?}...{:?}", s.len(), s.slice(..21), s.slice(s.len()-21..))
                                }else{
                                    format!("FCGI stdout: {:?}", s)
                                };
                                trace!("FCGI stdout: {}", print);
                            }                            
                            if s.has_remaining() {
                                mpxs.buf.push(s);
                                if let Some(waker) = mpxs.waker.take() {
                                    waker.wake();
                                }
                            }
                        },
                        fastcgi::Body::StdErr(s) => {error!("FCGI #{} Err: {:?}", req_no+1, s);}
                        fastcgi::Body::EndRequest(status) => {
                            match status.protocol_status {
                                fastcgi::EndRequestBody::REQUEST_COMPLETE => info!("Req #{} ended with {}", req_no, status.app_status),
                                //CANT_MPX_CONN => ,
                                //TODO handle OVERLOADED
                                _ => error!("Req #{} ended with fcgi error {}", req_no, status.protocol_status)
                            };
                            mpxs.done = true;
                            if let Some(waker) = mpxs.waker.take() {
                                waker.wake()
                            }
                        },
                        _ => {warn!("type?");}
                    }
                }else{
                    debug!("not a pending red ID");
                }
        };
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
        //extern crate pretty_env_logger;
        //pretty_env_logger::init();
    use tokio::runtime::Runtime;
    use tokio::net::TcpListener;
    use std::collections::{VecDeque,HashMap};
    use std::net::SocketAddr;

    struct TestBod{
        l: VecDeque<Bytes>
    }
    impl Body for TestBod{

        type Data = Bytes;
        type Error = IoError;
        fn poll_data(
            mut self: Pin<&mut Self>, 
            _cx: &mut Context
        ) -> Poll<Option<Result<Self::Data, Self::Error>>>
        {
            let Self {
                ref mut l
            } = *self;
            match l.pop_front()
            {
                None => Poll::Ready(None),
                Some(i) => Poll::Ready(Some(Ok(i)))
            }
            
        }    
        fn poll_trailers(
            self: Pin<&mut Self>, 
            _cx: &mut Context
        ) -> Poll<Result<Option<HeaderMap>, Self::Error>>
        {
            Poll::Ready(Ok(None))
        }
    }

    #[test]
    fn simple_get() {
        // Create the runtime
        let mut rt = Runtime::new().unwrap();
        async fn mock_app(mut app_listener: TcpListener) {
            let (mut app_socket, _) = app_listener.accept().await.unwrap();
            let mut buf = BytesMut::with_capacity(4096);
            app_socket.read_buf(&mut buf).await.unwrap();
            trace!("app read {:?}", buf);
            let to_php = b"\x01\x01\0\x01\0\x08\0\0\0\x01\x01\0\0\0\0\0\x01\x04\0\x01\0i\x07\0\x0f\x1cSCRIPT_FILENAME/home/daniel/Public/test.php\x0c\x05QUERY_STRINGlol=1\x0e\x03REQUEST_METHODGET\x0b\tHTTP_accepttext/html\x01\x04\0\x01\0i\x07\x01\x04\0\x01\0\0\0\0\x01\x05\0\x01\0\0\0\0";
            assert_eq!(buf.to_bytes(), &to_php[..]);
            trace!("app answers on get");
            let from_php = b"\x01\x07\0\x01\0W\x01\0PHP Fatal error:  Kann nicht durch 0 teilen in /home/daniel/Public/test.php on line 14\n\0\x01\x06\0\x01\x01\xf7\x01\0Status: 404 Not Found\r\nX-Powered-By: PHP/7.3.16\r\nX-Authenticate: NTLM\r\nContent-type: text/html; charset=UTF-8\r\n\r\n<html><body>\npub\n<pre>Array\n(\n)\nArray\n(\n    [lol] => 1\n)\nArray\n(\n    [lol] => 1\n)\nArray\n(\n    [HTTP_accept] => text/html\n    [REQUEST_METHOD] => GET\n    [QUERY_STRING] => lol=1\n    [SCRIPT_NAME] => /test\n    [SCRIPT_FILENAME] => /home/daniel/Public/test.php\n    [FCGI_ROLE] => RESPONDER\n    [PHP_SELF] => /test\n    [REQUEST_TIME_FLOAT] => 1587740954.2741\n    [REQUEST_TIME] => 1587740954\n)\n\0\x01\x03\0\x01\0\x08\0\0\0\0\0\0\0\0\0\0";
            app_socket.write_buf(&mut Bytes::from(&from_php[..])).await.unwrap();
        }

        async fn con() {
            let a: SocketAddr = "127.0.0.1:59000".parse().unwrap();
            let app_listener = TcpListener::bind(a).await.unwrap();
            tokio::spawn(mock_app(app_listener));

            let fcgi_con = Connection::connect(&"127.0.0.1:59000".parse().unwrap(), 1).await.unwrap();
            trace!("new connection obj");
            let b = TestBod{
                l: VecDeque::new()
            };
            let req = Request::get("/test?lol=1").header("Accept", "text/html").body(b).unwrap();
            trace!("new req obj");
            let mut params = HashMap::new();
            params.insert(
                Bytes::from(&b"SCRIPT_FILENAME"[..]),
                Bytes::from(&b"/home/daniel/Public/test.php"[..]),
            );
            let mut res = fcgi_con.forward(req,params).await.expect("forward failed");
            trace!("got res obj");
            assert_eq!(res.status(), StatusCode::NOT_FOUND);
            assert_eq!(res.headers().get("X-Powered-By").expect("powered by header missing"), "PHP/7.3.16");
            let read1 = res.data().await;
            assert!(read1.is_some());
            let read1 = read1.unwrap();
            assert!(read1.is_ok());
            if let Ok(mut d) = read1 {
                let body = b"<html><body>\npub\n<pre>Array\n(\n)\nArray\n(\n    [lol] => 1\n)\nArray\n(\n    [lol] => 1\n)\nArray\n(\n    [HTTP_accept] => text/html\n    [REQUEST_METHOD] => GET\n    [QUERY_STRING] => lol=1\n    [SCRIPT_NAME] => /test\n    [SCRIPT_FILENAME] => /home/daniel/Public/test.php\n    [FCGI_ROLE] => RESPONDER\n    [PHP_SELF] => /test\n    [REQUEST_TIME_FLOAT] => 1587740954.2741\n    [REQUEST_TIME] => 1587740954\n)\n";
                assert_eq!(d.to_bytes(), &body[..] );
            }
            let read2 = res.data().await;
            assert!(read2.is_none());
        }
        rt.block_on(con());
    }
    #[test]
    fn app_answer_split_mid_record() { //flup did this once
        extern crate pretty_env_logger;
        pretty_env_logger::init();
        // Create the runtime
        let mut rt = Runtime::new().unwrap();
        async fn mock_app(mut app_listener: TcpListener) {
            let (mut app_socket, _) = app_listener.accept().await.unwrap();
            let mut buf = BytesMut::with_capacity(4096);
            app_socket.read_buf(&mut buf).await.unwrap();
            trace!("app read {:?}", buf);
            trace!("app answers on get");
            let from_flup = b"\x01\x06\0\x01\0@\0\0Status: 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 13\r\n\r\n\x01\x06\0\x01\0\r\x03\0Hello World!\n";
            app_socket.write_buf(&mut Bytes::from(&from_flup[..])).await.unwrap();
        }

        async fn con() {            
            let a: SocketAddr = "127.0.0.1:59001".parse().unwrap();
            let app_listener = TcpListener::bind(a).await.unwrap();
            tokio::spawn(mock_app(app_listener));

            let fcgi_con = Connection::connect(&"127.0.0.1:59001".parse().unwrap(), 1).await.unwrap();
            trace!("new connection obj");
            let b = TestBod{
                l: VecDeque::new()
            };
            let req = Request::get("/").body(b).unwrap();
            trace!("new req obj");
            let params = HashMap::new();
            let mut res = fcgi_con.forward(req,params).await.expect("forward failed");
            trace!("got res obj");
            let read1 = res.data().await;
            assert!(read1.is_some());
            let read1 = read1.unwrap();
            assert!(read1.is_ok());
            if let Ok(mut d) = read1 {
                let body = b"Hello World!\n";
                assert_eq!(d.to_bytes(), &body[..] );
            }
        }
        rt.block_on(con());
    }

    #[test]
    fn app_http_headers_split() {
        // Create the runtime
        let mut rt = Runtime::new().unwrap();
        async fn mock_app(mut app_listener: TcpListener) {
            let (mut app_socket, _) = app_listener.accept().await.unwrap();
            let mut buf = BytesMut::with_capacity(4096);
            app_socket.read_buf(&mut buf).await.unwrap();
            trace!("app read {:?}", buf);
            trace!("app answers on get");
            let from_flup = b"\x01\x06\0\x01\0\x1e\0\0Status: 200 OK\r\nContent-Type: ";
            app_socket.write_buf(&mut Bytes::from(&from_flup[..])).await.unwrap();
            let from_flup = b"\x01\x06\0\x01\0\"\0\0text/plain\r\nContent-Length: 13\r\n\r\n\x01\x03\0\x01\0\x08\0\0\0\0\0\0\0\0\0\0";
            app_socket.write_buf(&mut Bytes::from(&from_flup[..])).await.unwrap();
        }

        async fn con() {
            let a: SocketAddr = "127.0.0.1:59002".parse().unwrap();
            let app_listener = TcpListener::bind(a).await.unwrap();
            tokio::spawn(mock_app(app_listener));

            let fcgi_con = Connection::connect(&"127.0.0.1:59002".parse().unwrap(), 1).await.unwrap();
            trace!("new connection obj");
            let b = TestBod{
                l: VecDeque::new()
            };
            let req = Request::get("/").body(b).unwrap();
            trace!("new req obj");
            let params = HashMap::new();
            let mut res = fcgi_con.forward(req,params).await.expect("forward failed");
            trace!("got res obj");
            assert_eq!(res.status(), StatusCode::OK);
            assert_eq!(res.headers().get("Content-Length").expect("len header missing"), "13");
            assert_eq!(res.headers().get("Content-Type").expect("type header missing"), "text/plain");

            let read1 = res.data().await;
            assert!(read1.is_none());
        }
        rt.block_on(con());
    }
}