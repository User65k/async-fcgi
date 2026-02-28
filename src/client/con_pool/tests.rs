
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
        cp: std::rc::Rc<ConPool>,
        uri: &str,
    ) -> Result<Response<impl HttpBody<Data = Bytes, Error = IoError>>, IoError> {
        let b = TestBod { l: VecDeque::new() };
        let req = Request::get(uri).body(b).unwrap();
        let params: HashMap<Bytes, Bytes> = HashMap::new();
        info!("reqesting {}", &uri);
        cp.forward(req, params).await
    }
    async fn mock_app(app_listener: &TcpListener, inst: u8) {
        let (mut app_socket, _) = app_listener.accept().await.unwrap();
        info!("accepted {inst}");
        for i in 0..inst {
            let mut buf = BytesMut::with_capacity(128);
            app_socket.read_buf(&mut buf).await.unwrap();
            trace!("app read {:?}", buf);
            let to_php = b"\x01\x01\0\x01\0\x08\0\0\0\x01\x01\0\0\0\0\0\x01\x04\0\x01\0\"\x06\0\x0c\x01QUERY_STRING1\x0e\x03REQUEST_METHODGET\x01\x04\0\x01\0\"\x01\x04\0\x01\0\0\0\0\x01\x05\0\x01\0\0\0\0";
            assert_eq!(buf[..38], Bytes::from(&to_php[..38]));
            assert_eq!(buf[39..], Bytes::from(&to_php[39..]));

            trace!("app got get /?{} on {i}", buf[38] as char);
            match buf[38] {
                b'1'|b'3' => assert_eq!(i, 0),
                b'2' => assert_eq!(i, 1),
                _ => panic!("w00t")
            }
            

            let req_no = buf[38] - b'0';
    //            if req_no==1 {
                tokio::time::sleep(Duration::from_millis(req_no as u64*500)).await;
    //            }

            trace!("app answers on get /?{} on {i}", buf[38] as char);
            let mut from_php =
                *b"\x01\x06\0\x01\0\x1b\x05\0Status: 204 Not Found\r\n\r\n\r\n\x01\x06\0\x01\0\x01\x03\0\x01\0\x08\0\0\0\0\0\0\0\0\0\0";
            from_php[18] = buf[38];
            app_socket
                .write_buf(&mut Bytes::from(from_php.to_vec()))
                .await
                .unwrap();
        }
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
        let s = tokio::spawn(async move {
            mock_app_w2cons(&app_listener).await;
            
            let _res = tokio::join!(
                mock_app(&app_listener, 2),
                mock_app(&app_listener, 1)
            );
        });
        let a = a.into();

        let local = tokio::task::LocalSet::new();
        let mut set = tokio::task::JoinSet::new();
        let client = local.run_until(async move {
            let cp = std::rc::Rc::new(ConPool::new(&a).await.unwrap());
            assert_eq!(cp.max_cons, 2);
            assert_eq!(cp.max_req_per_con, 1);
            let cp1 = cp.clone();
            set.spawn_local(async move {
                let res = send_empty_get(cp1, "/?3").await;
                assert_eq!(res.expect("forward failed").status(), StatusCode::NON_AUTHORITATIVE_INFORMATION);
                3
            });
            let cp1 = cp.clone();
            set.spawn_local(async move {
                let res = send_empty_get(cp1, "/?1").await;
                assert_eq!(res.expect("forward failed").status(), StatusCode::CREATED);
                1
            });
            let cp1 = cp.clone();
            set.spawn_local(async move {
                let res = send_empty_get(cp1, "/?2").await;
                assert_eq!(res.expect("forward failed").status(), StatusCode::ACCEPTED);
                2
            });

            let res = set.join_next().await.unwrap();
            assert_eq!(res.unwrap(), 1);
            info!("1st done");
            assert!(set.join_next().await.unwrap().is_ok());
            info!("2nd done");
            assert!(set.join_next().await.unwrap().is_ok());
            info!("3rd done");
            assert_eq!(2, cp.con_pool.read().await.len());
        });

        //TODO fast req + reused connection request (slow accept in new server sock)
        client.await;
        s.await.unwrap();
    }
    rt.block_on(con());
}
