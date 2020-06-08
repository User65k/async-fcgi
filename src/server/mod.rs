/*! Fast CGI server/application side
 * 
*/

/*
use tokio_util::codec::{Decoder, Encoder};
use crate::fastcgi::Record;
use crate::bufvec::BufList;
use std::io::Error as IoError;
use bytes::BytesMut;
*/

/// A `Codec` implementation that parses FCGI requests for FCGI servers like backend services.
/// The Decoder parses and returns `fastcgi::Record` objects containing header/body request data from an
/// FCGI client such as a frontend web server. The Encoder passes through the raw response to be sent
/// back to the FCGI client.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FCGICodec {
}


impl FCGICodec {
    /// Returns a client `FCGICodec` for accepting and parsing FCGI-format requests by FCGI servers
    /// like backend services.
    pub fn new() -> FCGICodec {
        FCGICodec {
        }
    }
}
/*
/// Decodes FCGI-format requests, while forwarding through any content payload
impl Decoder for FCGICodec {
    type Item = Record;
    type Error = IoError;

    fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<Record>, IoError> {
        if buf.len() <= 8 {
            //we need at least a header
            return Ok(None);
        }
        let mut b = &buf.freeze();
        Ok(Record::read(&mut b))
    }
}*/

/*// Forwards a response to an FCGI request back to the client/webserver.
impl Encoder for FCGICodec {
    type Item = Record;
    type Error = IoError;

    fn encode(&mut self, data: Record, buf: &mut BytesMut) -> Result<(), IoError> {
        let mut b = BufList::new();
        data.append(&mut b);
        buf.reserve(b.remaining());
        while let Some(bytes) = b.oldest() {
            buf.put(bytes);
        }
        Ok(())
    }
}*/