use knightbus_windows_protocol::iso::{delay, start_frame};

#[test]
fn frame_schedule_is_bounded_and_wraps() {
    for now in [0, 1, 1024, u32::MAX - 1, u32::MAX] {
        assert_eq!(start_frame(now, None), Ok(now.wrapping_add(1)));
        for ahead in [0, 1, 1023] {
            let frame = now.wrapping_add(ahead);
            assert_eq!(start_frame(now, Some(frame)), Ok(frame));
            assert_eq!(delay(now, frame), ahead);
            assert_eq!(delay(frame.wrapping_add(1), frame), 0);
        }
        for outside in [-1024i32, -1, 1024, i32::MAX, i32::MIN] {
            assert!(start_frame(now, Some(now.wrapping_add(outside as u32))).is_err());
        }
    }
}

#[test]
fn iso_slot_pool_is_disjoint_bounded_and_preserves_other_endpoints() {
    use knightbus_windows_protocol::{descriptors::*, iso::slot_masks};
    let device = [18, 1, 0, 2, 0, 0, 0, 64, 9, 0x12, 1, 0, 0, 1, 0, 0, 0, 1];
    for endpoints in [2, 10, 30] {
        let mut config = vec![
            9, 2, 0, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, endpoints, 0xff, 0, 0, 0,
        ];
        for i in 0..endpoints {
            let address = (i % 15 + 1) | if i >= 15 { 0x80 } else { 0 };
            let iso = i % 2 == 0;
            config.extend([
                7,
                5,
                address,
                if iso { 1 } else { 2 },
                64,
                0,
                if iso { 1 } else { 0 },
            ]);
        }
        let len = config.len() as u16;
        config[2..4].copy_from_slice(&len.to_le_bytes());
        let topology = validate(&device, &config, Speed::Full).unwrap();
        let masks = slot_masks(&topology);
        assert_eq!(masks[0], 1);
        let mut used = 1;
        for i in 0..endpoints as usize {
            let mask = masks[i + 1];
            assert_eq!(mask & used, 0);
            assert_ne!(mask & (1 << (i + 1)), 0);
            assert!(mask.count_ones() <= 8);
            if i % 2 == 1 {
                assert_eq!(mask, 1 << (i + 1));
            }
            used |= mask;
        }
        assert_eq!(used >> 31, 0);
    }
}

#[test]
fn packet_records_reject_mutated_layout_and_invalid_results() {
    use knightbus_windows_protocol::iso::*;
    let original = [
        Packet {
            offset: 0,
            length: 8,
            actual: 0,
            status: 0,
        },
        Packet {
            offset: 12,
            length: 4,
            actual: 0,
            status: 0,
        },
    ];
    let mut encoded = [0; 32];
    for (p, bytes) in original
        .iter()
        .zip(encoded.as_chunks_mut::<16>().0.iter_mut())
    {
        p.write(bytes);
    }
    assert_eq!(Packets::decode(&encoded, 16, false).unwrap().len(), 2);
    for length in 0..32 {
        assert!(Packets::decode(&encoded[..length], 16, false).is_err() || length == 16);
    }
    let reply = [
        Packet {
            actual: 3,
            ..original[0]
        },
        Packet {
            status: -18,
            ..original[1]
        },
    ];
    for (p, bytes) in reply.iter().zip(encoded.as_chunks_mut::<16>().0.iter_mut()) {
        p.write(bytes);
    }
    assert!(Packets::decode(&encoded, 16, false).is_err());
    let packets = Packets::decode(&encoded, 16, true).unwrap();
    packets.validate_reply(&original, 3, 1).unwrap();
    assert!(packets.validate_reply(&original, 3, 0).is_err());
    assert!(packets.validate_reply(&original, 4, 1).is_err());
    for (offset, value) in [(0, 1u32), (4, 9), (8, 9), (12, 1), (16, 7), (20, u32::MAX)] {
        let mut bad = encoded;
        bad[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        assert!(
            Packets::decode(&bad, 16, true)
                .and_then(|p| p.validate_reply(&original, 3, 1))
                .is_err()
        );
    }
}

#[test]
fn queued_frame_schedules_use_speed_interval_and_reset_after_idle() {
    use knightbus_windows_protocol::iso::Schedule;
    let mut full = Schedule::default();
    assert_eq!(full.reserve(100, None, 8, 1, false).unwrap(), (101, 109));
    assert_eq!(full.reserve(100, None, 8, 1, false).unwrap(), (109, 117));
    assert_eq!(full.reserve(2000, None, 8, 1, false).unwrap(), (2001, 2009));
    let mut high = Schedule::default();
    assert_eq!(high.reserve(u32::MAX, None, 8, 1, true).unwrap(), (0, 1));
    assert_eq!(high.reserve(u32::MAX, None, 8, 2, true).unwrap(), (1, 3));
    assert_eq!(
        high.reserve(100, Some(120), 8, 4, true).unwrap(),
        (120, 128)
    );
    assert!(high.reserve(100, Some(99), 8, 1, true).is_err());
    assert!(high.reserve(100, Some(1124), 8, 1, true).is_err());
    assert!(high.reserve(100, None, 1025, 1, true).is_err());
    assert!(high.reserve(100, None, 8, 0, true).is_err());
    assert!(high.reserve(100, None, 1024, 16, true).is_err());
}

#[test]
fn asap_reservation_preserves_a_future_tail_outside_the_start_window() {
    use knightbus_windows_protocol::iso::Schedule;
    for now in [100u32, u32::MAX - 500] {
        let mut schedule = Schedule::default();
        let mut tail = now.wrapping_add(1);
        for _ in 0..5 {
            let end = tail.wrapping_add(255);
            assert_eq!(schedule.reserve(now, None, 255, 1, false), Ok((tail, end)));
            tail = end;
        }
        // Reject instead of starting over, and leave the previous tail intact.
        for _ in 0..2 {
            assert!(schedule.reserve(now, None, 255, 1, false).is_err());
        }
        // The tail is still outside the window at exactly 1024 frames ahead.
        assert!(
            schedule
                .reserve(tail.wrapping_sub(1024), None, 8, 1, false)
                .is_err()
        );
        // Once it enters the window, continue after the last reservation.
        assert_eq!(
            schedule.reserve(tail.wrapping_sub(1023), None, 8, 1, false),
            Ok((tail, tail.wrapping_add(8)))
        );
        // At an idle boundary, begin with the next frame again.
        let idle = tail.wrapping_add(8);
        assert_eq!(
            schedule.reserve(idle, None, 8, 1, false),
            Ok((idle.wrapping_add(1), idle.wrapping_add(9)))
        );
    }
}

#[test]
fn deadline_expiring_between_scan_and_arm_still_schedules_a_wakeup() {
    use knightbus_windows_protocol::iso::CompletionTiming;
    for due in [0u32, 100, u32::MAX] {
        let timing = CompletionTiming::Scheduled(due);
        // The completion scan skips the last queued request just before it is due.
        assert!(timing.waiting(due.wrapping_sub(1)));
        // Time advances before timer setup. No later traffic is needed to wake it.
        assert_eq!(timing.wake_after(due), Some(1));
        assert_eq!(timing.wake_after(due.wrapping_add(1)), Some(1));
        assert!(!timing.waiting(due.wrapping_add(1)));
        // Future results keep their timing, without a repeated one-ms poll.
        assert_eq!(timing.wake_after(due.wrapping_sub(20)), Some(20));
    }
    assert!(!CompletionTiming::Immediate.waiting(100));
    assert_eq!(CompletionTiming::Immediate.wake_after(100), None);
}

#[test]
fn failed_iso_results_complete_immediately_and_keep_packet_results() {
    use knightbus_windows_protocol::{iso::*, wire::Reply};
    for status in [0u32, 1, 2] {
        for errors in 0..=2u32 {
            let records: Vec<_> = (0..2u32)
                .map(|i| Packet {
                    offset: i * 4,
                    length: 4,
                    actual: if i < errors { 0 } else { 4 },
                    status: if i < errors { -18 } else { 0 },
                })
                .collect();
            let actual: u32 = records.iter().map(|p| p.actual).sum();
            let mut bytes = vec![0; HEADER + actual as usize + records.len() * PACKET_BYTES];
            for (offset, value) in [
                (0, VERSION),
                (4, 0x81),
                (16, actual),
                (20, actual),
                (24, status),
                (field::PACKET_COUNT, 2),
                (field::ERRORS, errors),
            ] {
                bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            }
            bytes[HEADER..HEADER + actual as usize].fill(0x5a);
            for (packet, target) in records.iter().zip(
                bytes[HEADER + actual as usize..]
                    .as_chunks_mut::<PACKET_BYTES>()
                    .0
                    .iter_mut(),
            ) {
                packet.write(target);
            }
            let reply = Reply::decode(&bytes).unwrap();
            reply.validate_length(8, true).unwrap();
            let timing = reply.completion_timing(500);
            if status == 0 && errors < 2 {
                // Partial packet errors still have a successful overall URB.
                assert_eq!(timing, CompletionTiming::Scheduled(500));
                assert!(timing.waiting(100));
                assert_eq!(timing.wake_after(100), Some(400));
            } else {
                assert_eq!(timing, CompletionTiming::Immediate);
                assert!(!timing.waiting(100));
                assert_eq!(timing.wake_after(100), None);
            }
            assert_eq!(reply.packets.unwrap().iter().collect::<Vec<_>>(), records);
            assert_eq!(reply.payload, vec![0x5a; actual as usize]);
            assert_eq!(reply.errors, errors);
            assert_eq!(reply.status, status);
        }
    }
}

#[test]
fn ordinary_replies_never_use_iso_completion_timing() {
    use knightbus_windows_protocol::{iso::CompletionTiming, wire::Reply};
    let mut bytes = [0; 32];
    bytes[0] = 1;
    let reply = Reply::decode(&bytes).unwrap();
    assert_eq!(reply.completion_timing(500), CompletionTiming::Immediate);
}
