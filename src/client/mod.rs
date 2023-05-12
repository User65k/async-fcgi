/*! Fast CGI client/webserver side
 *
*/

#[cfg(feature = "con_pool")]
pub mod con_pool;
#[cfg(feature = "web_server")]
pub mod connection;


#[cfg(test)]
pub(crate) mod tests {
    use std::net::SocketAddr;
    use tokio::net::TcpListener;
    use async_stream_connection::Addr;

    pub(crate) async fn local_socket_pair() -> Result<(TcpListener, Addr), std::io::Error> {
        let a: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let app_listener = TcpListener::bind(a).await?;
        let a: Addr = app_listener.local_addr()?.into();
        Ok((app_listener, a))
    }
}