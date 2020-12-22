/*! Contains constants and models for fcgi data records.
```
    use bytes::Bytes;
    use async_fcgi::fastcgi::*;
    let mut b = BufList::new();
    BeginRequestBody::new(BeginRequestBody::RESPONDER,0,1).append(&mut b);
    let mut nv = NVBody::new();
    nv.add(NameValuePair::new(Bytes::from(&b"SCRIPT_FILENAME"[..]),Bytes::from(&b"/home/daniel/Public/test.php"[..]))).expect("record full");
    nv.to_record(Record::PARAMS, 1).append(&mut b);
    NVBody::new().to_record(Record::PARAMS, 1).append(&mut b);
```
*/
use bytes::{Bytes, BytesMut, BufMut, Buf};
use crate::bufvec::BufList;
use log::{debug, trace};
use std::iter::{FromIterator, Extend, IntoIterator};

/// FCGI record header
#[derive(Debug, Clone)]
struct Header
{
    version: u8,
    rtype: u8,
    request_id: u16, // A request ID R becomes active when the application receives a record {FCGI_BEGIN_REQUEST, R, …} and becomes inactive when the application sends a record {FCGI_END_REQUEST, R, …} to the Web server. Management records have a requestId value of zero
    content_length: u16,
    padding_length: u8, // align by 8
//    reserved: [u8; 1],
}
pub struct BeginRequestBody
{
    role: u16,
    flags: u8,
//    reserved: [u8; 5],
}
pub struct EndRequestBody
{
    pub app_status: u32,
    pub protocol_status: u8,
//    pub reserved: [u8; 3],
}
pub struct UnknownTypeBody
{
    rtype: u8,
//    pub reserved: [u8; 7],
}

pub struct NameValuePair
{
    pub name_data: Bytes,
    pub value_data: Bytes,
}
/// Body type for Params and GetValues.
/// Holds multiple NameValuePair's
pub struct NVBody
{
    pairs: BufList<Bytes>,
    len: u16
}
/// Helper type to generate one or more NVBody Records,
/// depending on the space needed
pub struct NVBodyList
{
    bodies: Vec<NVBody>
}
pub struct STDINBody{

}
pub enum Body
{
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
pub struct Record
{
    header: Header,
    pub body: Body
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
    const HEADER_LEN: usize = 8;

    /// version component of Header
    const VERSION_1: u8 = 1;
}

impl Record {
    /// Default request id component of Header
    pub const MGMT_REQUEST_ID: u16 = 0;

    /// type component of Header
    /// # Request
    /// The Web server sends a FCGI_BEGIN_REQUEST record to start a request
    const BEGIN_REQUEST: u8 = 1;

    /// type component of Header
    /// # Request
    /// A Web server aborts a FastCGI request when an HTTP client closes its transport connection while the FastCGI request is running on behalf of that client
    const ABORT_REQUEST: u8 = 2;

    /// type component of Header
    /// # Response
    /// The application sends a FCGI_END_REQUEST record to terminate a request
    const END_REQUEST: u8 = 3;

    /// type component of Header
    /// # Request
    /// Receive name-value pairs from the Web server to the application
    pub const PARAMS: u8 = 4;

    /// type component of Header
    /// # Request
    /// Byte Stream
    const STDIN: u8 = 5;

    /// type component of Header
    /// # Response
    /// Byte Stream
    const STDOUT: u8 = 6;

    /// type component of Header
    /// # Response
    /// Byte Stream
    const STDERR: u8 = 7;

    /// type component of Header
    /// # Request
    /// Byte Stream
    const DATA: u8 = 8;

    /// type component of Header
    /// # Request
    /// The Web server can query specific variables within the application
    /// The application receives.
    pub const GET_VALUES: u8 = 9;

    /// type component of Header
    /// # Response
    /// The Web server can query specific variables within the application.
    /// The application responds.
    pub const GET_VALUES_RESULT: u8 = 10;

    /// type component of Header
    ///
    /// Unrecognized management record
    const UNKNOWN_TYPE: u8 = 11;
}
impl BeginRequestBody {
    /// Mask for flags component of BeginRequestBody
    pub const KEEP_CONN: u8 = 1;

    /// FastCGI role
    /// emulated CGI/1.1 program
    pub const RESPONDER: u16 = 1;

    /// FastCGI role
    /// authorized/unauthorized decision
    pub const AUTHORIZER: u16 = 2;

    /// FastCGI role
    /// extra stream of data from a file
    pub const FILTER: u16 = 3;
}
impl EndRequestBody {
    /// protocol_status component of EndRequestBody
    ///
    /// Normal end of request
    pub const REQUEST_COMPLETE: u8 = 0;

    /// protocol_status component of EndRequestBody
    ///
    /// Application is designed to process one request at a time per connection
    pub const CANT_MPX_CONN: u8 = 1;

    /// protocol_status component of EndRequestBody
    ///
    /// The application runs out of some resource, e.g. database connections
    pub const OVERLOADED: u8 = 2;

    /// protocol_status component of EndRequestBody
    ///
    /// Web server has specified a role that is unknown to the application
    pub const UNKNOWN_ROLE: u8 = 3;
}
/// Names for GET_VALUES / GET_VALUES_RESULT records.
///
/// The maximum number of concurrent transport connections this application will accept, e.g. "1" or "10".
pub const MAX_CONNS: &'static [u8] = b"MAX_CONNS";

/// Names for GET_VALUES / GET_VALUES_RESULT records.
///
/// The maximum number of concurrent requests this application will accept, e.g. "1" or "50".
pub const MAX_REQS: &'static [u8] = b"MAX_REQS";

/// Names for GET_VALUES / GET_VALUES_RESULT records.
///
/// If this application does not multiplex connections (i.e. handle concurrent requests over each connection), "1" otherwise.
pub const MPXS_CONNS: &'static [u8] = b"MPXS_CONNS";



// ----------------- implementation -----------------

impl NameValuePair
{
    //pub name_data: Vec<u8>;
    //pub value_data: Vec<u8>;
    pub fn parse(data: &mut Bytes) -> NameValuePair
    {
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
            value_data: value
        }
    }
    pub fn new(name_data: Bytes, value_data: Bytes) -> NameValuePair
    {
        NameValuePair {
            name_data: name_data,
            value_data: value_data
        }
    }
    
    fn param_length(data: &Bytes, pos: &mut usize) -> usize
    {
        let mut length: usize = data[*pos] as usize;

        if (length >> 7) == 1 {
            length = (data.slice(*pos+1..(*pos + 4)).get_u32() & 0x7FFFFFFF) as usize;

            *pos += 4;
        } else {
            *pos += 1;
        }

        length
    }
    fn len(&self) -> usize {
        let ln = self.name_data.len();
        let lv = self.value_data.len();
        let mut lf: usize = ln+lv+2;
        if ln > 0x7f {
            lf +=3;
        }
        if lv > 0x7f {
            lf +=3;
        }
        lf
    }
}

impl STDINBody
{
    /// create a single STDIN record from Bytes
    /// remaining bytes might be left if the record is full
    pub fn new(request_id: u16, b: &mut Bytes) -> Record
    {
        let mut size = b.remaining();
        if size > Body::MAX_LENGTH {
            size = Body::MAX_LENGTH;
        }

        Record {
            header: Header::new(Record::STDIN, request_id, size as u16),
            body: Body::StdIn(b.slice(..size))
        }
    }
}

impl Header {
    pub fn new(rtype:u8,request_id:u16,len:u16) -> Header {
        let mut pad: u8 = (len%8) as u8;
        if pad !=0 {
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
}

impl Header
{
    fn write_into(self, data: &mut BytesMut)
    {
        data.put_u8(self.version);
        data.put_u8(self.rtype);
        data.put_u16(self.request_id);
        data.put_u16(self.content_length);
        data.put_u8(self.padding_length);
        data.put_u8(0); // reserved
        debug!("h {} {} -> {:?}",self.request_id, self.content_length, &data);
    }
    fn parse(data: &mut Bytes) -> Header
    {
        let h = Header {
            version: data.get_u8(),
            rtype: data.get_u8(),
            request_id: data.get_u16(),
            content_length: data.get_u16(),
            padding_length: data.get_u8()
        };
        data.advance(1); // reserved
        h
    }
}

impl BeginRequestBody
{
    /// create a record of type BeginRequest
    pub fn new(role: u16, flags: u8, request_id: u16) -> Record
    {
        Record {
            header: Header {
                version: Header::VERSION_1,
                rtype: Record::BEGIN_REQUEST,
                request_id,
                content_length: 8,
                padding_length: 0,
            },
            body: Body::BeginRequest(BeginRequestBody {
                role,
                flags
            })
        }
    }
}
/// to get a sream of records from a stream of bytes
/// a record does not need to be completely available
pub(crate) struct RecordReader {
    current: Option<Header>
}
impl RecordReader
{
    pub(crate) fn new() -> RecordReader {
        RecordReader {
            current: None
        }
    }
    pub(crate) fn read(&mut self, data: &mut Bytes) -> Option<Record>
    { 
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
        }else{
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
        }else{
            data.advance(header.padding_length as usize);
        }

        let body = Record::parse_body(body, header.rtype);
        Some(Record {
            header,
            body
        })
    }
}
impl Record
{
    pub(crate) fn get_request_id(&self) -> u16 {
        self.header.request_id
    }
    /// parse bytes to a single record
    /// returns `Ǹone` and leaves data untouched if not enough data is available
    pub(crate) fn read(data: &mut Bytes) -> Option<Record>
    {
        //dont alter data until we have a whole packet to read
        if data.remaining() < 8 {
            return None;
        }
        let header = Header::parse(&mut data.slice(..));
        let len = header.content_length as usize+header.padding_length as usize;
        if data.remaining() < len+8 {
            return None;
        }
        data.advance(8);
        debug!("read type {}", header.rtype);
        trace!("payload: {:?}", &data.slice(..len));
        let body = data.slice(0..header.content_length as usize);
        data.advance(len);
        let body = Record::parse_body(body, header.rtype);
        Some(Record {
            header,
            body
        })
    }
    fn parse_body(mut payload: Bytes, ptype: u8) -> Body {
        match ptype {
            Record::STDOUT => Body::StdOut(payload),
            Record::STDERR => Body::StdErr(payload),
            Record::END_REQUEST => Body::EndRequest(EndRequestBody::parse(payload)),
            Record::UNKNOWN_TYPE => {
                let rtype = payload.get_u8();
                payload.advance(7);
                Body::UnknownType(UnknownTypeBody {
                    rtype
                })
            },
            Record::GET_VALUES_RESULT => Body::GetValuesResult(NVBody::from_bytes(payload)),
            Record::GET_VALUES => Body::GetValues(NVBody::from_bytes(payload)),
            Record::PARAMS => Body::Params(NVBody::from_bytes(payload)),            
            _ => panic!("not impl"),
        }
    }
    ///create a record of type ABORT_REQUEST
    pub(crate) fn abort(request_id: u16) -> Record {
        Record {
            header: Header {
                version: Header::VERSION_1,
                rtype: Record::ABORT_REQUEST,
                request_id,
                content_length: 0,
                padding_length: 0,
            },
            body: Body::Abort
        }
    }
    ///serialize this record and append it to buf
    pub(crate) fn append(self, buf: &mut BufList<Bytes>)
    {
        match self.body
        {
            Body::BeginRequest(brb) => {
                let mut data = BytesMut::with_capacity(Header::HEADER_LEN+8);
                self.header.write_into(&mut data);
                brb.write_into(&mut data);
                buf.push(data.into());
            },
            Body::Params(nvs) | Body::GetValues(nvs) | Body::GetValuesResult(nvs) => {
                let mut data = BytesMut::with_capacity(Header::HEADER_LEN);
                let pad = self.header.padding_length as usize;
                self.header.write_into(&mut data);
                let header = data.freeze();
                buf.push(header.clone());
                buf.append(nvs.pairs);
                if pad>0 {
                    buf.push(header.slice(0..pad));//..8 <= Header::HEADER_LEN
                }
            },
            Body::StdIn(b) => {
                let mut data = BytesMut::with_capacity(Header::HEADER_LEN);
                let pad = self.header.padding_length as usize;
                self.header.write_into(&mut data);
                let header = data.freeze();
                buf.push(header.clone());
                if !b.has_remaining()
                {
                    //end stream has no data or padding
                    debug_assert!(pad==0);
                    return;
                }
                buf.push(b);
                if pad>0 {
                    buf.push(header.slice(0..pad));//..8 <= Header::HEADER_LEN
                }
            },
            Body::Abort => {
                let mut data = BytesMut::with_capacity(Header::HEADER_LEN);
                self.header.write_into(&mut data);
                let header = data.freeze();
                buf.push(header);
            },
            _ => panic!("not impl"),
        }
    }
}
impl NVBody
{
    pub fn new() -> NVBody {
        NVBody {
            pairs: BufList::new(),
            len: 0
        }
    }
    pub fn to_record(self, rtype: u8, request_id: u16) -> Record {
        let mut pad: u8 = (self.len%8) as u8;
        if pad !=0 {
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
                Record::PARAMS => Body::Params(self),
                Record::GET_VALUES => Body::GetValues(self),
                Record::GET_VALUES_RESULT => Body::GetValuesResult(self),
                _ => panic!("No valid type"),
            }
        }
    }
    pub fn fits(&self, pair: &NameValuePair) -> bool {
        let l = pair.len()+self.len as usize;
        l <= Body::MAX_LENGTH
    }
    pub fn add(&mut self, pair: NameValuePair) -> Result<(),()> {
        let l = pair.len()+self.len as usize;
        if l > Body::MAX_LENGTH {
            return Err(());  // this body is full
        }
        self.len = l as u16;
        
        let mut ln = pair.name_data.len();
        let mut lv = pair.value_data.len();
        if ln+lv > Body::MAX_LENGTH {
            return Err(()); // could be handled
        }
        let mut lf: usize = 2;
        if ln > 0x7f {
            if ln > 0x7fffffff {
                return Err(());
            }
            lf +=3;
            ln |= 0x8000;
        }
        if lv > 0x7f {
            if lv > 0x7fffffff {
                return Err(());
            }
            lf +=3;
            lv |= 0x8000;
        }
        let mut data: BytesMut = BytesMut::with_capacity(lf);
        if ln > 0x7f {
            data.put_u32(ln as u32);
        }else{
            data.put_u8(ln as u8);
        }
        if lv > 0x7f {
            data.put_u32(lv as u32);
        }else{
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
    pub(crate) fn from_bytes(buf: Bytes) -> NVBody {
        let mut b = NVBody::new();
        b.len = buf.remaining() as u16;
        if b.len > 0 {
            b.pairs.push(buf);
        }
        b
    }
    pub fn drain(mut self) -> NVDrain {
        NVDrain(self.pairs.to_bytes())
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

impl NVBodyList{
    pub fn new() -> NVBodyList {
        NVBodyList{
            bodies: vec![NVBody::new()]
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
    pub(crate) fn append_records(self, rtype: u8, request_id: u16, wbuf: &mut BufList<Bytes>) {
        for nvbod in self.bodies {
            nvbod.to_record(rtype, request_id).append(wbuf);
        }
    }
}

impl FromIterator<(Bytes, Bytes)> for NVBodyList
{
    fn from_iter<T: IntoIterator<Item = (Bytes, Bytes)>>(iter: T) -> NVBodyList {
        let mut nv = NVBodyList::new();
        nv.extend(iter);
        nv
    }
}

impl Extend<(Bytes, Bytes)> for NVBodyList
{
    #[inline]
    fn extend<T: IntoIterator<Item = (Bytes, Bytes)>>(&mut self, iter: T) {
        for (k,v) in iter {
            self.add(NameValuePair::new(k,v));
        }
    }
}

impl BeginRequestBody
{
    fn write_into(self, data: &mut BytesMut)
    {
        data.put_u16(self.role);
        data.put_u8(self.flags);
        data.put_slice(&[0;5]); // reserved
        debug!("br {} -> {:?}",self.role, &data);
    }
}


impl EndRequestBody
{
    fn parse(mut data: Bytes) -> EndRequestBody
    {
        let b = EndRequestBody {
            app_status: data.get_u32(),
            protocol_status: data.get_u8(),
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
    let mut b = BufList::new();
    BeginRequestBody::new(BeginRequestBody::RESPONDER,0,1).append(&mut b);
    let mut nv = NVBody::new();
    nv.add(NameValuePair::new(Bytes::from(&b"SCRIPT_FILENAME"[..]),Bytes::from(&b"/home/daniel/Public/test.php"[..]))).expect("record full");
    nv.to_record(Record::PARAMS, 1).append(&mut b);
    NVBody::new().to_record(Record::PARAMS, 1).append(&mut b);

    assert_eq!(b.to_bytes(),
     &b"\x01\x01\0\x01\0\x08\0\0\0\x01\0\0\0\0\0\0\x01\x04\0\x01\0-\x03\0\x0f\x1cSCRIPT_FILENAME/home/daniel/Public/test.php\x01\x04\0\x01\x04\0\x01\0\0\0\0"[..]
    );
}
#[test]
fn encode_post() {
    let mut b = BufList::new();
    BeginRequestBody::new(BeginRequestBody::RESPONDER,0,1).append(&mut b);
    let mut nv = NVBody::new();
    nv.add(NameValuePair::new(Bytes::from(&b"SCRIPT_FILENAME"[..]),Bytes::from(&b"/home/daniel/Public/test.php"[..]))).expect("record full");
    nv.to_record(Record::PARAMS, 1).append(&mut b);
    NVBody::new().to_record(Record::PARAMS, 1).append(&mut b);
    STDINBody::new(1, &mut Bytes::from(&b"a=b"[..])).append(&mut b);
    STDINBody::new(1, &mut Bytes::new()).append(&mut b);

    assert_eq!(b.to_bytes(),
     &b"\x01\x01\0\x01\0\x08\0\0\0\x01\0\0\0\0\0\0\x01\x04\0\x01\0-\x03\0\x0f\x1cSCRIPT_FILENAME/home/daniel/Public/test.php\x01\x04\0\x01\x04\0\x01\0\0\0\0\x01\x05\0\x01\0\x03\x05\0a=b\x01\x05\0\x01\0\x01\x05\0\x01\0\0\0\0"[..]);
}