// `mem::uninitialized` replaced with `mem::MaybeUninit`,
// can't upgrade yet
#![allow(deprecated)]

use std::mem;

use bytes::{Bytes, Buf};
use http::header::{HeaderName, HeaderValue, HeaderMap};

use httparse::parse_headers;
const MAX_HEADERS: usize = 100;

//https://github.com/hyperium/hyper/blob/0eaf304644a396895a4ce1f0146e596640bb666a/src/proto/h1/role.rs

pub (crate) enum ParseResult {
    Ok(Bytes),
    Pending,
    Err
}

pub (crate) fn parse(mut bytes: Bytes, header_map: &mut HeaderMap) -> ParseResult
{
    let mut headers_indices: [HeaderIndices; MAX_HEADERS] = unsafe { mem::uninitialized() };
    let mut headers: [httparse::Header<'_>; MAX_HEADERS] = unsafe { mem::uninitialized() };
    //let mut bytes = BytesMut::from(&buf[..]);
    let buf = bytes.chunk();
    
    if let Ok(pr) = parse_headers(buf, &mut headers){
        match pr {
            httparse::Status::Complete(len) => {
                let headers_len = len.1.len();
                record_header_indices(buf, &len.1, &mut headers_indices);
                let slice = bytes.split_to(len.0);

                for header in &headers_indices[..headers_len] {
                    let name = HeaderName::from_bytes(&slice[header.name.0..header.name.1]).expect("header name validated by httparse");
                    let value = unsafe { HeaderValue::from_maybe_shared_unchecked(slice.slice(header.value.0..header.value.1))};
                    //print!("h {} = {:?}\r\n", name, value);
                    header_map.insert(name, value);
                }
                ParseResult::Ok(bytes)
            },
            httparse::Status::Partial => ParseResult::Pending
        }
    }else{
        ParseResult::Err
    }
}

#[test]
fn test_parse(){
    use bytes::Bytes;
    use httparse::parse_headers;
    const MAX_HEADERS: usize = 100;
    let mut headers_indices: [HeaderIndices; MAX_HEADERS] = unsafe { mem::uninitialized() };
    let mut headers: [httparse::Header<'_>; MAX_HEADERS] = unsafe { mem::uninitialized() };
    let buf = b"Status: 202 Not Found\r\nX-Powered-By: PHP/7.3.16\r\nX-Authenticate: NTLM\r\nContent-type: text/html; charset=UTF-8\r\n\r\n<html><body>\npub\n<pre>Array\n(\n)\nArray\n(\n    [lol] => 1\n)\nArray\n(\n    [lol] => 1\n)\nArray\n(\n    [HTTP_accept] => text/html\n    [REQUEST_METHOD] => GET\n    [QUERY_STRING] => lol=1\n    [SCRIPT_NAME] => /test\n    [SCRIPT_FILENAME] => /home/daniel/Public/test.php\n    [FCGI_ROLE] => RESPONDER\n    [PHP_SELF] => /test\n    [REQUEST_TIME_FLOAT] => 1587740954.2741\n    [REQUEST_TIME] => 1587740954\n)\n";
    let mut bytes = Bytes::from(&buf[..]);
    match parse_headers(buf, &mut headers).unwrap() {
        httparse::Status::Complete(len) => {
            print!("len {}\r\n", len.0);
            print!("data {:?}\r\n", len.1);
            print!("size {}\r\n", len.1.len());
            let headers_len = len.1.len();
            record_header_indices(buf, &len.1, &mut headers_indices);
            let slice = bytes.split_to(len.0);

            for header in &headers_indices[..headers_len] {
                let name = HeaderName::from_bytes(&slice[header.name.0..header.name.1]).expect("header name validated by httparse");
                let value = unsafe { HeaderValue::from_maybe_shared_unchecked(slice.slice(header.value.0..header.value.1))};
                print!("h {} = {:?}\r\n", name, value);
            }
            print!("b {:?}\r\n", bytes);
        },
        _ => {print!("ja");}
    }
}

#[derive(Clone, Copy)]
struct HeaderIndices {
    name: (usize, usize),
    value: (usize, usize),
}

fn record_header_indices(
    bytes: &[u8],
    headers: &[httparse::Header<'_>],
    indices: &mut [HeaderIndices],
) {
    let bytes_ptr = bytes.as_ptr() as usize;

    for (header, indices) in headers.iter().zip(indices.iter_mut()) {
        let name_start = header.name.as_ptr() as usize - bytes_ptr;
        let name_end = name_start + header.name.len();
        indices.name = (name_start, name_end);
        let value_start = header.value.as_ptr() as usize - bytes_ptr;
        let value_end = value_start + header.value.len();
        indices.value = (value_start, value_end);
    }

}
