// `mem::uninitialized` replaced with `mem::MaybeUninit`,
// can't upgrade yet
#![allow(deprecated)]

use std::mem;

use bytes::{Buf, Bytes};
use http::header::{HeaderMap, HeaderName, HeaderValue};

use httparse::parse_headers;
const MAX_HEADERS: usize = 100;

//https://github.com/hyperium/hyper/blob/0eaf304644a396895a4ce1f0146e596640bb666a/src/proto/h1/role.rs

pub(crate) enum ParseResult {
    Ok(Bytes),
    Pending,
    Err,
}

pub(crate) fn parse(mut bytes: Bytes, header_map: &mut HeaderMap) -> ParseResult {
    let mut headers: [httparse::Header<'_>; MAX_HEADERS] = unsafe { mem::uninitialized() };
    let buf = bytes.chunk();

    if let Ok(pr) = parse_headers(buf, &mut headers) {
        match pr {
            httparse::Status::Complete((pos, headers)) => {
                let bytes_ptr = bytes.as_ptr() as usize;
                for header in headers {
                    let name = HeaderName::from_bytes(header.name.as_bytes())
                        .expect("header name validated by httparse");
                    let value_start = header.value.as_ptr() as usize - bytes_ptr;
                    let value_end = value_start + header.value.len();
                    let value = unsafe {
                        HeaderValue::from_maybe_shared_unchecked(
                            bytes.slice(value_start..value_end),
                        )
                    };
                    header_map.insert(name, value);
                }
                let _ = bytes.split_to(pos);
                ParseResult::Ok(bytes)
            }
            httparse::Status::Partial => ParseResult::Pending,
        }
    } else {
        ParseResult::Err
    }
}

#[test]
fn test_parse() {
    use bytes::Bytes;
    let buf = b"Status: 202 Not Found\r\nX-Powered-By: PHP/7.3.16\r\nX-Authenticate: NTLM\r\nContent-type: text/html; charset=UTF-8\r\n\r\n<html><body>\npub\n<pre>Array\n(\n)\nArray\n(\n    [lol] => 1\n)\nArray\n(\n    [lol] => 1\n)\nArray\n(\n    [HTTP_accept] => text/html\n    [REQUEST_METHOD] => GET\n    [QUERY_STRING] => lol=1\n    [SCRIPT_NAME] => /test\n    [SCRIPT_FILENAME] => /home/daniel/Public/test.php\n    [FCGI_ROLE] => RESPONDER\n    [PHP_SELF] => /test\n    [REQUEST_TIME_FLOAT] => 1587740954.2741\n    [REQUEST_TIME] => 1587740954\n)\n";
    let bytes = Bytes::from(&buf[..]);
    let mut map = HeaderMap::new();
    if let ParseResult::Ok(body) = parse(bytes.clone(), &mut map) {
        assert_eq!(body, bytes.slice(113..));
        assert_eq!(map.get("Status").unwrap(), "202 Not Found");
        assert_eq!(map.get("X-Powered-By").unwrap(), "PHP/7.3.16");
        assert_eq!(map.get("X-Authenticate").unwrap(), "NTLM");
        assert_eq!(map.get("Content-type").unwrap(), "text/html; charset=UTF-8");
    } else {
        assert!(false);
    }
}
