use std::pin::Pin;
use std::task::{Poll, Context};

use tokio::io::{AsyncBufRead, AsyncRead, AsyncWrite, ReadBuf};
use bytes::{Buf, BufMut, Bytes, BytesMut};

use crate::fastcgi::{Record, NameValuePair, Header};
use tokio::io::AsyncWriteExt;
#[cfg(feature = "web_server")]
use http_body::Body;
use log::trace;

pub enum FCGIType{
    BeginRequest{request_id: u16,role: u16, flags: u8},
    AbortRequest{request_id: u16},
    EndRequest{request_id: u16, app_status: u32, proto_status: u8},
    Params{request_id: u16, p: Vec<NameValuePair>},
    STDIN{request_id: u16, data: Bytes},
    STDOUT{request_id: u16, data: Bytes},
    STDERR{request_id: u16, data: Bytes},
    DATA{request_id: u16, data: Bytes},
    GetValues{p: Vec<NameValuePair>},
    GetValuesResult{p: Vec<NameValuePair>},
}

pub struct FCGIWriter<RW> {
    io: RW,
    //current_read: Option<Header>
}
impl <RW: AsyncRead+AsyncWrite+Unpin>FCGIWriter<RW> {
    pub fn new(io: RW) -> FCGIWriter<RW> {
        FCGIWriter {
            io
        }
    }
}


const HEADER_LEN: usize = 8;
//buffer at max one record without padding, plus a header
const BUF_LEN: usize = 0xFF_FF-7+8;

impl <R: AsyncRead+Unpin>AsyncRead for FCGIWriter<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_read(cx, buf)
    }
}
impl<R: AsyncBufRead+Unpin> AsyncBufRead for FCGIWriter<R> {
    fn poll_fill_buf(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<&[u8]>> {
        Pin::new(&mut self.get_mut().io).poll_fill_buf(cx)
    }

    fn consume(self: Pin<&mut Self>, amt: usize) {
        Pin::new(&mut self.get_mut().io).consume(amt)
    }
}
impl <W: AsyncWrite+Unpin>FCGIWriter<W> {
    pub async fn shutdown(&mut self) -> std::io::Result<()> {
        self.io.shutdown().await
    }
    async fn write_whole_buf<B: Buf>(&mut self, buf: &mut B) -> std::io::Result<()> {
        while buf.has_remaining() {
            // Writes some prefix of the byte string, not necessarily
            // all of it.
            self.io.write_buf(buf).await?;
        }
        Ok(())
    }

    pub async fn encode(
        &mut self,
        item: FCGIType
    ) -> std::io::Result<()> {
        match item {
            FCGIType::BeginRequest { request_id, role, flags } => {
                let mut buf = BytesMut::with_capacity(Header::HEADER_LEN+8);
                Header::new(Record::BEGIN_REQUEST,request_id,8).write_into(&mut buf);
                buf.put_u16(role);
                buf.put_u8(flags);
                buf.put_slice(&[0;5]); // reserved
                self.write_whole_buf(&mut buf.freeze()).await?;
            }
            FCGIType::AbortRequest { request_id } => {
                let mut buf = BytesMut::with_capacity(Header::HEADER_LEN);
                Header::new(Record::ABORT_REQUEST,request_id,0).write_into(&mut buf);
                self.write_whole_buf(&mut buf.freeze()).await?;
            }
            FCGIType::EndRequest { request_id, app_status, proto_status } => {
                let mut buf = BytesMut::with_capacity(Header::HEADER_LEN+8);
                Header::new(Record::END_REQUEST,request_id,8).write_into(&mut buf);
                
                buf.put_u32(app_status);
                buf.put_u8(proto_status);
                buf.put_slice(&[0;3]); // reserved
                self.write_whole_buf(&mut buf.freeze()).await?;
            }
            FCGIType::Params { request_id, p } => {
                self.encode_kvp(request_id, Record::PARAMS, p).await?;
            }
            FCGIType::STDIN { request_id, data } => {
                self.encode_data(request_id, Record::STDIN, data).await?;
            }
            FCGIType::STDOUT { request_id, data } => {
                self.encode_data(request_id, Record::STDOUT, data).await?;
            }
            FCGIType::STDERR { request_id, data } => {
                self.encode_data(request_id, Record::STDERR, data).await?;
            }
            FCGIType::DATA { request_id, data } => {
                self.encode_data(request_id, Record::DATA, data).await?;
            }
            FCGIType::GetValues { p } => {
                self.encode_kvp(Record::MGMT_REQUEST_ID, Record::GET_VALUES, p).await?;
            }
            FCGIType::GetValuesResult { p } => {
                self.encode_kvp(Record::MGMT_REQUEST_ID, Record::GET_VALUES_RESULT, p).await?;
            }
        }
        Ok(())
    }
    async fn encode_kvp(&mut self, request_id: u16, rtype: u8, p: Vec<NameValuePair>) -> std::io::Result<()> {
        let mut kvps = self.kv_stream(request_id, rtype);

        for pair in p {
            kvps.add(pair).await?;
        }
        //write a header
        kvps.flush().await?;
        Ok(())
    }
    /// add val to the stream.
    /// will append and write a header if needed.
    /// will use `buf` to buffer small writes.
    /// `buf` might not be empty after the function is done. Use `end_stream` to flush the buffer.
    #[inline]
    async fn append_to_stream<B>(&mut self, mut val: B, buf: &mut BytesMut, request_id: u16, rtype: u8) -> std::io::Result<()>
        where B: Buf {
        while buf.len()+val.remaining() > BUF_LEN {
            let mut part = val.take(BUF_LEN - buf.len());
            Header::new(rtype,request_id, (BUF_LEN-HEADER_LEN) as u16)
                .write_into(&mut &mut buf[0..HEADER_LEN]);

            self.write_whole_buf(buf).await?;
            self.write_whole_buf(&mut part).await?;
            val = part.into_inner();
            
            buf.clear();
            buf.put_slice(&[0;8]);//reserve space for header
        }
        buf.put(val);
        Ok(())
    }
    /// write buf and padding and append an emty header to indicate the steams end
    #[inline]
    async fn end_stream(&mut self, mut buf: BytesMut, request_id: u16, rtype: u8) -> std::io::Result<()> {
        if buf.len()-HEADER_LEN > 0 {
            //write whats left
            let last_head = Header::new(rtype,request_id,
                (buf.len()-HEADER_LEN) as u16);
            let pad = last_head.get_padding() as usize;
            last_head.write_into(&mut &mut buf[0..HEADER_LEN]);
            let mut buf = buf.freeze();
            trace!("..with header: {:?}", buf);
            let mut pad = buf.slice(0..pad);
            self.write_whole_buf(&mut buf).await?;
            //padding
            self.write_whole_buf(&mut pad).await?;
        }
        //empty record
        let mut end = BytesMut::with_capacity(HEADER_LEN);
        Header::new(rtype,request_id,0).write_into(&mut end);
        self.write_whole_buf(&mut end).await?;
        Ok(())
    }
    /// write `data` to the stream.
    async fn encode_data(&mut self, request_id: u16, rtype: u8, mut data: Bytes) -> std::io::Result<()> {
        let mut buf = BytesMut::with_capacity(BUF_LEN);
        buf.put_slice(&[0;8]);//reserve space for header
        self.append_to_stream(&mut data, &mut buf,request_id, rtype).await?;
        self.end_stream(buf,request_id, rtype).await?;
        Ok(())
    }
    #[cfg(feature = "web_server")]
    /// Write `body` to the stream
    pub async fn data_stream<B>(&mut self, mut body: B, request_id: u16, rtype: u8, mut len: usize) -> std::io::Result<()>
    where B: Body+Unpin {
        let mut buf = BytesMut::with_capacity(BUF_LEN);
        buf.put_slice(&[0;8]);//reserve space for header
        while let Some(chunk) = body.data().await {
            if let Ok(mut b) = chunk { //b: Buf
                let val = b.copy_to_bytes(b.remaining());
                len = len.saturating_sub(val.len());
                self.append_to_stream(val, &mut buf, request_id, rtype).await?;
            }
        }
                
        if len > 0 {
            //fix broken connection?
            let a = FCGIType::AbortRequest { request_id };
            self.encode(a).await?;
            return Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted, 
                "body too short"));
        }
        self.end_stream(buf,request_id, rtype).await?;
        Ok(())
    }
    /// Create a NameValuePairWriter for this stream in order to write key value pairs (like PARAMS).
    /// ```
    /// let mut kvw = fcgi_writer.kv_stream(1, fastcgi::Record::PARAMS);
    /// kvw.add_kv(b"QUERY_STRING", b"").await?;
    /// kvw.flush().await?;
    /// ```
    /// See `encode_kvp`
    pub fn kv_stream(&mut self, request_id: u16, rtype: u8) -> NameValuePairWriter<W> {
        let mut buf = BytesMut::with_capacity(BUF_LEN);
        buf.put_slice(&[0;8]);//reserve space for header
        NameValuePairWriter {
            w: self,
            request_id,
            rtype,
            buf
        }
    }
}
/*
impl <W: AsyncRead>FCGIWriter<W> {
    fn decode(
        &mut self,
        src: &mut BytesMut
    ) -> std::io::Result<Option<FCGIType>> {
        //remove parsed stuff from src
        //allocate more space in src before returning
        //Ok(RecordReader.read())
        /*match type {
            out|err|in => //return chunks,
            kvpairs => //return pairs -> merge multiple records if needed,
            _ => //return whole Records only,
        }
        
        */
        Ok(None)
    }
}
*/

/// Writes Key Value Pairs to the stream
/// Uses an internal write buffer
pub struct NameValuePairWriter<'a, R>{
    w: & 'a mut FCGIWriter<R>,
    request_id: u16,
    rtype: u8,
    buf: BytesMut
}
impl <R: AsyncWrite+Unpin>NameValuePairWriter<'_, R> {
    /// write all remaining data to the stream
    pub async fn flush(self) -> std::io::Result<()> {
        //write a header
        self.w.end_stream(self.buf,self.request_id, self.rtype).await?;
        Ok(())
    }
    /// add all Key Value Pairs to the stream
    /// Panics if a Key or Value is bigger than 0x7fffffff bytes
    pub async fn extend<T: IntoIterator<Item = (P1,P2)>, P1: Buf, P2: Buf>(&mut self, iter: T) -> std::io::Result<()> {
        for (k,v) in iter {
            self.add_kv(k,v).await?;
        }
        Ok(())
    }
    #[inline]
    /// add a Key Value Pair to the stream
    /// Panics if Key or Value is bigger than 0x7fffffff bytes
    pub async fn add(&mut self, mut pair: NameValuePair) -> std::io::Result<()> {
        self.add_kv(&mut pair.name_data, &mut pair.value_data).await
    }
    #[inline]
    /// add a Key Value Pair to the stream
    /// Panics if Key or Value is bigger than 0x7fffffff bytes
    pub async fn add_kv<B1,B2>(&mut self, mut name: B1, mut val: B2) -> std::io::Result<()>
        where B1: Buf, B2: Buf {
        let mut ln = name.remaining();
        let mut lv = val.remaining();
        let mut lf: usize = 2;
        if ln > 0x7f {
            if ln > 0x7fffffff {
                panic!();
            }
            lf +=3;
            ln |= 0x8000;
        }
        if lv > 0x7f {
            if lv > 0x7fffffff {
                panic!();
            }
            lf +=3;
            lv |= 0x8000;
        }
        // check if size info fits into buf
        if self.buf.len() + lf > BUF_LEN {
            //No -> write buf with a header
            Header::new(self.rtype,
                self.request_id,
                (self.buf.len()-HEADER_LEN) as u16)
                .write_into(&mut &mut self.buf[0..HEADER_LEN]);

            self.w.write_whole_buf(&mut self.buf).await?;
            self.buf.clear();
            self.buf.put_slice(&[0;8]);//reserve space for header
        }

        if ln > 0x7f {
            self.buf.put_u32(ln as u32);
        }else{
            self.buf.put_u8(ln as u8);
        }
        if lv > 0x7f {
            self.buf.put_u32(lv as u32);
        }else{
            self.buf.put_u8(lv as u8);
        }
        
        self.w.append_to_stream(&mut name, &mut self.buf,self.request_id, self.rtype).await?;

        self.w.append_to_stream(&mut val, &mut self.buf,self.request_id, self.rtype).await?;
        Ok(())
    }
}