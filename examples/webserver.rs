//! Simple HTTP Server that forwards everything to an FCGI App on 127.0.0.1:9000.
//! Such an App could be flup/python:
//! ```
//! def myapp(environ, start_response):
//!     assert environ['REQUEST_METHOD'] == 'GET'
//!     assert environ['HTTP_host'] == '127.0.0.1:1337'
//!     start_response('200 OK', [('Content-Type', 'text/plain')])
//!     return ['Hello World!\n']
//! 
//! if __name__ == '__main__':
//!     from flup.server.fcgi import WSGIServer
//!     WSGIServer(myapp, bindAddress=('127.0.0.1',9000)).run()
//! ```

#![deny(warnings)]
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Server};
use async_fcgi::client::con_pool::ConPool;
use bytes::{BytesMut, Bytes};
use std::sync::Arc;
use tokio::sync::Mutex;
use std::io::Error as IoError;
use http::Response;
use http_body::Body as HTTPBody;
use std::collections::HashMap;

async fn fwd_to_fcgi(fcgi_app: Arc<Mutex<ConPool>>, req: Request<Body>) -> Result<Response<impl HTTPBody<Data = Bytes,Error = IoError>>, IoError> {
    let fcg = fcgi_app.lock().await;
    let mut params = HashMap::new();
    params.insert(
        Bytes::from(&b"SCRIPT_NAME"[..]),
        BytesMut::from(req.uri().path().as_bytes()).freeze(),
    );
    params.insert(
        Bytes::from(&b"SERVER_NAME"[..]),
        Bytes::from(&b"Awesome Server 1.0"[..]),
    );
    params.insert(
        Bytes::from(&b"SERVER_PORT"[..]),
        Bytes::from(&b"80"[..]),
    );
    params.insert(
        Bytes::from(&b"SERVER_PROTOCOL"[..]),
        Bytes::from(&b"HTTP"[..]),
    );
    fcg.forward(req, params).await
}

#[tokio::main]
async fn main() {
    pretty_env_logger::init();
    
    match ConPool::new(&"127.0.0.1:9000".parse().unwrap()).await {
        Ok(fcgi_app) => {
            let fcgi_link = Arc::new(Mutex::new(fcgi_app));
            let make_service = make_service_fn(move |_|{
                let fcg = fcgi_link.clone();
                async move {
                Ok::<_, hyper::Error>(service_fn(move |req: Request<Body>| fwd_to_fcgi(fcg.clone(), req) ))
            }});

            let in_addr = ([127, 0, 0, 1], 1337).into();
            let server = Server::bind(&in_addr).serve(make_service);
            println!("Listening on http://{}", in_addr);

            if let Err(e) = server.await {
                eprintln!("server error: {}", e);
            }
        },
        Err(e) => {
            eprintln!("FCGI error: {}", e);
        }
    }
}