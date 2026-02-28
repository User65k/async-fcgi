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
        let mut buf = BytesMut::with_capacity(256);
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
        let mut buf = BytesMut::with_capacity(64);
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
        let mut buf = BytesMut::with_capacity(64);
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
        let mut buf = BytesMut::with_capacity(256);
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
        let mut buf = BytesMut::with_capacity(256);
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
#[test]
fn drop_or_fail_during_send_body() {
    struct IWillFail;
    impl Body for IWillFail {
        type Data = Bytes;
        type Error = IoError;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            Poll::Ready(Some(Err(IoError::other("oh boy"))))
        }
        fn size_hint(&self) -> SizeHint {
            let mut sh = SizeHint::default();
            sh.set_exact(42);
            sh
        }
    }
    init_log();
    // Create the runtime
    let rt = Builder::new_current_thread().enable_all().build().unwrap();
    async fn mock_app(app_listener: TcpListener) {
        let (mut app_socket, _) = app_listener.accept().await.unwrap();
        let mut buf = BytesMut::with_capacity(128);
        app_socket.read_buf(&mut buf).await.unwrap();
        trace!("app read {:?}", buf);
        //params end is followed by abort
        let to_php = b"\x01\x01\0\x01\0\x08\0\0\0\x01\x01\0\0\0\0\0\x01\x04\0\x01\04\x04\0\x0c\0QUERY_STRING\x0e\x04REQUEST_METHODPOST\x0e\x02CONTENT_LENGTH42\x01\x04\0\x01\x01\x04\0\x01\0\0\0\0\x01\x02\0\x01\0\0\0\0";
        assert_eq!(buf, Bytes::from(&to_php[..]));
    }

    async fn con() {
        let (app_listener, a) = local_socket_pair().await.unwrap();
        let m = tokio::spawn(mock_app(app_listener));

        let fcgi_con = Connection::connect(&a, 1).await.unwrap();
        trace!("new connection obj");
        let req = Request::post("/")
            .header("Content-Length", "42")
            .body(IWillFail)
            .unwrap();
        trace!("new req obj");
        let params: HashMap<Bytes, Bytes> = HashMap::new();
        let mut res = fcgi_con.forward(req, params).await;
        trace!("got res obj");
        let Err(res) = res else {
            assert_eq!(1,2);
            return;
        };
        assert_eq!(res.kind(), std::io::ErrorKind::Other);
        m.await.unwrap();
    }
    rt.block_on(con());
}
#[test]
fn drop_return_body() {//dont consume entire return body
    init_log();
    // Create the runtime
    let rt = Builder::new_current_thread().enable_all().build().unwrap();
    async fn mock_app(app_listener: TcpListener) {
        let (mut app_socket, _) = app_listener.accept().await.unwrap();
        let mut buf = BytesMut::with_capacity(128);
        app_socket.read_buf(&mut buf).await.unwrap();
        trace!("app read {:?}", buf);
        let to_php = b"\x01\x01\0\x01\0\x08\0\0\0\x01\x01\0\0\0\0\0\x01\x04\0\x01\0!\x07\0\x0c\0QUERY_STRING\x0e\x03REQUEST_METHODGET\x01\x04\0\x01\0!\x07\x01\x04\0\x01\0\0\0\0\x01\x05\0\x01\0\0\0\0";
        assert_eq!(buf, Bytes::from(&to_php[..]));
        trace!("app answers on get");
        let from_php = b"\x01\x06\0\x01\0\x1b\x05\0Status: 404 Not Found\r\n\r\n\r\n\x01\x06\0\x01\0";
        app_socket
            .write_buf(&mut Bytes::from(&from_php[..]))
            .await
            .unwrap();

        buf.clear();
        app_socket.read_buf(&mut buf).await.unwrap();
        trace!("app read {:?}", buf);
        let to_php2 = b"\x01\x02\0\x01\0\0\0\0";
        assert_eq!(buf, Bytes::from(&to_php2[..]));
    }

    async fn con() {
        let (app_listener, a) = local_socket_pair().await.unwrap();
        let m = tokio::spawn(mock_app(app_listener));

        let fcgi_con = Connection::connect(&a, 1).await.unwrap();
        trace!("new connection obj");
        let b = TestBod { l: VecDeque::new() };
        let req = Request::get("/")
            .body(b)
            .unwrap();
        trace!("new req obj");
        let params: HashMap<Bytes, Bytes> = HashMap::new();
        let mut res = fcgi_con.forward(req, params).await.expect("forward failed");
        trace!("got res obj");
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        
        //do not read the body
        drop(res);

        m.await.unwrap();
    }
    rt.block_on(con());
}
#[test]
fn drop_request_instantly() {
    init_log();
    // Create the runtime
    let rt = Builder::new_current_thread().enable_all().build().unwrap();
    async fn mock_app(app_listener: TcpListener) {
        let (mut app_socket, _) = app_listener.accept().await.unwrap();
        let mut buf = BytesMut::with_capacity(8096);
        app_socket.read_buf(&mut buf).await.unwrap();
        trace!("app read {:?}", buf);
        let to_php = b"\x01\x01\0\x01\0\x08\0\0\0\x01\x01\0\0\0\0\0";
        assert_eq!(buf[..16], Bytes::from(&to_php[..]));

        if buf.ends_with(b"\x01\x02\0\x01\0\0\0\0") {
            //all good
        }else{
            buf.clear();
            app_socket.read_buf(&mut buf).await.unwrap();
            trace!("app read {:?}", buf);
            let to_php2 = b"\x01\x02\0\x01\0\0\0\0";
            assert_eq!(buf, Bytes::from(&to_php2[..]));
        }
    }

    async fn con() {
        let (app_listener, a) = local_socket_pair().await.unwrap();

        let m = tokio::spawn(mock_app(app_listener));

        let fcgi_con = Connection::connect(&a, 1).await.unwrap();
        trace!("new connection obj");
        let b = TestBod { l: VecDeque::new() };
        let req = Request::get("/")
            .body(b)
            .unwrap();
        trace!("new req obj");
        let params: HashMap<Bytes, Bytes> = HashMap::new();
        //let params = std::iter::repeat_n((&b"aaaaaaaa"[..],&b"bbbbbbbbbbbbbbbbbbb"[..]), 400);

        let r = PollOnce(fcgi_con.forward(req, params)).await;
        assert!(r.is_none());

        m.await.unwrap();
    }
    struct PollOnce<F: Future>(F);
    impl<F: Future> Future for PollOnce<F> {
        type Output = Option<F::Output>;
    
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            match unsafe{Pin::new_unchecked(&mut self.get_unchecked_mut().0)}.poll(cx) {
                Poll::Ready(r) => Poll::Ready(Some(r)),
                Poll::Pending => Poll::Ready(None),
            }
        }
    }
    rt.block_on(con());
}