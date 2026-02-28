
use super::*;
use crate::client::tests::{init_log, local_socket_pair, TestBod};
use http::StatusCode;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::iter::FromIterator;
use std::process::ExitStatus;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::runtime::Builder;

#[cfg(feature = "app_start")]
#[test]
fn start_app() {
    let rt = Builder::new_current_thread().enable_all().build().unwrap();
    async fn spawn() {
        let mut env = HashMap::new();
        env.insert("PATH", "/usr/bin");
        let a: Addr = "/tmp/jo".parse().unwrap();
        let s: ExitStatus = ConPool::prep_server("ls", &a)
            .await
            .expect("prep_server error")
            .args(&["-l", "-a"])
            .env_clear()
            .envs(env)
            .status()
            .await
            .expect("ls failed");
        assert!(s.success())
    }
    rt.block_on(spawn());
    std::fs::remove_file("/tmp/jo").unwrap();
}
#[test]
fn no_vals() {
    //extern crate pretty_env_logger;
    //pretty_env_logger::init();
    use tokio::net::TcpListener;
    // Create the runtime
    let rt = Builder::new_current_thread().enable_all().build().unwrap();
    async fn mock_app(app_listener: TcpListener) {
        let (mut app_socket, _) = app_listener.accept().await.unwrap();
        let mut buf = BytesMut::with_capacity(4096);
        info!("accepted");

        //app_socket.read_buf(&mut buf).await.unwrap();
        if let Err(e) = app_socket.read_buf(&mut buf).await {
            info!("{}", e);
            panic!("could not read");
        }

        let mut buf = buf.freeze();
        trace!("app read {:?}", buf);
        let rec = Record::read(&mut buf).unwrap(); //val stream
        assert_eq!(rec.get_request_id(), 0);
        let v = match rec.body {
            Body::GetValues(v) => v,
            _ => panic!("wrong body"),
        };
        let names = Vec::from_iter(v.drain());
        assert_eq!(names.len(), 3);

        let _ = Record::read(&mut buf).unwrap(); //val stream end

        assert!(!buf.has_remaining());

        trace!("app answers on get");
        let from_php =
            b"\x01\x0a\0\0\0!\x07\0\n\0MPXS_CONNS\x08\0MAX_REQS\t\0MAX_CONNS\0\0\0\0\0\0\0";
        app_socket
            .write_buf(&mut Bytes::from(&from_php[..]))
            .await
            .unwrap();
    }

    async fn con() {
        let (app_listener, a) = local_socket_pair().await.unwrap();
        info!("bound");
        let m = tokio::spawn(async move {
            let a = a.into();
            let cp = ConPool::new(&a).await.unwrap();
            assert_eq!(cp.max_cons, 1);
            assert_eq!(cp.max_req_per_con, 1);
        });
        mock_app(app_listener).await;
        m.await.unwrap();
    }
    rt.block_on(con());
}
#[test]
fn mplex() {
    init_log();
    use tokio::net::TcpListener;
    // Create the runtime
    let rt = Builder::new_current_thread().enable_all().build().unwrap();
    async fn mock_app_w2cons(app_listener: &TcpListener) {
        let (mut app_socket, _) = app_listener.accept().await.unwrap();
        let mut buf = BytesMut::with_capacity(4096);
        info!("accepted");

        //app_socket.read_buf(&mut buf).await.unwrap();
        if let Err(e) = app_socket.read_buf(&mut buf).await {
            info!("{}", e);
            panic!("could not read");
        }

        let mut buf = buf.freeze();
        trace!("app read {:?}", buf);
        let rec = Record::read(&mut buf).unwrap(); //val stream
        assert_eq!(rec.get_request_id(), 0);
        let v = match rec.body {
            Body::GetValues(v) => v,
            _ => panic!("wrong body"),
        };
        let names = Vec::from_iter(v.drain());
        assert_eq!(names.len(), 3);

        let _ = Record::read(&mut buf).unwrap(); //val stream end

        assert!(!buf.has_remaining());

        trace!("app answers on get");
        let from_php =
            b"\x01\x0a\0\0\0\"\x06\0\n\0MPXS_CONNS\x08\0MAX_REQS\t\x01MAX_CONNS2\0\0\0\0\0\0";
        app_socket
            .write_buf(&mut Bytes::from(&from_php[..]))
            .await
            .unwrap();
    }
    async fn send_empty_get(
        cp: &ConPool,
        uri: &str,
    ) -> Result<Response<impl HttpBody<Data = Bytes, Error = IoError>>, IoError> {
        let b = TestBod { l: VecDeque::new() };
        let req = Request::get(uri).body(b).unwrap();
        let params: HashMap<Bytes, Bytes> = HashMap::new();
        info!("reqesting");
        cp.forward(req, params).await
    }
    async fn mock_app(app_listener: &TcpListener, inst: u8) {
        let (mut app_socket, _) = app_listener.accept().await.unwrap();
        info!("accepted {inst}");
        let mut buf = BytesMut::with_capacity(128);
        app_socket.read_buf(&mut buf).await.unwrap();
        trace!("app read {:?}", buf);
        let to_php = b"\x01\x01\0\x01\0\x08\0\0\0\x01\x01\0\0\0\0\0\x01\x04\0\x01\0\"\x06\0\x0c\x01QUERY_STRING1\x0e\x03REQUEST_METHODGET\x01\x04\0\x01\0\"\x01\x04\0\x01\0\0\0\0\x01\x05\0\x01\0\0\0\0";
        assert_eq!(buf[..38], Bytes::from(&to_php[..38]));
        assert_eq!(buf[39..], Bytes::from(&to_php[39..]));

        trace!("app got get /?{}", buf[38] as char);

        let req_no = buf[38] - b'0';
//            if req_no==1 {
            tokio::time::sleep(Duration::from_millis(req_no as u64*500)).await;
//            }

        trace!("app answers on get /?{}", buf[38] as char);
        let from_php =
            b"\x01\x06\0\x01\0\x1b\x05\0Status: 404 Not Found\r\n\r\n\r\n\x01\x06\0\x01\0\x01\x03\0\x01\0\x08\0\0\0\0\0\0\0\0\0\0";
        app_socket
            .write_buf(&mut Bytes::from(&from_php[..]))
            .await
            .unwrap();
        /*
        buf.clear();
        app_socket.read_buf(&mut buf).await.unwrap();
        trace!("app read {:?}", buf);
        let to_php2 = b"\x01\x02\0\x01\0\0\0\0";
        assert_eq!(buf, Bytes::from(&to_php2[..]));*/
    }

    async fn con() {
        let (app_listener, a) = local_socket_pair().await.unwrap();
        info!("bound");
        let client = tokio::spawn(async move {
            let a = a.into();
            let cp = ConPool::new(&a).await.unwrap();
            assert_eq!(cp.max_cons, 2);
            assert_eq!(cp.max_req_per_con, 1);
            //TODO slow req + 2nd request
            tokio::select! {
                biased;
                res = send_empty_get(&cp, "/?1") => {
                    assert_eq!(res.expect("forward failed").status(), StatusCode::NOT_FOUND);
                    println!("do_stuff_async() completed first")
                }
                res2 = send_empty_get(&cp, "/?2") => {
                    assert_eq!(res2.expect("forward failed").status(), StatusCode::NOT_FOUND);
                    println!("more_async_work() completed first")
                }
                res3 = send_empty_get(&cp, "/?3") => {
                    assert_eq!(res3.expect("forward failed").status(), StatusCode::NOT_FOUND);
                    println!("more_async_work() completed first")
                }
            };

            //TODO fast req + reused connection request (slow accept in new server sock)
        });
        mock_app_w2cons(&app_listener).await;
        
        let _res = tokio::join!(
            mock_app(&app_listener, 0),
            mock_app(&app_listener, 2)
        );
        client.await.unwrap();
    }
    rt.block_on(con());
}
