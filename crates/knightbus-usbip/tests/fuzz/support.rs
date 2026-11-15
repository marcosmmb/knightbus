//! Shared fuzz assertions, also compiled and exercised by normal offline tests.
use knightbus_usbip::*;

fn inputs(data: &[u8]) -> Option<(&[u8], Context, Limits)> {
    if data.len() < 13 {
        return None;
    }
    let word = |p| u32::from_be_bytes(data[p..p + 4].try_into().unwrap());
    let context = match data[0] % 4 {
        0 => Context::Management,
        1 => Context::Requests,
        n => Context::Replies(Some(ReplyContext {
            seqnum: word(1),
            direction: if n == 2 {
                Direction::In
            } else {
                Direction::Out
            },
            transfer_buffer_length: word(5),
            number_of_packets: word(9) as i32,
        })),
    };
    Some((
        &data[13..],
        context,
        Limits {
            frame_bytes: 8192,
            transfer_bytes: 4096,
            devices: 8,
            iso_packets: 32,
        },
    ))
}
/// Successful parses must preserve exactly their consumed bytes when encoded.
pub fn decode_target(data: &[u8]) {
    if let Some((bytes, context, limits)) = inputs(data) {
        let expected = decode(bytes, context, limits, &mut |_| Ok(()));
        let mut remaining = usize::from(data[0]) * 128 + usize::from(*data.last().unwrap()) * 64;
        let admitted = decode(bytes, context, limits, &mut |n| {
            remaining = remaining
                .checked_sub(n)
                .ok_or(Error::Limit("fuzz allocation"))?;
            Ok(())
        });
        if admitted != Err(Error::Limit("fuzz allocation")) {
            assert_eq!(admitted, expected);
        }
        if let Ok((message, consumed)) = expected {
            assert!(consumed <= bytes.len());
            assert!(consumed <= limits.frame_bytes);
            let encoded = encode(&message, limits, &mut |_| Ok(())).unwrap();
            assert_eq!(encoded, bytes[..consumed]);
            assert_eq!(
                decode(&encoded, context, limits, &mut |_| Ok(())).unwrap(),
                (message, consumed)
            );
        }
    }
}
/// Fragmentation must agree with whole-prefix parsing, including malformed input.
pub fn stream_target(data: &[u8]) {
    if let Some((bytes, context, limits)) = inputs(data) {
        let expected = decode(bytes, context, limits, &mut |_| Ok(()));
        let mut decoder = Decoder::new(context, limits);
        let mut pos = 0;
        let mut remaining = usize::from(data[0]) * 128 + usize::from(*data.last().unwrap()) * 64;
        while pos < bytes.len() {
            let end = bytes.len().min(pos + 1 + bytes[pos] as usize % 31);
            match decoder.push(&bytes[pos..end], &mut |n| {
                remaining = remaining
                    .checked_sub(n)
                    .ok_or(Error::Limit("fuzz allocation"))?;
                Ok(())
            }) {
                Ok((used, Some(message))) => {
                    assert_eq!(expected, Ok((message, pos + used)));
                    assert!(decoder.finish().is_ok());
                    return;
                }
                Ok((used, None)) => {
                    assert_eq!(used, end - pos);
                    pos += used;
                }
                Err(error) => {
                    if error != Error::Limit("fuzz allocation") {
                        assert_eq!(expected, Err(error));
                    }
                    assert_eq!(decoder.push(&[], &mut |_| Ok(())), Err(Error::Poisoned));
                    return;
                }
            }
            assert!(decoder.buffered_len() <= limits.frame_bytes);
        }
        assert!(matches!(expected, Err(Error::Incomplete(_))));
        assert_eq!(decoder.finish().is_ok(), bytes.is_empty());
    }
}
