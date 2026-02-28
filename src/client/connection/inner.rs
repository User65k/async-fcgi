use super::{FCGIRequest};
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

/// Shared object to read from a `Connection`
///
/// Manages all requests on it and distributes data to them
pub struct InnerConnection {
    pub io: FCGIWriter<BufReader<Stream>>,
    ///all requests with pending responses
    pub running_requests: Slab<FCGIRequest>,
    pub fcgi_parser: fastcgi::RecordReader,
}

/*impl Future for InnerConnection {
    type Output = Option<Result<(), IoError>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<(), IoError>>> {
        self.poll_resp(cx)
    }
}*/

impl InnerConnection {
    ///returns true if the connection is still alive
    pub fn check_alive(&mut self) -> CheckAlive<'_> {
        CheckAlive(self)
    }
    pub async fn abort_req(&mut self, request_id: u16) -> Result<(), IoError> {
        self.io.encode(FCGIType::AbortRequest { request_id }).await
    }
    /// drive this connection
    /// Read, parse and distribute data from the socket.
    /// return None if the connection was closed
    pub fn poll_resp(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Result<(), IoError>>> {
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
    
    /// Something happened. We are done with everything
    pub fn notify_everyone(&mut self) {
        for (rid, mpxs) in self.running_requests.iter_mut() {
            if let Some(waker) = mpxs.waker.take() {
                waker.wake()
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

pub struct CheckAlive<'a>(&'a mut InnerConnection);

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