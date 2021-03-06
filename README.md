[![Project Status: Active – The project has reached a stable, usable state and is being actively developed.](https://www.repostatus.org/badges/latest/active.svg)](https://www.repostatus.org/#active)
[![crates.io](https://meritbadge.herokuapp.com/async-fcgi)](https://crates.io/crates/async-fcgi)
[![Released API docs](https://docs.rs/async-fcgi/badge.svg)](https://docs.rs/async-fcgi)
[![GitHub](https://img.shields.io/github/license/User65k/async-fcgi)](./LICENSE)

FastCGI implementation in pure Rust.

The focus is on the webserver/client side, but the application/server side could be added in the future.

Developed for [FlashRust Webserver](https://github.com/User65k/flash_rust_ws)
with focus on
- Vectorized IO and Zero Copy
- async IO / tokio
- easy [HTTP](https://crates.io/crates/http) interfaces

Tested with:
- Flup (Python)
- PHP

`cargo run --example webserver --features="con_pool"`

# TODOs

- [x] `Connection` should handle UnixStream and TCPStream transparently
- [ ] `con_pool` should handle more than one connection :sweat_smile: and load balance
- [ ] A dropped `FCGIBody` should not block a RequestID
- [x] `Connection` should reconnect to the FCGI App if a connection is closed
- [ ] `Connection` should handle overload error from FCGI app
- [ ] Parsing for FCGI application/server side
- [x] Means to start an FCGI server (exec + env)
- [ ] Support Key-Value Pairs bigger than maximum record size

PullRequests are welcome BTW

# Other FCGI Crates

- [fastcgi-client](https://crates.io/crates/fastcgi-client): Async client
- [fastcgi](https://crates.io/crates/fastcgi): Synchronous Server
- [gfcgi](https://crates.io/crates/gfcgi): Only Server Side
- [fastcgi-sdk](https://crates.io/crates/fastcgi-sdk): Binding to the FastCGI SDK
- [fcgi](https://crates.io/crates/fcgi): Bindings
