/*! FastCGI implementation in pure Rust.

Developed for [FlashRust Webserver](https://github.com/User65k/flash_rust_ws)
with focus on
- Vectorized IO and Zero Copy
- async IO / tokio
- easy [HTTP](https://crates.io/crates/http) interfaces

 The default is only to provide the FastCGI Record parsing.
 Use these features to get
 - `con_pool`: [`client::con_pool::ConPool`] to manage a set of Connections
 - `web_server`: [`client::connection::Connection`] to easily resolv HTTPRequests to HTTPResponses
 - `app_start`: [`client::con_pool::ConPool`] gains prep_server methode to start an FCGI Application

*/
#![cfg_attr(docsrs, feature(doc_cfg))]

mod bufvec;
pub mod fastcgi;

#[cfg(feature = "web_server")]
#[cfg_attr(docsrs, doc(cfg(feature = "web_server")))]
pub mod stream;
#[cfg(feature = "web_server")]
#[cfg_attr(docsrs, doc(cfg(feature = "web_server")))]
pub use async_stream_connection::Addr as FCGIAddr;

#[cfg(feature = "web_server")]
#[cfg_attr(docsrs, doc(cfg(feature = "web_server")))]
pub mod client;

#[cfg(feature = "web_server")]
#[cfg_attr(docsrs, doc(cfg(feature = "web_server")))]
mod httpparse;

#[cfg(feature = "application")]
#[cfg_attr(docsrs, doc(cfg(feature = "application")))]
pub mod server;

#[cfg(feature = "codec")]
#[cfg_attr(docsrs, doc(cfg(feature = "codec")))]
pub mod codec;
