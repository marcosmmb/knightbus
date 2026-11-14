use crate::{Error, Result};
use std::sync::{Arc, Mutex};

/// Aggregate live storage and native request limits, shared across sessions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceLimits {
    /// Charged allocation capacity, including queued completions and scratch.
    pub bytes: usize,
    /// Prepared or submitted native requests, released only on retirement.
    pub requests: usize,
}
/// Current usage; returning a completion does not release its byte charge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// Live charged bytes (excludes allocator overhead).
    pub bytes: usize,
    /// Prepared or unretired native operations.
    pub requests: usize,
}
#[derive(Debug)]
struct State {
    limits: ResourceLimits,
    usage: Usage,
}
/// Shared admission authority. Cloning shares limits, never creates new credits.
#[derive(Clone, Debug)]
pub struct Budget(Arc<Mutex<State>>);
impl Budget {
    /// Reserve byte credits for externally allocated backend metadata. Reserve
    /// before allocation and keep the guard until that storage actually retires.
    /// The caller must include capacity and overlapping replacement allocations.
    pub fn reserve_memory(&self, bytes: usize) -> Result<MemoryReservation> {
        self.reserve(bytes, 0).map(MemoryReservation)
    }
    /// Construct a budget. Zero limits intentionally disable data admission.
    pub fn new(limits: ResourceLimits) -> Self {
        Self(Arc::new(Mutex::new(State {
            limits,
            usage: Usage::default(),
        })))
    }
    /// Inspect charged live storage and native operations.
    pub fn usage(&self) -> Usage {
        self.0.lock().expect("budget lock").usage
    }
    pub(crate) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub(crate) fn reserve(&self, bytes: usize, requests: usize) -> Result<Credit> {
        let mut s = self.0.lock().expect("budget lock");
        let b = s.usage.bytes.checked_add(bytes).ok_or(Error::Exhausted)?;
        let r = s
            .usage
            .requests
            .checked_add(requests)
            .ok_or(Error::Exhausted)?;
        if b > s.limits.bytes || r > s.limits.requests {
            return Err(Error::Exhausted);
        }
        s.usage = Usage {
            bytes: b,
            requests: r,
        };
        Ok(Credit {
            budget: self.clone(),
            bytes,
            requests,
        })
    }
    /// Allocate initialized scratch/transport storage after aggregate admission.
    /// Keep this owner alive until all native or transport access has retired.
    pub fn buffer(&self, length: usize) -> Result<Buffer> {
        let mut credit = self.reserve(length, 0)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| Error::Exhausted)?;
        // Account for allocator-reported spare capacity before exposing storage.
        credit.grow(bytes.capacity() - length)?;
        bytes.resize(length, 0);
        Ok(Buffer {
            bytes,
            _credit: credit,
        })
    }
}

/// RAII credits for backend-owned metadata or native storage. Dropping the guard
/// asserts the corresponding allocation can no longer be used by callbacks.
#[derive(Debug)]
pub struct MemoryReservation(Credit);
impl MemoryReservation {
    /// Bytes charged to the shared budget.
    pub fn bytes(&self) -> usize {
        self.0.bytes
    }
}
#[derive(Debug)]
pub(crate) struct Credit {
    pub(crate) budget: Budget,
    bytes: usize,
    requests: usize,
}
impl Credit {
    pub(crate) fn grow(&mut self, bytes: usize) -> Result<()> {
        let mut extra = self.budget.reserve(bytes, 0)?;
        self.bytes += bytes;
        extra.bytes = 0;
        Ok(())
    }
    pub(crate) fn retire(&mut self) {
        let mut s = self.budget.0.lock().expect("budget lock");
        s.usage.requests -= self.requests;
        self.requests = 0;
    }
}
impl Drop for Credit {
    fn drop(&mut self) {
        let mut s = self.budget.0.lock().expect("budget lock");
        s.usage.bytes -= self.bytes;
        s.usage.requests -= self.requests;
    }
}
/// Owned, non-cloneable buffer with capacity credits. Fields cannot be resized.
#[derive(Debug)]
pub struct Buffer {
    bytes: Vec<u8>,
    _credit: Credit,
}
impl Buffer {
    pub(crate) fn budget_matches(&self, budget: &Budget) -> bool {
        self._credit.budget.same(budget)
    }
    /// Initialized bytes; never expose uninitialized allocation capacity.
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }
    /// Mutable initialized storage, available only through exclusive ownership.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.bytes
    }
    /// Actual reserved capacity charged to the aggregate budget.
    pub fn capacity(&self) -> usize {
        self.bytes.capacity()
    }
}

impl Budget {
    /// Start an incremental wire decoder backed by this aggregate byte budget.
    /// Input slices remain the transport's separately charged storage.
    pub fn decoder(
        &self,
        context: crate::usbip::Context,
        limits: crate::usbip::Limits,
    ) -> Result<IncomingDecoder> {
        Ok(IncomingDecoder {
            decoder: crate::usbip::Decoder::new(context, limits),
            credit: Some(self.reserve(std::mem::size_of::<IncomingDecoder>(), 0)?),
        })
    }
    /// Encode to an owned, charged transport buffer. The source message's storage
    /// remains separately charged until its owner is released.
    pub fn encode<'a>(
        &self,
        message: impl Into<crate::usbip::Frame<'a>>,
        limits: crate::usbip::Limits,
    ) -> std::result::Result<Buffer, crate::usbip::Error> {
        let mut credit = self
            .reserve(0, 0)
            .map_err(|_| crate::usbip::Error::Limit("aggregate bytes"))?;
        let bytes = crate::usbip::encode(message, limits, &mut |n| admit(&mut credit, n))?;
        Ok(Buffer {
            bytes,
            _credit: credit,
        })
    }
}
fn admit(credit: &mut Credit, bytes: usize) -> std::result::Result<(), crate::usbip::Error> {
    credit
        .grow(bytes)
        .map_err(|_| crate::usbip::Error::Limit("aggregate bytes"))
}
impl Credit {
    // Move a portion of an existing charge with its storage; never return credits
    // to the shared budget between decoder output and native ownership.
    fn split(&mut self, bytes: usize) -> Self {
        assert!(bytes <= self.bytes);
        self.bytes -= bytes;
        Self {
            budget: self.budget.clone(),
            bytes,
            requests: 0,
        }
    }
}
/// Single-frame decoder retaining all allocation charges through errors and EOF.
/// Charges conservatively include temporary/reallocation storage until the decoded
/// owner is consumed or dropped. Drop this object after a fatal decoding error.
pub struct IncomingDecoder {
    decoder: crate::usbip::Decoder,
    credit: Option<Credit>,
}
impl IncomingDecoder {
    /// Consume one frame prefix with pre-allocation admission. Returns the number
    /// of consumed bytes and an owner carrying the decoded message's byte credits.
    pub fn push(
        &mut self,
        input: &[u8],
    ) -> std::result::Result<(usize, Option<DecodedMessage>), crate::usbip::Error> {
        let credit = self.credit.as_mut().ok_or(crate::usbip::Error::Poisoned)?;
        let (used, message) = self.decoder.push(input, &mut |n| admit(credit, n))?;
        Ok((
            used,
            message.map(|message| DecodedMessage {
                message,
                credit: self.credit.take().unwrap(),
            }),
        ))
    }
    /// Declare EOF; rejects truncated or poisoned frames.
    pub fn finish(&self) -> std::result::Result<(), crate::usbip::Error> {
        self.decoder.finish()
    }
}
/// Decoded wire message whose allocation credits cannot be detached from storage.
/// Field order ensures message storage is dropped before its charge.
pub struct DecodedMessage {
    message: crate::usbip::Message,
    credit: Credit,
}
impl DecodedMessage {
    /// Inspect validated wire fields without extracting uncharged storage.
    pub fn message(&self) -> &crate::usbip::Message {
        &self.message
    }
    /// Convert a SUBMIT into native ownership, moving OUT payload storage without
    /// copying. IN requests allocate their native receive buffer under admission.
    /// ISO metadata copies are admitted while the original decoded arrays remain
    /// charged. Unsupported message kinds are rejected and released.
    pub fn into_transfer(self) -> Result<crate::Transfer> {
        let Self {
            message,
            mut credit,
        } = self;
        let submit = match message {
            crate::usbip::Message::Submit(submit) => submit,
            other => {
                drop(other);
                return Err(Error::Invalid);
            }
        };
        let (header, data, layout) = submit.into_parts();
        let budget = credit.budget.clone();
        if header.direction == crate::usbip::Direction::Out {
            let charge = credit.split(data.capacity());
            let buffer = Buffer {
                bytes: data,
                _credit: charge,
            };
            crate::Transfer::from_buffer(&budget, header, &layout, buffer)
        } else {
            crate::Transfer::new(&budget, header, &layout)
        }
    }
}
