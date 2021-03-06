
/*! FastCGI implementation in pure Rust.

Developed for [FlashRust Webserver](https://github.com/User65k/flash_rust_ws)
with focus on
- Vectorized IO and Zero Copy
- async IO / tokio
- easy [HTTP](https://crates.io/crates/http) interfaces
 
 The default is only to provide the FastCGI Record parsing.
 Use these features to get
 - `con_pool`: [`ConPool`] to manage a set of Connections
 - `web_server`: [`Connection`] to easily resolv HTTPRequests to HTTPResponses
 - `application`: [`FCGICodec`] a tokio codec for FastCGI Servers / Applications
 - `app_start`: [`ConPool`] gains prep_server methode to start an FCGI Application

[`ConPool`]: ./client/con_pool/struct.ConPool.html
[`Connection`]: ./client/connection/index.html
[`FCGICodec`]: ./server/struct.FCGICodec.html
*/

pub mod fastcgi;
mod bufvec;

#[cfg(feature = "web_server")]
pub mod stream;
#[cfg(feature = "web_server")]
pub use stream::FCGIAddr;

#[cfg(feature = "web_server")]
pub mod client;

#[cfg(feature = "web_server")]
mod httpparse;

#[cfg(feature = "application")]
pub mod server;

#[cfg(feature = "codec")]
pub mod codec;