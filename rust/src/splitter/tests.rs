use super::*;
use crate::crypto::generate_relay_init;

/// Build a splitter plus an encryptor that mirrors it, so tests can hand it
/// correctly-encrypted packets.
fn splitter_and_encryptor(proto: ProtoTag) -> (MsgSplitter, AesCtr256) {
    let relay_init = generate_relay_init(proto, 2);
    let splitter = MsgSplitter::new(&relay_init, proto);

    let mut enc = make_cipher(
        &relay_init[SKIP_LEN..SKIP_LEN + PREKEY_LEN],
        &relay_init[SKIP_LEN + PREKEY_LEN..SKIP_LEN + PREKEY_LEN + IV_LEN],
    );
    let mut skip = [0u8; HANDSHAKE_LEN];
    enc.apply_keystream(&mut skip);

    (splitter, enc)
}

fn intermediate_packet(payload_len: usize) -> Vec<u8> {
    let mut packet = (payload_len as u32).to_le_bytes().to_vec();
    packet.resize(4 + payload_len, 0x5a);
    packet
}

/// Feed one already-framed packet through the splitter, encrypting it first.
fn feed(splitter: &mut MsgSplitter, enc: &mut AesCtr256, packet: &[u8]) -> Vec<Vec<u8>> {
    let mut wire = packet.to_vec();
    enc.apply_keystream(&mut wire);

    splitter.split(&wire)
}

#[test]
fn an_oversized_buffer_is_released_after_enough_quiet_calls() {
    let (mut splitter, mut enc) = splitter_and_encryptor(ProtoTag::PaddedIntermediate);

    // One big packet forces the buffers to grow well past what is retained.
    // That call drains fully, so it already counts as the first quiet one.
    let big = intermediate_packet(8 * RETAINED_CAPACITY);
    assert_eq!(feed(&mut splitter, &mut enc, &big).len(), 1);
    assert!(splitter.cipher_buf.capacity() > RETAINED_CAPACITY);
    assert_eq!(splitter.idle_calls, 1);

    // Each subsequent small packet drains fully, so the counter keeps climbing
    // and the capacity is held until the threshold is actually reached.
    let small = intermediate_packet(16);
    for expected in 2..TRIM_AFTER_IDLE_CALLS {
        assert_eq!(feed(&mut splitter, &mut enc, &small).len(), 1);
        assert_eq!(splitter.idle_calls, expected);
        assert!(
            splitter.cipher_buf.capacity() > RETAINED_CAPACITY,
            "released early, after {expected} quiet calls"
        );
    }

    // The call that reaches the threshold gives the capacity back.
    assert_eq!(feed(&mut splitter, &mut enc, &small).len(), 1);
    assert!(splitter.cipher_buf.capacity() <= RETAINED_CAPACITY);
    assert!(splitter.plain_buf.capacity() <= RETAINED_CAPACITY);
    assert_eq!(splitter.idle_calls, 0);
}

#[test]
fn a_sustained_transfer_never_pays_for_a_shrink() {
    // The counter must reset whenever a call leaves data buffered, otherwise a
    // media transfer would shrink and regrow its buffer on every packet.
    let (mut splitter, mut enc) = splitter_and_encryptor(ProtoTag::PaddedIntermediate);

    let big = intermediate_packet(8 * RETAINED_CAPACITY);
    feed(&mut splitter, &mut enc, &big);
    let grown = splitter.cipher_buf.capacity();

    for _ in 0..(TRIM_AFTER_IDLE_CALLS as usize * 3) {
        let next = intermediate_packet(4 * RETAINED_CAPACITY);
        let mut wire = next.clone();
        enc.apply_keystream(&mut wire);

        // Half a packet leaves the buffer non-empty...
        let split_at = wire.len() / 2;
        assert!(splitter.split(&wire[..split_at]).is_empty());
        assert_eq!(splitter.idle_calls, 0, "counter climbed mid-packet");

        // ...and the rest completes it.
        assert_eq!(splitter.split(&wire[split_at..]).len(), 1);
    }

    assert_eq!(
        splitter.cipher_buf.capacity(),
        grown,
        "a sustained transfer must not shrink and regrow its buffer"
    );
}

#[test]
fn a_buffer_that_never_grows_is_left_alone() {
    let (mut splitter, mut enc) = splitter_and_encryptor(ProtoTag::PaddedIntermediate);

    let small = intermediate_packet(16);
    for _ in 0..(TRIM_AFTER_IDLE_CALLS as usize * 2) {
        feed(&mut splitter, &mut enc, &small);
        // Nothing to release, so the counter never starts climbing.
        assert_eq!(splitter.idle_calls, 0);
    }

    assert!(splitter.cipher_buf.capacity() <= RETAINED_CAPACITY);
}

#[test]
fn disabling_the_splitter_releases_both_buffers_outright() {
    let (mut splitter, mut enc) = splitter_and_encryptor(ProtoTag::PaddedIntermediate);

    let big = intermediate_packet(8 * RETAINED_CAPACITY);
    feed(&mut splitter, &mut enc, &big);
    assert!(splitter.cipher_buf.capacity() > RETAINED_CAPACITY);

    // A zero-length packet switches the splitter to pass-through for good.
    let zero = intermediate_packet(0);
    feed(&mut splitter, &mut enc, &zero);

    assert!(splitter.disabled);
    assert_eq!(splitter.cipher_buf.capacity(), 0);
    assert_eq!(splitter.plain_buf.capacity(), 0);
}
