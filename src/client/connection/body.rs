use super::{InnerConnection, ServerRequestId};
use bytes::{Buf, Bytes};
use http_body::{Body, Frame};

use log::{debug, trace, warn};

use std::{
    future::Future,
    io::Error as IoError,
    ops::Drop,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::sync::Mutex;

/// [http_body](https://docs.rs/http-body/0.3.1/http_body/trait.Body.html) type for FCGI.
///
/// This is the STDOUT of a FastCGI Application.
/// STDERR is logged using [log::error](https://doc.rust-lang.org/1.1.0/log/macro.error!.html)
pub struct FCGIBody {
    ///where to read
    con: Arc<Mutex<InnerConnection>>,
    //request is no longer polled by forward
    pub was_returned: bool,
    transaction: ServerState,
}

impl Drop for FCGIBody {
    fn drop(&mut self) {
        if let ServerState::Done(_) = self.transaction {
            return;
        }
        let rid = self.transaction.id() - 1;
        debug!("Dropping FCGIBody #{}!", rid + 1);
        let con = self.con.clone();
        let _ = tokio::spawn(async move {
            let _req = con.lock().await.running_requests.remove(rid as usize);
        });
    }
}
/// state of the body
enum ServerState {
    /// the server closed STDOUT
    Done(u16),
    /// the server is still sending answers.
    /// We can abort
    Running(ServerRequestId),
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
            ServerState::Done(_) => {}
            ServerState::Running(server_request_id) => server_request_id.mark_complete(),
        };
    }
}
impl FCGIBody {
    pub fn new(con: Arc<Mutex<InnerConnection>>, transaction: ServerRequestId) -> FCGIBody {
        FCGIBody {
            con,
            was_returned: false,
            transaction: ServerState::Running(transaction),
        }
    }
    pub fn id(&self) -> u16 {
        self.transaction.id()
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
            was_returned,
            ref mut transaction,
        } = *self;
        let rid = transaction.id() - 1;

        if let ServerState::Done(_) = transaction {
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
                        transaction.mark_done();
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

                if slab.buf.has_remaining() {
                    trace!("body #{} has data and is {} closed", rid + 1, slab.ended);
                    let retdata = Poll::Ready(Some(Ok(Frame::data(slab.buf.oldest().unwrap()))));
                    if was_returned && slab.ended && !slab.buf.has_remaining() {
                        //ret rid of this as fast as possible,
                        //it blocks us and clients might stop reading
                        trace!("next read on #{} will not have data -> release", rid + 1);
                        mut_inner.running_requests.remove(rid as usize);
                        transaction.mark_done();
                    }
                    retdata
                } else {
                    //data buffer empty
                    let req_done = slab.ended;
                    if req_done {
                        debug!("body #{} is done", rid + 1);
                        if was_returned {
                            mut_inner.running_requests.remove(rid as usize);
                            transaction.mark_done();
                        } else {
                            warn!("#{} closed before handover", rid + 1);
                        }
                        Poll::Ready(None)
                    } else {
                        if let Poll::Ready(Some(Err(e))) = _con_stat {
                            return Poll::Ready(Some(Err(e)));
                        }
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
