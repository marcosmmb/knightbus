use crate::codec::{FrameCursor, decode_owned_transfer, frame_len};
use crate::*;
use std::io::{self, Read, Write};

/// Bounded incremental decoder for a single frame. Returns unconsumed coalesced
/// bytes to its caller by count. Construct a fresh decoder for the next frame,
/// choosing the next phase/request context explicitly. Fatal errors poison it.
#[derive(Debug)]
pub struct Decoder {
    context: Context,
    limits: Limits,
    bytes: Vec<u8>,
    transfer_header: Option<[u8; TRANSFER_HEADER_LEN]>,
    needed: usize,
    framing: FrameCursor,
    done: bool,
    poisoned: bool,
}
impl Decoder {
    /// Create an empty decoder. No allocation occurs until input arrives.
    pub fn new(context: Context, limits: Limits) -> Self {
        Self {
            context,
            limits,
            bytes: Vec::new(),
            transfer_header: None,
            needed: 0,
            framing: FrameCursor::default(),
            done: false,
            poisoned: false,
        }
    }
    /// Consume as little input as needed for one frame; report consumed bytes.
    /// An incomplete frame returns `None`. Reuse after completion is rejected.
    pub fn push(&mut self, input: &[u8]) -> Result<(usize, Option<Message>), Error> {
        if self.poisoned || self.done {
            return Err(Error::Poisoned);
        }
        match self.push_inner(input) {
            Ok(result) => Ok(result),
            Err(e) => {
                self.poisoned = true;
                Err(e)
            }
        }
    }
    fn push_inner(&mut self, input: &[u8]) -> Result<(usize, Option<Message>), Error> {
        let mut used = 0;
        loop {
            if self.bytes.len() < self.needed {
                let n = (self.needed - self.bytes.len()).min(input.len() - used);
                if n == 0 {
                    return Ok((used, None));
                }
                self.bytes
                    .try_reserve_exact(self.needed - self.bytes.len())
                    .map_err(|_| Error::Allocation)?;
                self.bytes.extend_from_slice(&input[used..used + n]);
                used += n;
                if self.bytes.len() < self.needed {
                    return Ok((used, None));
                }
            }
            if let Some(header) = self.transfer_header {
                let message = decode_owned_transfer(
                    &header,
                    std::mem::take(&mut self.bytes),
                    self.context,
                    self.limits,
                )?;
                self.transfer_header = None;
                self.done = true;
                return Ok((used, Some(message)));
            }
            match frame_len(&self.bytes, self.context, self.limits, &mut self.framing) {
                Ok(n) => {
                    debug_assert_eq!(n, self.bytes.len());
                    let (message, _) = decode(&self.bytes, self.context, self.limits)?;
                    self.done = true;
                    self.bytes = Vec::new();
                    return Ok((used, Some(message)));
                }
                Err(Error::Incomplete(n)) => {
                    if self.context != Context::Management
                        && self.bytes.len() == TRANSFER_HEADER_LEN
                    {
                        self.transfer_header = Some(self.bytes[..].try_into().unwrap());
                        self.bytes.clear();
                        self.needed = n - TRANSFER_HEADER_LEN;
                    } else {
                        self.needed = n;
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }
    /// Declare EOF. Empty input is a clean EOF; a partial frame is truncated.
    pub fn finish(&self) -> Result<(), Error> {
        if self.poisoned {
            Err(Error::Poisoned)
        } else if self.done || self.buffered_len() == 0 {
            Ok(())
        } else {
            Err(Error::Incomplete(self.needed + self.header_len()))
        }
    }
    /// Received frame bytes currently buffered, including any saved header.
    /// This is not allocated memory: use [`Self::allocated_capacity`] for the
    /// decoder's heap buffer capacity.
    pub fn buffered_len(&self) -> usize {
        self.bytes.len() + self.header_len()
    }
    /// Current heap buffer capacity in bytes, including unused reserved space.
    /// Excludes the inline decoder/header, allocator overhead, temporary parsing
    /// allocations and returned messages. Completion releases or transfers the
    /// buffer; callers must account for returned message allocations separately.
    /// This reports an existing allocation, not a budget enforced before reserving.
    pub fn allocated_capacity(&self) -> usize {
        self.bytes.capacity()
    }
    fn header_len(&self) -> usize {
        if self.transfer_header.is_some() {
            TRANSFER_HEADER_LEN
        } else {
            0
        }
    }
}
fn io_error(error: Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

/// Read one frame from any blocking byte reader, with no socket assumptions.
/// Returns `None` only for EOF before the first byte. Interrupted reads retry;
/// WouldBlock is propagated: use [`Decoder`] for nonblocking transports.
/// Reads directly into the frame buffer up to each validated framing boundary.
/// The caller owns deadlines and should discard this adapter on any error.
pub fn read_message(
    reader: &mut impl Read,
    context: Context,
    limits: Limits,
) -> io::Result<Option<Message>> {
    let mut decoder = Decoder::new(context, limits);
    loop {
        let (_, message) = decoder.push(&[]).map_err(io_error)?;
        if message.is_some() {
            return Ok(message);
        }
        let mut filled = decoder.bytes.len();
        decoder
            .bytes
            .try_reserve_exact(decoder.needed - filled)
            .map_err(|_| io_error(Error::Allocation))?;
        // Initialize once per framing stage, then fill the remaining slice on
        // short reads. No scratch copy or repeated zeroing of an unread tail.
        decoder.bytes.resize(decoder.needed, 0);
        while filled < decoder.needed {
            match reader.read(&mut decoder.bytes[filled..]) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
                Ok(0) if filled == 0 && decoder.transfer_header.is_none() => return Ok(None),
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "truncated USB/IP frame",
                    ));
                }
                Ok(n) => filled += n,
            }
        }
    }
}
/// Encode, then write all bytes, correctly retrying short/interrupted writes.
/// Does not flush. After an I/O error a prefix may have been written: close the
/// stream rather than retrying the whole message. Nonblocking callers should
/// use [`encode`] and track their own write offset.
pub fn write_message(writer: &mut impl Write, message: &Message, limits: Limits) -> io::Result<()> {
    writer.write_all(&encode(message, limits).map_err(io_error)?)
}
