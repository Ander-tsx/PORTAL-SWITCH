// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Primitive readers and writers (little-endian, `protocol/v1.md` §4).

use crate::Error;

pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    msg: u16,
}

impl<'a> Reader<'a> {
    pub(crate) const fn new(buf: &'a [u8], msg: u16) -> Self {
        Self { buf, pos: 0, msg }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or(Error::ShortPayload(self.msg))?;
        let slice = self
            .buf
            .get(self.pos..end)
            .ok_or(Error::ShortPayload(self.msg))?;
        self.pos = end;
        Ok(slice)
    }

    pub(crate) const fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let mut out = [0; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.array::<1>()?[0])
    }
    pub(crate) fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    pub(crate) fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    pub(crate) fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    pub(crate) fn i16(&mut self) -> Result<i16, Error> {
        Ok(i16::from_le_bytes(self.array()?))
    }
    pub(crate) fn i32(&mut self) -> Result<i32, Error> {
        Ok(i32::from_le_bytes(self.array()?))
    }
    pub(crate) fn bytes16(&mut self) -> Result<[u8; 16], Error> {
        self.array()
    }
    pub(crate) fn bytes32(&mut self) -> Result<[u8; 32], Error> {
        self.array()
    }

    pub(crate) fn bool(&mut self) -> Result<bool, Error> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(Error::InvalidBool(other)),
        }
    }

    pub(crate) fn str(&mut self, field: &'static str, max: usize) -> Result<String, Error> {
        let len = usize::from(self.u16()?);
        if len > max {
            return Err(Error::LimitExceeded { field, max });
        }
        let raw = self.take(len)?;
        String::from_utf8(raw.to_vec()).map_err(|_| Error::InvalidUtf8)
    }

    pub(crate) fn blob(&mut self, field: &'static str, max: usize) -> Result<Vec<u8>, Error> {
        let len = self.u32()? as usize;
        if len > max {
            return Err(Error::LimitExceeded { field, max });
        }
        Ok(self.take(len)?.to_vec())
    }
}

pub(crate) struct Writer<'a> {
    out: &'a mut Vec<u8>,
}

impl<'a> Writer<'a> {
    pub(crate) fn new(out: &'a mut Vec<u8>) -> Self {
        Self { out }
    }

    pub(crate) fn u8(&mut self, v: u8) {
        self.out.push(v);
    }
    pub(crate) fn u16(&mut self, v: u16) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }
    pub(crate) fn u32(&mut self, v: u32) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }
    pub(crate) fn u64(&mut self, v: u64) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }
    pub(crate) fn i16(&mut self, v: i16) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }
    pub(crate) fn i32(&mut self, v: i32) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }
    pub(crate) fn raw(&mut self, v: &[u8]) {
        self.out.extend_from_slice(v);
    }
    pub(crate) fn bool(&mut self, v: bool) {
        self.u8(u8::from(v));
    }

    pub(crate) fn str(&mut self, field: &'static str, max: usize, v: &str) -> Result<(), Error> {
        if v.len() > max {
            return Err(Error::LimitExceeded { field, max });
        }
        // `max` is far below `u16::MAX`, so the cast cannot truncate.
        #[allow(clippy::cast_possible_truncation)]
        self.u16(v.len() as u16);
        self.raw(v.as_bytes());
        Ok(())
    }

    pub(crate) fn blob(&mut self, field: &'static str, max: usize, v: &[u8]) -> Result<(), Error> {
        if v.len() > max {
            return Err(Error::LimitExceeded { field, max });
        }
        #[allow(clippy::cast_possible_truncation)]
        self.u32(v.len() as u32);
        self.raw(v);
        Ok(())
    }
}
