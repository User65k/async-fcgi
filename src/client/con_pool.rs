/*! FCGI Application serving for [Hyper 0.13](https://github.com/hyperium/hyper).
  
  
  
  This Module consists of the following Objects:
   
 * [`ConPool`]: supports FCGI_MAX_CONNS Connections
 * [`Connection`]: handles up to FCGI_MAX_REQS concurrent Requests
 
[`ConPool`]: ./struct.ConPool.html
[`Connection`]: ../connection/index.html
*/

use http::{Request, Response};
use log::info;
use crate::client::connection::Connection;
use std::error::Error;
use std::io::Error as IoError;
use bytes::{Bytes, BytesMut, Buf, BufMut};
use http_body::Body as HttpBody;
use std::iter::IntoIterator;
use std::fmt;
use crate::stream::{FCGIAddr, Stream};
use crate::fastcgi::{NVBodyList, Record, Body, MAX_CONNS, MAX_REQS, MPXS_CONNS};
use crate::bufvec::BufList;
use std::iter::FromIterator;
use std::collections::HashMap;
use tokio::prelude::*;
//use tokio::io::{AsyncRead, AsyncWrite};
//use tokio::prelude::*;

/// manage a pool of [Connection](../connection/struct.Connection.html)s to an Server.
pub struct ConPool
{/*
    sock_addr: String,*/
    max_cons: u8,  /// The maximum number of concurrent transport connections this application will accept
    max_req_per_con: u16, /// The maximum number of concurrent requests this application will accept
    con_pool: Connection
}
impl ConPool
{
    pub async fn new(sock_addr: &FCGIAddr) -> Result<ConPool, Box<dyn Error>> {
        // query VALUES from connection
        let mut stream = Stream::connect(sock_addr).await?;

        let mut query = HashMap::new();
        query.insert(
            Bytes::from(MAX_CONNS),
            Bytes::new(),
        );
        query.insert(
            Bytes::from(MAX_REQS),
            Bytes::new(),
        );
        query.insert(
            Bytes::from(MPXS_CONNS),
            Bytes::new(),
        );
        let vals = NVBodyList::from_iter(query);
        let mut wbuf = BufList::new();
        vals.append_records(Record::GET_VALUES, Record::MGMT_REQUEST_ID, &mut wbuf);
        let mut max_cons = 1;
        let mut max_req_per_con = 1;
        for rec in send_and_receive(&mut stream, &mut wbuf).await? {
            if let Body::GetValuesResult(kvs) = rec.body {
                for kv in kvs {
                    match kv.name_data.bytes() {
                        MAX_CONNS => {
                            if let Some(v) = parse_int::<u8>(kv.value_data) {
                                max_cons = v;
                            }
                        },
                        MAX_REQS => {
                            if let Some(v) = parse_int::<u16>(kv.value_data) {
                                max_req_per_con = v;
                            }
                        },
                        MPXS_CONNS => {
                            if kv.value_data == "0" {
                                max_req_per_con = 1;
                                break;
                            }
                        },
                        _ => {}
                    };
                }
            }
        }
        info!("App supports {} connections with {} requests", max_cons, max_req_per_con);
        let c = Connection::connect(&sock_addr, max_req_per_con).await?;

        Ok(ConPool {/*
            sock_addr,*/
            max_cons,
            max_req_per_con,
            con_pool: c
        })
    }
    /// Forwards an HTTP request to a FGCI Application
    ///
    /// Calls the corresponding function of an available [Connection](../connection/struct.Connection.html#method.forward).
    pub async fn forward<B, I>(&self, req: Request<B>, dyn_headers: I)
                            -> Result<Response<impl HttpBody<Data = Bytes,Error = IoError>>, IoError>
    where   B: HttpBody+Unpin,
            I: IntoIterator<Item = (Bytes, Bytes)>
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
    if let Ok(s) = std::str::from_utf8(bytes.bytes()) {
        if let Ok(i) = s.parse() {
            return Some(i);
        }
    }
    return None;
}

/// Note: only use this if there are no requests pending
async fn send_and_receive(stream: &mut Stream, wbuf: &mut BufList<Bytes>) -> Result<Vec<Record>, IoError> {
    stream.write_buf(wbuf).await?;
    let mut recs = Vec::new();

    let mut rbuf = BytesMut::with_capacity(4096);
    loop {
        stream.read_buf(&mut rbuf).await?;
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

/*
#[test]
fn it_works() {
    extern crate pretty_env_logger;
    pretty_env_logger::init();
    use tokio::runtime::Runtime;
    // Create the runtime
    let mut rt = Runtime::new().unwrap();
    async fn app() {
        let a = ConPool::new("127.0.0.1:9000").await;
    
    }
    rt.block_on(app());
}*/


/*
use std::process::{Stdio};
use tokio::process::Command;

let command = Command::new("ls").stdin(Stdio::null())
        .arg("-l")
        .arg("-a").env_clear()
        .env("FCGI_WEB_SERVER_ADDRS", "199.170.183.28,199.170.183.71");




*/