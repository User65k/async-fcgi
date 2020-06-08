use std::collections::VecDeque;
use std::io::IoSlice;
use log::{info};
use bytes::Buf;

pub(crate) struct BufList<T> {
    bufs: VecDeque<T>,
}

impl<T: Buf> BufList<T> {
    pub(crate) fn new() -> BufList<T> {
        BufList {
            bufs: VecDeque::new(),
        }
    }

    #[inline]
    pub(crate) fn push(&mut self, buf: T) {
        debug_assert!(buf.has_remaining());
        self.bufs.push_back(buf);
    }
/*
    #[inline]
    pub(crate) fn bufs_cnt(&self) -> usize {
        self.bufs.len()
    }*/
    #[inline]
    pub(crate) fn append(&mut self, mut other: BufList<T>) {
        self.bufs.append(&mut other.bufs)
    }
    #[inline]
    pub(crate) fn oldest(&mut self) -> Option<T> {
        self.bufs.pop_front()
    }
}

impl<T: Buf> Buf for BufList<T> {
    #[inline]
    fn remaining(&self) -> usize {
        self.bufs.iter().map(|buf| buf.remaining()).sum()
    }

    #[inline]
    fn bytes(&self) -> &[u8] {
        info!("bytes");
        self.bufs.front().map(Buf::bytes).unwrap_or_default()
    }

    #[inline]
    fn advance(&mut self, mut cnt: usize) {
        while cnt > 0 {
            {
                let front = &mut self.bufs[0];
                let rem = front.remaining();
                if rem > cnt {
                    front.advance(cnt);
                    return;
                } else {
                    front.advance(rem);
                    cnt -= rem;
                }
            }
            self.bufs.pop_front();
        }
    }

    #[inline]
    fn bytes_vectored<'t>(&'t self, dst: &mut [IoSlice<'t>]) -> usize {
        info!("vec");
        if dst.is_empty() {
            return 0;
        }
        let mut vecs = 0;
        for buf in &self.bufs {
            vecs += buf.bytes_vectored(&mut dst[vecs..]);
            if vecs == dst.len() {
                break;
            }
        }
        vecs
    }
}