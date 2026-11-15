//! Independent fixtures, hostile inputs, and stream-boundary tests.
use knightbus_usbip::*;
use std::io::{self, Cursor, Read, Write};

fn fixture(name: &str) -> Vec<u8> {
    // Match the section marker regardless of LF or CRLF checkout line endings.
    let header = format!("[{name}]");
    let (_, text) = include_str!("fixtures.txt").split_once(&header).unwrap();
    let hex: String = text.split('[').next().unwrap().split_whitespace().collect();
    assert_eq!(hex.len() % 2, 0, "odd hex length in {name}");
    hex.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
        .collect()
}
fn request(direction: Direction, length: u32, packets: i32) -> ReplyContext {
    ReplyContext {
        seqnum: 0x10203040,
        direction,
        transfer_buffer_length: length,
        number_of_packets: packets,
    }
}
fn cases() -> Vec<(&'static str, Context)> {
    let mut v = Vec::new();
    for name in [
        "devlist-request",
        "devlist-empty",
        "devlist-error",
        "devlist-two",
        "import-request",
        "import-success",
        "import-error",
    ] {
        v.push((name, Context::Management));
    }
    for name in [
        "control-in",
        "control-out",
        "control-zero",
        "bulk-in",
        "bulk-out",
        "interrupt-in",
        "interrupt-out",
        "iso-in",
        "iso-out",
        "unlink",
    ] {
        v.push((name, Context::Requests));
    }
    for (name, dir, len, count) in [
        ("return-in-short", Direction::In, 64, -1),
        ("return-out", Direction::Out, 64, -1),
        ("return-stall", Direction::In, 64, -1),
        ("return-iso-in", Direction::In, 8, 2),
        ("return-iso-out", Direction::Out, 5, 2),
    ] {
        v.push((name, Context::Replies(Some(request(dir, len, count)))));
    }
    for name in ["return-unlink-canceled", "return-unlink-too-late"] {
        v.push((name, Context::Replies(None)));
    }
    v
}
fn word(b: &mut [u8], pos: usize, value: u32) {
    b[pos..pos + 4].copy_from_slice(&value.to_be_bytes());
}

#[test]
fn every_independent_golden_vector_round_trips_byte_for_byte() {
    for (name, context) in cases() {
        let b = fixture(name);
        let (m, consumed) = decode(&b, context, Limits::default(), &mut |_| Ok(()))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(consumed, b.len(), "{name}");
        assert_eq!(
            encode(&m, Limits::default(), &mut |_| Ok(())).unwrap(),
            b,
            "{name}"
        );
    }
}
#[test]
fn borrowed_submit_preserves_golden_bytes_and_validation_before_admission() {
    for (name, context) in cases() {
        let bytes = fixture(name);
        let (message, _) = decode(&bytes, context, Limits::default(), &mut |_| Ok(())).unwrap();
        let Message::Submit(submit) = message else {
            continue;
        };
        let header = SubmitHeader::from(&submit);
        let mut admitted = 0;
        let encoded = encode(
            Frame::Submit {
                header: &header,
                data: &submit.data,
                packets: &submit.iso_packets,
            },
            Limits::default(),
            &mut |n| {
                admitted += n;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(encoded, bytes, "{name}");
        assert_eq!(admitted, encoded.capacity());
        assert!(matches!(
            encode(
                Frame::Submit {
                    header: &header,
                    data: &submit.data,
                    packets: &submit.iso_packets
                },
                Limits::default(),
                &mut |_| Err(Error::Limit("test budget"))
            ),
            Err(Error::Limit("test budget"))
        ));
        let mut invalid = header;
        invalid.endpoint = 16;
        let mut allocated = false;
        assert!(
            encode(
                Frame::Submit {
                    header: &invalid,
                    data: &submit.data,
                    packets: &submit.iso_packets
                },
                Limits::default(),
                &mut |_| {
                    allocated = true;
                    Ok(())
                }
            )
            .is_err()
        );
        assert!(
            encode(
                Frame::Submit {
                    header: &header,
                    data: &submit.data,
                    packets: &submit.iso_packets
                },
                Limits {
                    frame_bytes: bytes.len() - 1,
                    ..Limits::default()
                },
                &mut |_| {
                    allocated = true;
                    Ok(())
                }
            )
            .is_err()
        );
        assert!(!allocated, "invalid input must fail before allocation");
    }
}
#[test]
fn golden_values_have_correct_meaning_not_just_round_trips() {
    let limits = Limits::default();
    let (m, _) = decode(
        &fixture("devlist-two"),
        Context::Management,
        limits,
        &mut |_| Ok(()),
    )
    .unwrap();
    let Message::DevListReply { status, devices } = m else {
        panic!()
    };
    assert_eq!(status, 0);
    assert_eq!(devices.len(), 2);
    assert_eq!(
        devices[0].device.path.as_bytes(),
        b"/synthetic/knightbus/7-4"
    );
    assert_eq!(devices[0].device.bus_id.as_bytes(), b"7-4");
    assert_eq!(devices[0].device.vendor_id, 0x1234);
    assert_eq!(devices[0].device.product_id, 0xABCD);
    assert_eq!(devices[0].device.device_bcd, 0x102);
    assert_eq!(devices[0].device.bus_number, 7);
    assert_eq!(devices[0].device.device_number, 4);
    assert_eq!(devices[0].device.speed, 3);
    assert_eq!(devices[0].interfaces[1].class, 0xFF);
    assert!(devices[1].interfaces.is_empty());
    let (m, _) = decode(
        &fixture("control-in"),
        Context::Requests,
        limits,
        &mut |_| Ok(()),
    )
    .unwrap();
    let Message::Submit(s) = m else { panic!() };
    assert_eq!(s.setup, [0x80, 6, 0, 1, 0, 0, 18, 0]);
    assert_eq!(s.transfer_buffer_length, 18);
    assert_eq!(s.endpoint, 0);
    assert_eq!(s.device_id, 0x70004);
    assert!(s.data.is_empty());
    let context = Context::Replies(Some(request(Direction::In, 8, 2)));
    let (m, _) = decode(&fixture("return-iso-in"), context, limits, &mut |_| Ok(())).unwrap();
    let Message::ReturnSubmit(s) = m else {
        panic!()
    };
    assert_eq!(s.data, b"abc"); // Original buffer contains a gap; wire does not.
    assert_eq!(
        s.iso_packets[1],
        IsoPacket {
            offset: 5,
            length: 3,
            actual_length: 1,
            status: -18
        }
    );
    assert_eq!(s.error_count, 1);
    assert_eq!(s.start_frame, 27);
}
#[test]
fn every_split_and_every_truncated_prefix() {
    for (name, context) in cases() {
        let b = fixture(name);
        for split in 0..b.len() {
            assert!(
                matches!(
                    decode(&b[..split], context, Limits::default(), &mut |_| Ok(())),
                    Err(Error::Incomplete(_))
                ),
                "{name}/{split}"
            );
            let mut d = Decoder::new(context, Limits::default());
            let (n, m) = d.push(&b[..split], &mut |_| Ok(())).unwrap();
            assert_eq!(n, split);
            assert!(m.is_none());
            if split > 0 {
                assert!(d.finish().is_err());
            }
            let (n, m) = d.push(&b[split..], &mut |_| Ok(())).unwrap();
            assert_eq!(n, b.len() - split);
            assert!(m.is_some());
            assert!(d.finish().is_ok());
        }
    }
}
#[test]
fn byte_at_a_time_and_coalesced_frames() {
    for (name, context) in cases() {
        let b = fixture(name);
        let mut d = Decoder::new(context, Limits::default());
        for (i, byte) in b.iter().enumerate() {
            let (n, m) = d.push(&[*byte], &mut |_| Ok(())).unwrap();
            assert_eq!(n, 1);
            assert_eq!(m.is_some(), i + 1 == b.len());
            assert!(d.buffered_len() <= Limits::default().frame_bytes);
        }
        let joined = [b.as_slice(), b.as_slice(), &[0x99]].concat();
        let mut d = Decoder::new(context, Limits::default());
        let (n, m) = d.push(&joined, &mut |_| Ok(())).unwrap();
        assert_eq!(n, b.len());
        assert!(m.is_some());
        let (_, consumed) =
            decode(&joined[n..], context, Limits::default(), &mut |_| Ok(())).unwrap();
        assert_eq!(consumed, b.len());
        assert_eq!(joined[n + consumed], 0x99);
    }
}
struct Choppy<T> {
    inner: T,
    interrupted: bool,
}
impl<T: Read> Read for Choppy<T> {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.interrupted = !self.interrupted;
        if self.interrupted {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let n = b.len().min(3);
        self.inner.read(&mut b[..n])
    }
}
impl<T: Write> Write for Choppy<T> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.interrupted = !self.interrupted;
        if self.interrupted {
            return Err(io::ErrorKind::Interrupted.into());
        }
        self.inner.write(&b[..b.len().min(3)])
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
#[test]
fn short_interrupted_reads_writes_eof_and_no_overread() {
    for (name, context) in cases() {
        let b = fixture(name);
        let joined = [b.as_slice(), b.as_slice()].concat();
        let mut reader = Choppy {
            inner: Cursor::new(joined),
            interrupted: false,
        };
        let m = read_message(&mut reader, context, Limits::default(), &mut |_| Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!(reader.inner.position(), b.len() as u64);
        let mut writer = Choppy {
            inner: Vec::new(),
            interrupted: false,
        };
        write_message(&mut writer, &m, Limits::default(), &mut |_| Ok(())).unwrap();
        assert_eq!(writer.inner, b);
        assert!(
            read_message(&mut reader, context, Limits::default(), &mut |_| Ok(()))
                .unwrap()
                .is_some()
        );
        assert!(
            read_message(&mut reader, context, Limits::default(), &mut |_| Ok(()))
                .unwrap()
                .is_none()
        );
        for end in 1..b.len() {
            let e = read_message(
                &mut Cursor::new(&b[..end]),
                context,
                Limits::default(),
                &mut |_| Ok(()),
            )
            .unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof, "{name}/{end}");
        }
    }
}
#[test]
fn blocking_reads_use_the_full_validated_body_without_overreading() {
    struct CountingReader {
        inner: Cursor<Vec<u8>>,
        requests: Vec<usize>,
        chunk_limit: usize,
    }
    impl Read for CountingReader {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            self.requests.push(b.len());
            let n = b.len().min(self.chunk_limit);
            self.inner.read(&mut b[..n])
        }
    }

    let limits = Limits::default();
    let (Message::Submit(mut submit), _) =
        decode(&fixture("bulk-out"), Context::Requests, limits, &mut |_| {
            Ok(())
        })
        .unwrap()
    else {
        panic!();
    };
    let length = 1024 * 1024;
    submit.transfer_buffer_length = length as u32;
    submit.data = (0..length).map(|i| i as u8).collect();
    let expected = Message::Submit(submit);
    let bytes = encode(&expected, limits, &mut |_| Ok(())).unwrap();
    for chunk_limit in [usize::MAX, 16381] {
        let mut reader = CountingReader {
            inner: Cursor::new([bytes.as_slice(), &[0xAA, 0xBB]].concat()),
            requests: Vec::new(),
            chunk_limit,
        };
        assert_eq!(
            read_message(&mut reader, Context::Requests, limits, &mut |_| Ok(())).unwrap(),
            Some(expected.clone())
        );
        assert_eq!(reader.inner.position(), bytes.len() as u64);
        assert_eq!(reader.requests[1], length);
        assert_eq!(reader.requests.len(), 1 + length.div_ceil(chunk_limit));
    }
    let error = read_message(
        &mut Cursor::new(&bytes[..bytes.len() - 1]),
        Context::Requests,
        limits,
        &mut |_| Ok(()),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);

    // An oversized claim must fail after the header, before any body read.
    let mut oversized = bytes;
    word(&mut oversized, 24, length as u32 + 1);
    let mut reader = CountingReader {
        inner: Cursor::new(oversized),
        requests: Vec::new(),
        chunk_limit: usize::MAX,
    };
    let error = read_message(&mut reader, Context::Requests, limits, &mut |_| Ok(())).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(reader.requests, [48]);
}

#[test]
fn decoder_capacity_accounts_for_reserved_body_until_handoff() {
    let limits = Limits::default();
    let length = limits.transfer_bytes;
    let mut bytes = fixture("bulk-out");
    word(&mut bytes, 24, length as u32);
    bytes.resize(TRANSFER_HEADER_LEN + length, 0);

    let mut decoder = Decoder::new(Context::Requests, limits);
    assert_eq!(decoder.buffered_len(), 0);
    assert_eq!(decoder.allocated_capacity(), 0);

    // A header plus one body byte reserves the entire declared 1 MiB body.
    let prefix = TRANSFER_HEADER_LEN + 1;
    assert_eq!(
        decoder.push(&bytes[..prefix], &mut |_| Ok(())).unwrap(),
        (prefix, None)
    );
    assert_eq!(decoder.buffered_len(), prefix);
    let reserved = decoder.allocated_capacity();
    assert!(reserved >= length);
    assert_eq!(decoder.finish(), Err(Error::Incomplete(bytes.len())));
    assert_eq!(decoder.allocated_capacity(), reserved);

    let (used, message) = decoder.push(&bytes[prefix..], &mut |_| Ok(())).unwrap();
    assert_eq!(used, bytes.len() - prefix);
    let Some(Message::Submit(submit)) = message else {
        panic!();
    };
    assert_eq!(submit.data.capacity(), reserved);
    assert_eq!(submit.data, bytes[TRANSFER_HEADER_LEN..]);
    assert_eq!(decoder.buffered_len(), 0);
    assert_eq!(decoder.allocated_capacity(), 0);
}

#[test]
fn streamed_payload_reuses_storage_without_retaining_excess_capacity() {
    struct BodyReader {
        inner: Cursor<Vec<u8>>,
        body_address: Option<usize>,
    }
    impl Read for BodyReader {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            if self.inner.position() == TRANSFER_HEADER_LEN as u64 {
                self.body_address = Some(bytes.as_ptr() as usize);
            }
            self.inner.read(bytes)
        }
    }
    // Small/bulk, full/sparse/empty ISO, and either side of the 1/8 spare limit.
    for (capacity, count, actual, reused) in [
        (7u32, 0, 7, false),
        (4096, 0, 4096, true),
        (32768, 32, 32768, true),
        (32768, 32, 1, false),
        (32768, 32, 0, false),
        (4096, 32, 4096, true),
        (4096, 32, 4095, false),
    ] {
        let mut remaining = actual;
        let packets: Vec<_> = (0..count)
            .map(|i| {
                let length = capacity / count;
                let transferred = remaining.min(length);
                remaining -= transferred;
                IsoPacket {
                    offset: i * length,
                    length,
                    actual_length: transferred,
                    status: if transferred == 0 { -18 } else { 0 },
                }
            })
            .collect();
        let request = request(Direction::In, capacity, count as i32);
        let expected = Message::ReturnSubmit(ReturnSubmit {
            request,
            status: 0,
            actual_length: actual,
            start_frame: 0,
            number_of_packets: count as i32,
            error_count: packets.iter().filter(|p| p.status != 0).count() as u32,
            data: vec![0xA5; actual as usize],
            iso_packets: packets,
        });
        let context = Context::Replies(Some(request));
        let limits = Limits::default();
        let bytes = encode(&expected, limits, &mut |_| Ok(())).unwrap();
        let mut reader = BodyReader {
            inner: Cursor::new([bytes.as_slice(), &[0x99]].concat()),
            body_address: None,
        };
        let message = read_message(&mut reader, context, limits, &mut |_| Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!(message, expected);
        assert_eq!(reader.inner.position(), bytes.len() as u64);
        let Message::ReturnSubmit(s) = message else {
            panic!()
        };
        assert!(s.data.capacity() - s.data.len() <= s.data.len() / 8);
        if !s.data.is_empty() {
            assert_eq!(
                Some(s.data.as_ptr() as usize) == reader.body_address,
                reused
            );
        }
        let mut decoder = Decoder::new(context, limits);
        assert_eq!(
            decoder.push(&bytes[..48], &mut |_| Ok(())).unwrap(),
            (48, None)
        );
        assert_eq!(decoder.buffered_len(), 48);
        assert_eq!(decoder.finish(), Err(Error::Incomplete(bytes.len())));
        let (used, message) = decoder
            .push(&[&bytes[48..], &[0x99]].concat(), &mut |_| Ok(()))
            .unwrap();
        assert_eq!(used, bytes.len() - 48);
        assert_eq!(message, Some(expected));
        assert_eq!(decoder.buffered_len(), 0);
        assert!(decoder.finish().is_ok());
    }
}

#[test]
fn write_zero_and_nonblocking_errors_are_not_retried_forever() {
    struct Zero;
    impl Write for Zero {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Ok(0)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    assert_eq!(
        write_message(
            &mut Zero,
            &Message::DevListRequest,
            Limits::default(),
            &mut |_| Ok(())
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::WriteZero
    );
    struct Blocked;
    impl Read for Blocked {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::WouldBlock.into())
        }
    }
    assert_eq!(
        read_message(
            &mut Blocked,
            Context::Management,
            Limits::default(),
            &mut |_| Ok(())
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::WouldBlock
    );
}
#[test]
fn malformed_management_fields_and_counts() {
    let mut b = fixture("devlist-request");
    b[0] = 2;
    assert!(decode(&b, Context::Management, Limits::default(), &mut |_| Ok(())).is_err());
    let mut b = fixture("devlist-request");
    b[3] = 0xFF;
    assert!(decode(&b, Context::Management, Limits::default(), &mut |_| Ok(())).is_err());
    let mut b = fixture("import-request");
    word(&mut b, 4, 1);
    assert!(decode(&b, Context::Management, Limits::default(), &mut |_| Ok(())).is_err());
    let mut b = fixture("import-request");
    b[8..40].fill(b'x');
    assert!(decode(&b, Context::Management, Limits::default(), &mut |_| Ok(())).is_err());
    let mut b = fixture("import-request");
    b[39] = b'x';
    assert!(decode(&b, Context::Management, Limits::default(), &mut |_| Ok(())).is_err());
    let mut b = fixture("devlist-two");
    b[327] = 1;
    assert!(decode(&b, Context::Management, Limits::default(), &mut |_| Ok(())).is_err());
    let mut b = fixture("devlist-empty");
    word(&mut b, 8, u32::MAX);
    assert!(matches!(
        decode(&b, Context::Management, Limits::default(), &mut |_| Ok(())),
        Err(Error::Limit(_))
    ));
    let limits = Limits {
        devices: usize::MAX,
        frame_bytes: 1000,
        ..Limits::default()
    };
    assert!(matches!(
        decode(&b, Context::Management, limits, &mut |_| Ok(())),
        Err(Error::Limit(_))
    ));
}
#[test]
fn malformed_transfer_fields_and_phase() {
    for (pos, value) in [(0, 9), (12, 2), (16, 16), (24, u32::MAX), (32, 0xFFFF_FFFE)] {
        let mut b = fixture("bulk-in");
        word(&mut b, pos, value);
        assert!(
            decode(&b, Context::Requests, Limits::default(), &mut |_| Ok(())).is_err(),
            "{pos}"
        );
    }
    assert!(
        decode(
            &fixture("bulk-in"),
            Context::Management,
            Limits::default(),
            &mut |_| Ok(())
        )
        .is_err()
    );
    assert!(
        decode(
            &fixture("bulk-in"),
            Context::Replies(None),
            Limits::default(),
            &mut |_| Ok(())
        )
        .is_err()
    );
    for (name, pos) in [("unlink", 12), ("unlink", 16), ("unlink", 24)] {
        let mut b = fixture(name);
        b[pos] = 1;
        assert!(decode(&b, Context::Requests, Limits::default(), &mut |_| Ok(())).is_err());
    }
    for pos in [8, 12, 16, 40] {
        let mut b = fixture("return-in-short");
        b[pos] = 1;
        assert!(
            decode(
                &b,
                Context::Replies(Some(request(Direction::In, 64, -1))),
                Limits::default(),
                &mut |_| Ok(())
            )
            .is_err()
        );
    }
}
#[test]
fn reply_requires_matching_original_context_and_respects_capacity() {
    let b = fixture("return-in-short");
    assert_eq!(
        decode(&b, Context::Replies(None), Limits::default(), &mut |_| Ok(
            ()
        ))
        .unwrap_err(),
        Error::Context(0x10203040)
    );
    let mut r = request(Direction::In, 64, -1);
    r.seqnum += 1;
    assert!(matches!(
        decode(
            &b,
            Context::Replies(Some(r)),
            Limits::default(),
            &mut |_| Ok(())
        ),
        Err(Error::Context(_))
    ));
    assert!(
        decode(
            &b,
            Context::Replies(Some(request(Direction::In, 2, -1))),
            Limits::default(),
            &mut |_| Ok(())
        )
        .is_err()
    );
    // Direction is zero in both wire headers; only original context decides framing.
    let b = fixture("return-out");
    assert!(
        decode(
            &b,
            Context::Replies(Some(request(Direction::Out, 64, 0))),
            Limits::default(),
            &mut |_| Ok(())
        )
        .is_ok()
    );
    assert!(matches!(
        decode(
            &b,
            Context::Replies(Some(request(Direction::In, 64, 0))),
            Limits::default(),
            &mut |_| Ok(())
        ),
        Err(Error::Incomplete(53))
    ));
}
#[test]
fn iso_ranges_counts_sums_and_error_counts_are_checked() {
    let context = Context::Replies(Some(request(Direction::In, 8, 2)));
    for (pos, val) in [
        (32, 1),
        (36, 3),
        (36, u32::MAX),
        (51, u32::MAX),
        (55, 9),
        (59, 4),
        (59, 0),
    ] {
        let mut b = fixture("return-iso-in");
        word(&mut b, pos, val);
        assert!(
            decode(&b, context, Limits::default(), &mut |_| Ok(())).is_err(),
            "{pos}/{val}"
        );
    }
    let mut b = fixture("iso-in");
    word(&mut b, 48, u32::MAX);
    assert!(decode(&b, Context::Requests, Limits::default(), &mut |_| Ok(())).is_err());
    let mut b = fixture("iso-in");
    word(&mut b, 32, i32::MAX as u32);
    assert!(matches!(
        decode(&b, Context::Requests, Limits::default(), &mut |_| Ok(())),
        Err(Error::Limit(_))
    ));
}
#[test]
fn strict_limits_apply_to_decode_encode_and_incremental_buffer() {
    for (name, context) in cases() {
        let b = fixture(name);
        let (m, _) = decode(&b, context, Limits::default(), &mut |_| Ok(())).unwrap();
        let limits = Limits {
            frame_bytes: b.len() - 1,
            ..Limits::default()
        };
        assert!(
            matches!(
                decode(&b, context, limits, &mut |_| Ok(())),
                Err(Error::Limit(_))
            ),
            "{name}"
        );
        assert!(
            matches!(encode(&m, limits, &mut |_| Ok(())), Err(Error::Limit(_))),
            "{name}"
        );
        let mut d = Decoder::new(context, limits);
        assert!(d.push(&b, &mut |_| Ok(())).is_err());
        assert!(d.buffered_len() <= limits.frame_bytes);
        assert_eq!(d.push(&[], &mut |_| Ok(())), Err(Error::Poisoned));
    }
    let limits = Limits {
        transfer_bytes: 6,
        ..Limits::default()
    };
    assert!(matches!(
        decode(
            &fixture("interrupt-in"),
            Context::Requests,
            limits,
            &mut |_| Ok(())
        ),
        Err(Error::Limit(_))
    ));
    let limits = Limits {
        iso_packets: 1,
        ..Limits::default()
    };
    assert!(matches!(
        decode(&fixture("iso-in"), Context::Requests, limits, &mut |_| Ok(
            ()
        )),
        Err(Error::Limit(_))
    ));
}
#[test]
fn invalid_outgoing_structures_never_serialize() {
    let l = Limits::default();
    let (m, _) = decode(&fixture("bulk-out"), Context::Requests, l, &mut |_| Ok(())).unwrap();
    let Message::Submit(mut s) = m else { panic!() };
    s.data.pop();
    assert!(encode(&Message::Submit(s.clone()), l, &mut |_| Ok(())).is_err());
    s.data.push(0);
    s.endpoint = 16;
    assert!(encode(&Message::Submit(s.clone()), l, &mut |_| Ok(())).is_err());
    s.endpoint = 1;
    s.number_of_packets = 1;
    assert!(encode(&Message::Submit(s), l, &mut |_| Ok(())).is_err());
    assert!(
        encode(
            &Message::ImportReply {
                status: 0,
                device: None
            },
            l,
            &mut |_| Ok(())
        )
        .is_err()
    );
    let (m, _) = decode(&fixture("devlist-two"), Context::Management, l, &mut |_| {
        Ok(())
    })
    .unwrap();
    let Message::DevListReply { mut devices, .. } = m else {
        panic!()
    };
    assert!(
        encode(
            &Message::DevListReply {
                status: 1,
                devices: devices.clone()
            },
            l,
            &mut |_| Ok(())
        )
        .is_err()
    );
    devices[0].interfaces.pop();
    assert!(
        encode(
            &Message::DevListReply { status: 0, devices },
            l,
            &mut |_| Ok(())
        )
        .is_err()
    );
}
#[test]
fn fixed_strings_are_bounded_and_byte_preserving() {
    assert!(WireString::<0>::new(b"").is_err());
    assert!(WireString::<4>::new(b"abcd").is_err());
    assert!(WireString::<4>::new(b"a\0b").is_err());
    assert_eq!(
        WireString::<4>::new(&[0xFF, 0xFE]).unwrap().as_bytes(),
        &[0xFF, 0xFE]
    );
    let mut b = fixture("import-request");
    b[8] = 0xFF;
    let (m, _) = decode(&b, Context::Management, Limits::default(), &mut |_| Ok(())).unwrap();
    assert_eq!(encode(&m, Limits::default(), &mut |_| Ok(())).unwrap(), b);
}
#[test]
fn generated_transfer_round_trips_cover_directions_statuses_and_iso() {
    for n in 0..128u32 {
        for direction in [Direction::Out, Direction::In] {
            let data: Vec<_> = (0..n).map(|v| v as u8).collect();
            let s = Submit {
                seqnum: u32::MAX - n,
                device_id: 0xAABBCDEF,
                direction,
                endpoint: n % 16,
                transfer_flags: n,
                transfer_buffer_length: n,
                start_frame: -1,
                number_of_packets: 0,
                interval: n as i32,
                setup: [0xA5; 8],
                data: if direction == Direction::Out {
                    data.clone()
                } else {
                    vec![]
                },
                iso_packets: vec![],
            };
            let r = ReplyContext::from(&s);
            let m = Message::Submit(s);
            assert_eq!(
                decode(
                    &encode(&m, Limits::default(), &mut |_| Ok(())).unwrap(),
                    Context::Requests,
                    Limits::default(),
                    &mut |_| Ok(())
                )
                .unwrap()
                .0,
                m
            );
            for status in [0, -32, -104, i32::MIN, i32::MAX] {
                let m = Message::ReturnSubmit(ReturnSubmit {
                    request: r,
                    status,
                    actual_length: n,
                    start_frame: -1,
                    number_of_packets: 0,
                    error_count: 0,
                    data: if direction == Direction::In {
                        data.clone()
                    } else {
                        vec![]
                    },
                    iso_packets: vec![],
                });
                assert_eq!(
                    decode(
                        &encode(&m, Limits::default(), &mut |_| Ok(())).unwrap(),
                        Context::Replies(Some(r)),
                        Limits::default(),
                        &mut |_| Ok(())
                    )
                    .unwrap()
                    .0,
                    m
                );
            }
        }
    }
}
#[test]
fn deterministic_mutation_corpus_never_panics_and_success_reencodes() {
    for (name, context) in cases() {
        let b = fixture(name);
        for offset in 0..b.len() {
            for mask in [1, 0x80, 0xFF] {
                let mut mutated = b.clone();
                mutated[offset] ^= mask;
                if let Ok((m, n)) = decode(&mutated, context, Limits::default(), &mut |_| Ok(())) {
                    assert_eq!(
                        encode(&m, Limits::default(), &mut |_| Ok(())).unwrap(),
                        mutated[..n],
                        "{name}/{offset}"
                    );
                }
            }
        }
    }
}

#[path = "fuzz/support.rs"]
mod fuzz_support;

fn fuzz_seeds() -> impl Iterator<Item = (&'static str, Vec<u8>)> {
    cases().into_iter().map(|(name, context)| {
        let (selector, r) = match context {
            Context::Management => (0, request(Direction::Out, 64, -1)),
            Context::Requests => (1, request(Direction::Out, 64, -1)),
            Context::Replies(r) => {
                let r = r.unwrap_or(request(Direction::Out, 64, -1));
                (if r.direction == Direction::In { 2 } else { 3 }, r)
            }
        };
        let mut seed = vec![selector];
        seed.extend(r.seqnum.to_be_bytes());
        seed.extend(r.transfer_buffer_length.to_be_bytes());
        seed.extend(r.number_of_packets.to_be_bytes());
        seed.extend(fixture(name));
        (name, seed)
    })
}

// Explicit utility invocation keeps corpus generation beside the shared fixtures.
#[test]
#[ignore = "writes local fuzz corpus; run explicitly before cargo fuzz"]
fn write_fuzz_seeds() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fuzz/corpus");
    for target in ["decode", "stream"] {
        let directory = root.join(target);
        std::fs::create_dir_all(&directory).unwrap();
        for (name, seed) in fuzz_seeds() {
            std::fs::write(directory.join(name), seed).unwrap();
        }
    }
}

#[test]
fn fuzz_assertions_run_offline_on_seeds_and_deterministic_noise() {
    for (_, seed) in fuzz_seeds() {
        fuzz_support::decode_target(&seed);
        fuzz_support::stream_target(&seed);
        for i in 0..seed.len() {
            let mut mutated = seed.clone();
            mutated[i] ^= 0xFF;
            fuzz_support::decode_target(&mutated);
            fuzz_support::stream_target(&mutated);
        }
    }
    let mut state = 0xD3A9_4127_826B_5C01u64;
    for len in 0..1024 {
        let data: Vec<_> = (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        fuzz_support::decode_target(&data);
        fuzz_support::stream_target(&data);
    }
}

#[test]
fn admission_covers_all_codec_allocations_and_preserves_wire_results() {
    let limits = Limits::default();
    for (name, context) in cases() {
        let bytes = fixture(name);
        let expected = decode(&bytes, context, limits, &mut |_| Ok(())).unwrap();
        let mut allocations = Vec::new();
        let decoded = decode(&bytes, context, limits, &mut |n| {
            allocations.push(n);
            Ok(())
        })
        .unwrap();
        assert_eq!(decoded, expected, "{name}");
        // Denying any allocation must abort, including later interface/ISO arrays.
        for fail in 0..allocations.len() {
            let mut calls = 0;
            assert!(
                decode(&bytes, context, limits, &mut |_| {
                    let reject = calls == fail;
                    calls += 1;
                    if reject {
                        Err(Error::Limit("test admission"))
                    } else {
                        Ok(())
                    }
                })
                .is_err(),
                "{name}, allocation {fail}"
            );
            assert_eq!(calls, fail + 1);
        }
        let mut output_bytes = 0;
        let encoded = encode(&decoded.0, limits, &mut |n| {
            output_bytes += n;
            Ok(())
        })
        .unwrap();
        assert_eq!(output_bytes, encoded.capacity());
        assert_eq!(encoded, bytes);
        assert!(
            encode(&decoded.0, limits, &mut |_| Err(Error::Limit(
                "test admission"
            )))
            .is_err()
        );
        // Streaming admission also covers every framing stage and fallback copy.
        let mut decoder = Decoder::new(context, limits);
        let mut allocations = Vec::new();
        let mut message = None;
        for byte in &bytes {
            let (_, m) = decoder
                .push(&[*byte], &mut |n| {
                    allocations.push(n);
                    Ok(())
                })
                .unwrap();
            if m.is_some() {
                message = m;
            }
        }
        assert_eq!(message, Some(expected.0));
        for fail in 0..allocations.len() {
            let mut decoder = Decoder::new(context, limits);
            let mut calls = 0;
            let mut rejected = false;
            for byte in &bytes {
                if decoder
                    .push(&[*byte], &mut |_| {
                        let reject = calls == fail;
                        calls += 1;
                        if reject {
                            Err(Error::Limit("test admission"))
                        } else {
                            Ok(())
                        }
                    })
                    .is_err()
                {
                    rejected = true;
                    break;
                }
            }
            assert!(rejected, "{name}, streaming allocation {fail}");
            assert_eq!(calls, fail + 1);
            assert_eq!(decoder.push(&[], &mut |_| Ok(())), Err(Error::Poisoned));
        }
    }
}

#[test]
fn admission_rejects_body_before_reserving_it_and_header_split_moves_storage() {
    let limits = Limits::default();
    let mut bytes = fixture("bulk-out");
    word(&mut bytes, 24, 1024 * 1024);
    let mut decoder = Decoder::new(Context::Requests, limits);
    decoder.push(&bytes[..48], &mut |_| Ok(())).unwrap();
    let capacity = decoder.allocated_capacity();
    assert!(
        decoder
            .push(&[0], &mut |n| {
                assert_eq!(n, 1024 * 1024);
                Err(Error::Limit("aggregate bytes"))
            })
            .is_err()
    );
    assert_eq!(decoder.allocated_capacity(), capacity);
    let Message::Submit(submit) =
        decode(&fixture("iso-out"), Context::Requests, limits, &mut |_| {
            Ok(())
        })
        .unwrap()
        .0
    else {
        panic!()
    };
    let original = submit.clone();
    let pointer = submit.data.as_ptr();
    let iso_pointer = submit.iso_packets.as_ptr();
    let (header, data, iso) = submit.into_parts();
    assert_eq!(data.as_ptr(), pointer);
    assert_eq!(iso.as_ptr(), iso_pointer);
    assert_eq!(ReplyContext::from(&header), ReplyContext::from(&original));
    assert_eq!(header.with_payload(data, iso), original);
}

#[test]
fn borrowed_completion_matches_every_golden_reply_and_rejects_before_allocation() {
    for (name, context) in cases() {
        let bytes = fixture(name);
        let (Message::ReturnSubmit(reply), _) =
            decode(&bytes, context, Limits::default(), &mut |_| Ok(())).unwrap()
        else {
            continue;
        };
        let header = ReturnHeader::from(&reply);
        let frame = || Frame::ReturnSubmit {
            header,
            data: &reply.data,
            packets: &reply.iso_packets,
        };
        let mut charged = 0;
        let encoded = encode(frame(), Limits::default(), &mut |n| {
            charged += n;
            Ok(())
        })
        .unwrap();
        assert_eq!(encoded, bytes, "{name}");
        assert_eq!(charged, encoded.capacity());
        assert_eq!(
            encode(frame(), Limits::default(), &mut |_| Err(Error::Limit(
                "denied"
            ))),
            Err(Error::Limit("denied"))
        );
        let mut invalid = header;
        invalid.actual_length = header.request.transfer_buffer_length + 1;
        let mut allocations = 0;
        assert!(
            encode(
                Frame::ReturnSubmit {
                    header: invalid,
                    data: &reply.data,
                    packets: &reply.iso_packets
                },
                Limits::default(),
                &mut |_| {
                    allocations += 1;
                    Ok(())
                }
            )
            .is_err()
        );
        assert_eq!(allocations, 0);
    }
}

#[test]
fn blocking_reader_admits_storage_and_denial_does_not_read_a_body() {
    let bytes = fixture("bulk-out");
    let mut reader = Cursor::new(bytes.clone());
    let mut allocated = 0;
    let message = read_message(
        &mut reader,
        Context::Requests,
        Limits::default(),
        &mut |n| {
            allocated += n;
            Ok(())
        },
    )
    .unwrap()
    .unwrap();
    assert!(allocated >= bytes.len());
    assert_eq!(
        encode(&message, Limits::default(), &mut |_| Ok(())).unwrap(),
        bytes
    );
    let mut reader = Cursor::new(bytes);
    let result = read_message(
        &mut reader,
        Context::Requests,
        Limits::default(),
        &mut |_| Err(Error::Limit("denied")),
    );
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    assert_eq!(
        reader.position(),
        0,
        "admission precedes input allocation and reading"
    );
}
