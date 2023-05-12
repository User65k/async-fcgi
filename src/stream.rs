/*! FCGI Server/Clients usually support TCP as well as Unixsockets
 *
 *
 */
#[deprecated(since = "0.4.1",
    note="Use the crate async_stream_connection instead")]
pub use async_stream_connection::{Addr as FCGIAddr, Listener, Stream};
