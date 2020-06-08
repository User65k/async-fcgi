/*! FCGI Application serving for [Hyper 0.13](https://github.com/hyperium/hyper).
  
  
  
  This Module consists of the following Objects:
   
 * [`ConPool`]: supports FCGI_MAX_CONNS Connections
 * [`Connection`]: handles up to FCGI_MAX_REQS concurrent Requests
 
[`ConPool`]: ./struct.ConPool.html
[`Connection`]: ../connection/index.html
*/

use http::{Request, Response};
//use log::{trace, info};
use crate::client::connection::Connection;
use std::error::Error;
use std::io::Error as IoError;
use bytes::Bytes;
use http_body::Body;
use std::iter::IntoIterator;
use std::fmt;
use crate::stream::FCGIAddr;
//use tokio::io::{AsyncRead, AsyncWrite};
//use tokio::prelude::*;

/// manage a pool of [Connection](../connection/struct.Connection.html)s to an Server.
pub struct ConPool
{/*
    sock_addr: String,
    max_cons: u8,
    max_req_per_con: u8,*/
    con_pool: Connection
}
impl ConPool
{
    pub async fn new(sock_addr: &FCGIAddr) -> Result<ConPool, Box<dyn Error>> {
        let max_req_per_con = 1;
        let c = Connection::connect(&sock_addr, max_req_per_con).await?;
        Ok(ConPool {/*
            sock_addr,
            max_cons: 1,
            max_req_per_con,*/
            con_pool: c
        })
    }
    /// Forwards an HTTP request to a FGCI Application
    ///
    /// Calls the corresponding function of an available [Connection](../connection/struct.Connection.html#method.forward).
    pub async fn forward<B, I>(&self, req: Request<B>, dyn_headers: I)
                            -> Result<Response<impl Body<Data = Bytes,Error = IoError>>, IoError>
    where   B: Body+Unpin,
            I: IntoIterator<Item = (Bytes, Bytes)>
    {
        self.con_pool.forward(req, dyn_headers).await
    }
}
impl fmt::Debug for ConPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConPool")
         .field("Connections", &"[TCP]")
         .field("max_cons", &1)
         .field("max_req_per_con", &1)
         .finish()
    }
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