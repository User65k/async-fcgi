/*! Contains constants and models for fcgi data records.
```
    use bytes::{Bytes, BytesMut};
    use async_fcgi::fastcgi::*;
    let mut b = BytesMut::with_capacity(96);
    BeginRequestBody::new(FastCGIRole::Responder,0,1).append(&mut b);
    let mut nv = NVBody::new();
    nv.add(NameValuePair::new(Bytes::from(&b"SCRIPT_FILENAME"[..]),Bytes::from(&b"/home/daniel/Public/test.php"[..]))).expect("record full");
    nv.to_record(RecordType::Params, 1).append(&mut b);
    NVBody::new().to_record(RecordType::Params, 1).append(&mut b);
```
*/
use crate::bufvec::BufList;
use bytes::{Buf, BufMut, Bytes, BytesMut};
#[cfg(feature = "web_server")]
use log::{debug, trace};
use std::{iter::{Extend, FromIterator, IntoIterator}, fmt::Display};

/// FCGI record header
#[derive(Debug, Clone)]
pub(crate) struct Header {
    version: u8,
    rtype: RecordType,
    request_id: u16, // A request ID R becomes active when the application receives a record {FCGI_BEGIN_REQUEST, R, …} and becomes inactive when the application sends a record {FCGI_END_REQUEST, R, …} to the Web server. Management records have a requestId value of zero
    content_length: u16,
    padding_length: u8, // align by 8
                        //    reserved: [u8; 1],
}
pub struct BeginRequestBody {
    role: FastCGIRole,
    flags: u8,
    //    reserved: [u8; 5],
}
pub struct EndRequestBody {
    pub app_status: u32,
    pub protocol_status: ProtoStatus,
    //    pub reserved: [u8; 3],
}
pub struct UnknownTypeBody {
    pub rtype: u8,
    //    pub reserved: [u8; 7],
}

pub struct NameValuePair {
    pub name_data: Bytes,
    pub value_data: Bytes,
}
/// Body type for Params and GetValues.
/// Holds multiple NameValuePair's
pub struct NVBody {
    pairs: BufList<Bytes>,
    len: u16,
}
/// Helper type to generate one or more NVBody Records,
/// depending on the space needed
pub struct NVBodyList {
    bodies: Vec<NVBody>,
}
pub struct STDINBody {}
pub enum Body {
    BeginRequest(BeginRequestBody),
    EndRequest(EndRequestBody),
    UnknownType(UnknownTypeBody),
    Params(NVBody),
    StdIn(Bytes),
    StdErr(Bytes),
    StdOut(Bytes),
    Abort,
    GetValues(NVBody),
    GetValuesResult(NVBody),
}
/// FCGI record
pub struct Record {
    header: Header,
    pub body: Body,
}

/// Listening socket file number
pub const LISTENSOCK_FILENO: u8 = 0;

impl Body {
    /// Maximum length per record
    const MAX_LENGTH: usize = 0xffff;
}

impl Header {
    /// Number of bytes in a Header.
    ///
    /// Future versions of the protocol will not reduce this number.
    #[cfg(feature = "codec")]
    pub const HEADER_LEN: usize = 8;

    /// version component of Header
    const VERSION_1: u8 = 1;
}

impl Record {
    /// Default request id component of Header
    pub const MGMT_REQUEST_ID: u16 = 0;
}
/// rtype of Header
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum RecordType {
    /// The Web server sends a FCGI_BEGIN_REQUEST record to start a request
    BeginRequest = 1,
    /// A Web server aborts a FastCGI request when an HTTP client closes its transport connection while the FastCGI request is running on behalf of that client
    AbortRequest = 2,
    /// The application sends a FCGI_END_REQUEST record to terminate a request
    EndRequest = 3,
    /// Receive name-value pairs from the Web server to the application
    Params = 4,
    /// Byte Stream
    StdIn = 5,
    /// Byte Stream
    StdOut = 6,
    /// Byte Stream
    StdErr = 7,
    /// Byte Stream
    Data = 8,
    /// The Web server can query specific variables within the application
    /// The application receives.
    GetValues = 9,
    /// The Web server can query specific variables within the application.
    /// The application responds.
    GetValuesResult = 10,
    /// Unrecognized management record
    #[cfg(feature = "web_server")]
    UnknownType = 11,
}
impl TryFrom<u8> for RecordType {
    type Error = u8;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(RecordType::BeginRequest),
            2 => Ok(RecordType::AbortRequest),
            3 => Ok(RecordType::EndRequest),
            4 => Ok(RecordType::Params),
            5 => Ok(RecordType::StdIn),
            6 => Ok(RecordType::StdOut),
            7 => Ok(RecordType::StdErr),
            8 => Ok(RecordType::Data),
            9 => Ok(RecordType::GetValues),
            10 => Ok(RecordType::GetValuesResult),
            #[cfg(feature = "web_server")]
            11 => Ok(RecordType::UnknownType),
            o => Err(o),
        }
    }
}
impl Display for RecordType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            //RecordType::Other(o) => f.write_fmt(format_args!("Unknown Status: {}", o)),
            RecordType::BeginRequest => f.write_str("BeginRequest"),
            RecordType::AbortRequest => f.write_str("AbortRequest"),
            RecordType::EndRequest => f.write_str("EndRequest"),
            RecordType::Params => f.write_str("Params"),
            RecordType::StdIn => f.write_str("StdIn"),
            RecordType::StdOut => f.write_str("StdOut"),
            RecordType::StdErr => f.write_str("StdErr"),
            RecordType::Data => f.write_str("Data"),
            RecordType::GetValues => f.write_str("GetValues"),
            RecordType::GetValuesResult => f.write_str("GetValuesResult"),
            #[cfg(feature = "web_server")]
            RecordType::UnknownType => f.write_str("UnknownType"),
        }
    }
}
impl BeginRequestBody {
    /// Mask for flags component of BeginRequestBody
    pub const KEEP_CONN: u8 = 1;
}
/// FastCGI role
#[repr(u16)]
pub enum FastCGIRole {
    /// emulated CGI/1.1 program
    Responder = 1,
    /// authorized/unauthorized decision
    Authorizer = 2,    
    Filter = 3
}
/// protocol_status component of EndRequestBody
#[repr(u8)]
pub enum ProtoStatus {
    /// Normal end of request
    Complete = 0,
    /// Application is designed to process one request at a time per connection
    CantMpxCon = 1,
    /// The application runs out of some resource, e.g. database connections
    Overloaded = 2,
    /// Web server has specified a role that is unknown to the application
    UnknownRole = 3,
}
impl TryFrom<u8> for ProtoStatus {
    type Error = u8;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(ProtoStatus::Complete),
            1 => Ok(ProtoStatus::CantMpxCon),
            2 => Ok(ProtoStatus::Overloaded),
            3 => Ok(ProtoStatus::UnknownRole),
            o => Err(o),
        }
    }
}
impl Display for ProtoStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtoStatus::Complete => f.write_str("Complete"),
            ProtoStatus::CantMpxCon => f.write_str("CantMpxCon"),
            ProtoStatus::Overloaded => f.write_str("Overloaded"),
            ProtoStatus::UnknownRole => f.write_str("UnknownRole"),
        }
    }
}
/// The maximum number of concurrent transport connections this application will accept
/// 
/// e.g. "1" or "10".
/// Used with GET_VALUES / GET_VALUES_RESULT records.
pub const MAX_CONNS: &[u8] = b"MAX_CONNS";

/// The maximum number of concurrent requests this application will accept
/// 
/// e.g. "1" or "50".
/// Used with GET_VALUES / GET_VALUES_RESULT records.
pub const MAX_REQS: &[u8] = b"MAX_REQS";

/// If this application do multiplex connections
/// 
/// i.e. handle concurrent requests over each connection ("1": Yes, "0": No).
/// Used with GET_VALUES / GET_VALUES_RESULT records.
pub const MPXS_CONNS: &[u8] = b"MPXS_CONNS";

// ----------------- implementation -----------------

impl NameValuePair {
    //pub name_data: Vec<u8>;
    //pub value_data: Vec<u8>;
    pub fn parse(data: &mut Bytes) -> NameValuePair {
        let mut pos: usize = 0;
        let key_length = NameValuePair::param_length(data, &mut pos);
        let value_length = NameValuePair::param_length(data, &mut pos);
        let key = data.slice(pos..pos + key_length);
        pos += key_length;
        let value = data.slice(pos..pos + value_length);
        pos += value_length;
        data.advance(pos);

        NameValuePair {
            name_data: key,
            value_data: value,
        }
    }
    pub fn new(name_data: Bytes, value_data: Bytes) -> NameValuePair {
        NameValuePair {
            name_data,
            value_data,
        }
    }

    fn param_length(data: &Bytes, pos: &mut usize) -> usize {
        let mut length: usize = data[*pos] as usize;

        if (length >> 7) == 1 {
            length = (data.slice(*pos + 1..(*pos + 4)).get_u32() & 0x7FFFFFFF) as usize;

            *pos += 4;
        } else {
            *pos += 1;
        }

        length
    }
    pub fn len(&self) -> usize {
        let ln = self.name_data.len();
        let lv = self.value_data.len();
        let mut lf: usize = ln + lv + 2;
        if ln > 0x7f {
            lf += 3;
        }
        if lv > 0x7f {
            lf += 3;
        }
        lf
    }
}

impl STDINBody {
    /// create a single STDIN record from Bytes
    /// remaining bytes might be left if the record is full
    #[allow(clippy::new_ret_no_self)]
    pub fn new(request_id: u16, b: &mut dyn Buf) -> Record {
        let mut rec_data = b.take(Body::MAX_LENGTH);
        let len = rec_data.remaining();

        Record {
            header: Header::new(RecordType::StdIn, request_id, len as u16),
            body: Body::StdIn(rec_data.copy_to_bytes(len)),
        }
    }
}

impl Header {
    pub fn new(rtype: RecordType, request_id: u16, len: u16) -> Header {
        let mut pad: u8 = (len % 8) as u8;
        if pad != 0 {
            pad = 8 - pad;
        }
        Header {
            version: Header::VERSION_1,
            rtype,
            request_id,
            content_length: len,
            padding_length: pad,
        }
    }
    pub fn write_into<BM: BufMut>(self, data: &mut BM) {
        data.put_u8(self.version);
        data.put_u8(self.rtype as u8);
        data.put_u16(self.request_id);
        data.put_u16(self.content_length);
        data.put_u8(self.padding_length);
        data.put_u8(0); // reserved
                        //debug!("h {} {} -> {:?}",self.request_id, self.content_length, &data);
    }
    #[cfg(feature = "web_server")]
    fn parse(data: &mut Bytes) -> Header {
        let h = Header {
            version: data.get_u8(),
            rtype: data.get_u8().try_into().expect("not a fcgi type"),
            request_id: data.get_u16(),
            content_length: data.get_u16(),
            padding_length: data.get_u8(),
        };
        data.advance(1); // reserved
        h
    }
    #[inline]
    #[cfg(feature = "codec")]
    pub fn get_padding(&self) -> u8 {
        self.padding_length
    }
}

impl BeginRequestBody {
    /// create a record of type BeginRequest
    #[allow(clippy::new_ret_no_self)]
    pub fn new(role: FastCGIRole, flags: u8, request_id: u16) -> Record {
        Record {
            header: Header {
                version: Header::VERSION_1,
                rtype: RecordType::BeginRequest,
                request_id,
                content_length: 8,
                padding_length: 0,
            },
            body: Body::BeginRequest(BeginRequestBody { role, flags }),
        }
    }
}
/// to get a sream of records from a stream of bytes
/// a record does not need to be completely available
#[cfg(feature = "web_server")]
pub(crate) struct RecordReader {
    current: Option<Header>,
}
#[cfg(feature = "web_server")]
impl RecordReader {
    pub(crate) fn new() -> RecordReader {
        RecordReader { current: None }
    }
    pub(crate) fn read(&mut self, data: &mut Bytes) -> Option<Record> {
        let mut full_header = match self.current.take() {
            Some(h) => h,
            None => {
                //dont alter data until we have a whole header to read
                if data.remaining() < 8 {
                    return None;
                }
                debug!("new header");
                Header::parse(data)
            }
        };
        let mut body_len = full_header.content_length as usize;
        let header = if data.remaining() < body_len {
            let mut nh = full_header.clone();
            body_len = data.remaining();
            nh.content_length = body_len as u16;
            nh.padding_length = 0;

            //store rest for next time
            full_header.content_length -= body_len as u16;
            self.current = Some(full_header);
            //we need to enforce a read if we are out of data
            if body_len < 1 {
                return None;
            }

            debug!("more later, now:");
            nh
        } else {
            full_header
        };

        debug!("read type {}", header.rtype);
        trace!("payload: {:?}", &data.slice(..body_len));
        let body = data.slice(0..body_len);
        data.advance(body_len);

        if data.remaining() < header.padding_length as usize {
            //only possible if last/only fragment -> self.current is None
            if body_len < 1 {
                //no data at all
                self.current = Some(header);
                return None;
            }
            let mut nh = header.clone();
            nh.content_length = 0;
            self.current = Some(nh);
            debug!("padding {} is still missing", header.padding_length);
        } else {
            data.advance(header.padding_length as usize);
        }

        let body = Record::parse_body(body, header.rtype);
        Some(Record { header, body })
    }
}
impl Record {
    #[cfg(feature = "web_server")]
    pub(crate) fn get_request_id(&self) -> u16 {
        self.header.request_id
    }
    /// parse bytes to a single record
    /// returns `Ǹone` and leaves data untouched if not enough data is available
    #[cfg(feature = "con_pool")]
    pub(crate) fn read(data: &mut Bytes) -> Option<Record> {
        //dont alter data until we have a whole packet to read
        if data.remaining() < 8 {
            return None;
        }
        let header = Header::parse(&mut data.slice(..));
        let len = header.content_length as usize + header.padding_length as usize;
        if data.remaining() < len + 8 {
            return None;
        }
        data.advance(8);
        debug!("read type {}", header.rtype);
        trace!(
            "payload: {:?}",
            &data.slice(..header.content_length as usize)
        );
        let body = data.slice(0..header.content_length as usize);
        data.advance(len);
        let body = Record::parse_body(body, header.rtype);
        Some(Record { header, body })
    }
    #[cfg(feature = "web_server")]
    fn parse_body(mut payload: Bytes, ptype: RecordType) -> Body {
        match ptype {
            RecordType::StdOut => Body::StdOut(payload),
            RecordType::StdErr => Body::StdErr(payload),
            RecordType::EndRequest => Body::EndRequest(EndRequestBody::parse(payload)),
            RecordType::UnknownType => {
                let rtype = payload.get_u8();
                payload.advance(7);
                Body::UnknownType(UnknownTypeBody { rtype })
            }
            RecordType::GetValuesResult => Body::GetValuesResult(NVBody::from_bytes(payload)),
            RecordType::GetValues => Body::GetValues(NVBody::from_bytes(payload)),
            RecordType::Params => Body::Params(NVBody::from_bytes(payload)),
            _ => panic!("not impl"),
        }
    }
    ///serialize this record and append it to buf - used by tests
    pub fn append<BM: BufMut>(self, buf: &mut BM) {
        match self.body {
            Body::BeginRequest(brb) => {
                self.header.write_into(buf);
                brb.write_into(buf);
            }
            Body::Params(nvs) | Body::GetValues(nvs) | Body::GetValuesResult(nvs) => {
                let pad = self.header.padding_length as usize;
                self.header.write_into(buf);
                let kvp = nvs.drain().0;
                buf.put(kvp);
                unsafe {
                    buf.advance_mut(pad);
                } // padding, value does not matter
            }
            Body::StdIn(b) => {
                let pad = self.header.padding_length as usize;
                self.header.write_into(buf);
                if !b.has_remaining() {
                    //end stream has no data or padding
                    debug_assert!(pad == 0);
                    return;
                }
                buf.put(b);
                unsafe {
                    buf.advance_mut(pad);
                } // padding, value does not matter
            }
            Body::Abort => {
                self.header.write_into(buf);
            }
            _ => panic!("not impl"),
        }
    }
}
impl NVBody {
    pub fn new() -> NVBody {
        NVBody {
            pairs: BufList::new(),
            len: 0,
        }
    }
    pub fn to_record(self, rtype: RecordType, request_id: u16) -> Record {
        let mut pad: u8 = (self.len % 8) as u8;
        if pad != 0 {
            pad = 8 - pad;
        }
        Record {
            header: Header {
                version: Header::VERSION_1,
                rtype,
                request_id,
                content_length: self.len,
                padding_length: pad,
            },
            body: match rtype {
                RecordType::Params => Body::Params(self),
                RecordType::GetValues => Body::GetValues(self),
                RecordType::GetValuesResult => Body::GetValuesResult(self),
                _ => panic!("No valid type"),
            },
        }
    }
    pub fn fits(&self, pair: &NameValuePair) -> bool {
        let l = pair.len() + self.len as usize;
        l <= Body::MAX_LENGTH
    }
    pub fn add(&mut self, pair: NameValuePair) -> Result<(), ()> {
        let l = pair.len() + self.len as usize;
        if l > Body::MAX_LENGTH {
            return Err(()); // this body is full
        }
        self.len = l as u16;

        let mut ln = pair.name_data.len();
        let mut lv = pair.value_data.len();
        if ln + lv > Body::MAX_LENGTH {
            return Err(()); // could be handled
        }
        let mut lf: usize = 2;
        if ln > 0x7f {
            if ln > 0x7fffffff {
                return Err(());
            }
            lf += 3;
            ln |= 0x80000000;
        }
        if lv > 0x7f {
            if lv > 0x7fffffff {
                return Err(());
            }
            lf += 3;
            lv |= 0x80000000;
        }
        let mut data: BytesMut = BytesMut::with_capacity(lf);
        if ln > 0x7f {
            data.put_u32(ln as u32);
        } else {
            data.put_u8(ln as u8);
        }
        if lv > 0x7f {
            data.put_u32(lv as u32);
        } else {
            data.put_u8(lv as u8);
        }
        //self.pairs.push(pair.write());
        self.pairs.push(data.freeze());
        self.pairs.push(pair.name_data);
        if lv > 0 {
            self.pairs.push(pair.value_data);
        }
        Ok(())
    }
    #[cfg(feature = "web_server")]
    pub(crate) fn from_bytes(buf: Bytes) -> NVBody {
        let mut b = NVBody::new();
        b.len = buf.remaining() as u16;
        if b.len > 0 {
            b.pairs.push(buf);
        }
        b
    }
    pub fn drain(mut self) -> NVDrain {
        NVDrain(self.pairs.copy_to_bytes(self.len as usize))
    }
}
pub struct NVDrain(Bytes);

impl Iterator for NVDrain {
    type Item = NameValuePair;

    /// might panic
    fn next(&mut self) -> Option<NameValuePair> {
        if !self.0.has_remaining() {
            return None;
        }
        Some(NameValuePair::parse(&mut self.0))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (1, None)
    }
}

impl NVBodyList {
    pub fn new() -> NVBodyList {
        NVBodyList {
            bodies: vec![NVBody::new()],
        }
    }
    /// # Panics
    /// If a KV-Pair is bigger than one Record
    pub fn add(&mut self, pair: NameValuePair) {
        let mut nv = self.bodies.last_mut().unwrap(); //never empty
        if !nv.fits(&pair) {
            let new = NVBody::new();
            self.bodies.push(new);
            nv = self.bodies.last_mut().unwrap(); //never empty
        }
        nv.add(pair).expect("KVPair bigger that 0xFFFF");
    }
}

impl FromIterator<(Bytes, Bytes)> for NVBodyList {
    fn from_iter<T: IntoIterator<Item = (Bytes, Bytes)>>(iter: T) -> NVBodyList {
        let mut nv = NVBodyList::new();
        nv.extend(iter);
        nv
    }
}

impl Extend<(Bytes, Bytes)> for NVBodyList {
    #[inline]
    fn extend<T: IntoIterator<Item = (Bytes, Bytes)>>(&mut self, iter: T) {
        for (k, v) in iter {
            self.add(NameValuePair::new(k, v));
        }
    }
}

impl BeginRequestBody {
    pub fn write_into<BM: BufMut>(self, data: &mut BM) {
        data.put_u16(self.role as u16);
        data.put_u8(self.flags);
        data.put_slice(&[0; 5]); // reserved
    }
}

impl EndRequestBody {
    #[cfg(feature = "web_server")]
    fn parse(mut data: Bytes) -> EndRequestBody {
        let b = EndRequestBody {
            app_status: data.get_u32(),
            protocol_status: data.get_u8().try_into().expect("not a fcgi status"),
        };
        data.advance(3); // reserved
        b
    }
}

impl std::fmt::Debug for NameValuePair {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:?} = {:?}", self.name_data, self.value_data)
    }
}

#[test]
fn encode_simple_get() {
    let mut b = BytesMut::with_capacity(80);
    BeginRequestBody::new(FastCGIRole::Responder, 0, 1).append(&mut b);
    let mut nv = NVBody::new();
    nv.add(NameValuePair::new(
        Bytes::from(&b"SCRIPT_FILENAME"[..]),
        Bytes::from(&b"/home/daniel/Public/test.php"[..]),
    ))
    .expect("record full");
    nv.to_record(RecordType::Params, 1).append(&mut b);
    NVBody::new().to_record(RecordType::Params, 1).append(&mut b);

    let mut dst = [0; 80];
    b.copy_to_slice(&mut dst);

    let expected = b"\x01\x01\0\x01\0\x08\0\0\0\x01\0\0\0\0\0\0\x01\x04\0\x01\0-\x03\0\x0f\x1cSCRIPT_FILENAME/home/daniel/Public/test.php\x01\x04\0\x01\x04\0\x01\0\0\0\0";

    assert_eq!(dst[..69], expected[..69]);
    //padding is uninit
    assert_eq!(dst[72..], expected[72..]);
}
#[test]
fn encode_post() {
    let mut b = BytesMut::with_capacity(104);
    BeginRequestBody::new(FastCGIRole::Responder, 0, 1).append(&mut b);
    let mut nv = NVBody::new();
    nv.add(NameValuePair::new(
        Bytes::from(&b"SCRIPT_FILENAME"[..]),
        Bytes::from(&b"/home/daniel/Public/test.php"[..]),
    ))
    .expect("record full");
    nv.to_record(RecordType::Params, 1).append(&mut b);
    NVBody::new().to_record(RecordType::Params, 1).append(&mut b);
    STDINBody::new(1, &mut Bytes::from(&b"a=b"[..])).append(&mut b);
    STDINBody::new(1, &mut Bytes::new()).append(&mut b);

    let mut dst = [0; 104];
    b.copy_to_slice(&mut dst);
    //padding is uninit
    dst[69] = 0xFF;
    dst[70] = 0xFF;
    dst[71] = 0xFF;
    dst[91] = 0xFF;
    dst[92] = 0xFF;
    dst[93] = 0xFF;
    dst[94] = 0xFF;
    dst[95] = 0xFF;
    let expected = b"\x01\x01\0\x01\0\x08\0\0\0\x01\0\0\0\0\0\0\x01\x04\0\x01\0-\x03\0\x0f\x1cSCRIPT_FILENAME/home/daniel/Public/test.php\xff\xff\xff\x01\x04\0\x01\0\0\0\0\x01\x05\0\x01\0\x03\x05\0a=b\xff\xff\xff\xff\xff\x01\x05\0\x01\0\0\0\0";

    assert_eq!(dst, &expected[..]);
}

#[test]
fn encode_long_param() {
    let mut b = BytesMut::with_capacity(190);
    BeginRequestBody::new(FastCGIRole::Responder, 0, 1).append(&mut b);
    let mut nv = NVBody::new();
    nv.add(NameValuePair::new(
        Bytes::from(&b"HTTP_ACCEPT"[..]),
        Bytes::from(&b"text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7"[..]),
    ))
    .expect("record full");
    nv.to_record(RecordType::Params, 1).append(&mut b);
    NVBody::new().to_record(RecordType::Params, 1).append(&mut b);

    let mut dst = [0; 184];
    b.copy_to_slice(&mut dst);

    //padding is uninit
    dst[175]=1;

    let expected = b"\x01\x01\0\x01\0\x08\0\0\0\x01\0\0\0\0\0\0\x01\x04\0\x01\0\x97\x01\0\x0b\x80\0\0\x87HTTP_ACCEPTtext/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7\x01\x01\x04\0\x01\0\0\0\0";

    assert_eq!(dst, expected[..]);
}
