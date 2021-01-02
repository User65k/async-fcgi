use std::collections::VecDeque;
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
    fn chunk(&self) -> &[u8] {
        self.bufs.front().map(Buf::chunk).unwrap_or_default()
    }

    #[inline]
    fn advance(&mut self, mut cnt: usize) {
        while cnt > 0 {
            if let Some(front) = &mut self.bufs.front_mut() {
                let rem = front.remaining();
                if rem > cnt {
                    front.advance(cnt);
                    return;
                } else {
                    front.advance(rem);
                    cnt -= rem;
                }
            }else{
                //no data -> panic?
                return;
            }
            self.bufs.pop_front();
        }
    }

}