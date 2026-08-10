use cipher::StreamCipher;
use tg_ws_proxy_rs::{
    crypto::{
        ProtoTag, build_connection_ciphers, generate_client_handshake, generate_relay_init,
        parse_handshake,
    },
    splitter::MsgSplitter,
};

#[test]
fn splitter_buffers_partial_intermediate_packet_until_complete() {
    // The WS backend expects complete MTProto packets, so a partial encrypted
    // TCP chunk must stay buffered until the length-prefixed packet is whole.
    let payload = b"0123456789abcdef";
    let mut plain_packet = Vec::new();
    plain_packet.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    plain_packet.extend_from_slice(payload);

    let (relay_init, encrypted) =
        encrypted_relay_packet(ProtoTag::PaddedIntermediate, &plain_packet);
    let mut splitter = MsgSplitter::new(&relay_init, ProtoTag::PaddedIntermediate);

    assert!(splitter.split(&encrypted[..5]).is_empty());
    assert_eq!(splitter.split(&encrypted[5..]), vec![encrypted]);
    assert!(splitter.flush().is_empty());
}

#[test]
fn splitter_returns_each_complete_abridged_packet_separately() {
    // A single TCP read can contain multiple MTProto packets; the splitter
    // must keep returning one encrypted packet per WebSocket message.
    let first_payload = b"abcdefgh";
    let second_payload = b"ijklmnop";
    let mut plain_stream = Vec::new();
    plain_stream.push((first_payload.len() / 4) as u8);
    plain_stream.extend_from_slice(first_payload);
    plain_stream.push((second_payload.len() / 4) as u8);
    plain_stream.extend_from_slice(second_payload);

    let (relay_init, encrypted_stream) = encrypted_relay_packet(ProtoTag::Abridged, &plain_stream);
    let mut splitter = MsgSplitter::new(&relay_init, ProtoTag::Abridged);

    let parts = splitter.split(&encrypted_stream);

    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0], encrypted_stream[..1 + first_payload.len()]);
    assert_eq!(parts[1], encrypted_stream[1 + first_payload.len()..]);
}

#[test]
fn splitter_disables_after_zero_length_abridged_packet() {
    let first_payload = b"abcdefgh";
    let mut plain_stream = Vec::new();
    plain_stream.push((first_payload.len() / 4) as u8);
    plain_stream.extend_from_slice(first_payload);
    plain_stream.push(0);
    plain_stream.extend_from_slice(b"tail");

    let (relay_init, encrypted_stream) = encrypted_relay_packet(ProtoTag::Abridged, &plain_stream);
    let mut splitter = MsgSplitter::new(&relay_init, ProtoTag::Abridged);

    let parts = splitter.split(&encrypted_stream);
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0], encrypted_stream[..1 + first_payload.len()]);
    assert_eq!(parts[1], encrypted_stream[1 + first_payload.len()..]);
}

#[test]
fn splitter_reassembles_a_packet_spread_across_many_reads() {
    // A large media packet arrives over many relay-buffer-sized reads and must
    // come back out as exactly one WebSocket frame.
    let payload = vec![0xa5u8; 40_000];
    let mut plain_packet = Vec::new();
    plain_packet.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    plain_packet.extend_from_slice(&payload);

    let (relay_init, encrypted) =
        encrypted_relay_packet(ProtoTag::PaddedIntermediate, &plain_packet);
    let mut splitter = MsgSplitter::new(&relay_init, ProtoTag::PaddedIntermediate);

    let mut parts = Vec::new();
    for chunk in encrypted.chunks(16 * 1024) {
        parts.extend(splitter.split(chunk));
    }

    assert_eq!(parts, vec![encrypted]);
    assert!(splitter.flush().is_empty());
}

#[test]
fn splitter_keeps_emitting_correctly_after_releasing_a_large_buffer() {
    // Oversized buffers are trimmed once traffic settles; the cipher state
    // must survive that, so packets sent long afterwards still decode.
    let big_payload = vec![7u8; 32_000];
    let mut plain_stream = Vec::new();
    plain_stream.extend_from_slice(&(big_payload.len() as u32).to_le_bytes());
    plain_stream.extend_from_slice(&big_payload);

    let small_payload = b"still working";
    let mut small_packet = Vec::new();
    small_packet.extend_from_slice(&(small_payload.len() as u32).to_le_bytes());
    small_packet.extend_from_slice(small_payload);

    // Enough small packets after the big one to cross the trim threshold.
    let small_packet_count = 200;
    for _ in 0..small_packet_count {
        plain_stream.extend_from_slice(&small_packet);
    }

    let (relay_init, encrypted) =
        encrypted_relay_packet(ProtoTag::PaddedIntermediate, &plain_stream);
    let mut splitter = MsgSplitter::new(&relay_init, ProtoTag::PaddedIntermediate);

    let mut parts = Vec::new();
    for chunk in encrypted.chunks(4096) {
        parts.extend(splitter.split(chunk));
    }

    assert_eq!(parts.len(), 1 + small_packet_count);
    let big_len = 4 + big_payload.len();
    assert_eq!(parts[0], encrypted[..big_len]);
    for (i, part) in parts[1..].iter().enumerate() {
        let start = big_len + i * small_packet.len();
        assert_eq!(part, &encrypted[start..start + small_packet.len()]);
    }
    assert!(splitter.flush().is_empty());
}

#[test]
fn splitter_flush_returns_an_incomplete_trailing_packet() {
    // A connection that dies mid-packet still has to forward what it has, so
    // the bytes are not silently dropped.
    let payload = b"0123456789abcdef";
    let mut plain_packet = Vec::new();
    plain_packet.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    plain_packet.extend_from_slice(payload);

    let (relay_init, encrypted) =
        encrypted_relay_packet(ProtoTag::PaddedIntermediate, &plain_packet);
    let mut splitter = MsgSplitter::new(&relay_init, ProtoTag::PaddedIntermediate);

    assert!(splitter.split(&encrypted[..10]).is_empty());

    assert_eq!(splitter.flush(), vec![encrypted[..10].to_vec()]);
    // Flushing twice must not repeat the tail.
    assert!(splitter.flush().is_empty());
}

#[test]
fn splitter_ignores_empty_reads() {
    let (relay_init, _) = encrypted_relay_packet(ProtoTag::Abridged, b"");
    let mut splitter = MsgSplitter::new(&relay_init, ProtoTag::Abridged);

    assert!(splitter.split(&[]).is_empty());
    assert!(splitter.flush().is_empty());
}

#[test]
fn splitter_handles_the_four_byte_abridged_header() {
    // Payloads of 0x7F*4 bytes or more switch to the extended abridged header.
    let payload = vec![3u8; 4 * 0x80];
    let mut plain_packet = vec![0x7f];
    plain_packet.extend_from_slice(&((payload.len() / 4) as u32).to_le_bytes()[..3]);
    plain_packet.extend_from_slice(&payload);

    let (relay_init, encrypted) = encrypted_relay_packet(ProtoTag::Abridged, &plain_packet);
    let mut splitter = MsgSplitter::new(&relay_init, ProtoTag::Abridged);

    assert_eq!(splitter.split(&encrypted), vec![encrypted.clone()]);
    assert!(splitter.flush().is_empty());
}

#[test]
fn splitter_passes_everything_through_once_disabled() {
    // After a zero-length packet the splitter stops parsing; later reads must
    // still be forwarded verbatim rather than swallowed.
    let mut plain_stream = vec![0u8];
    plain_stream.extend_from_slice(b"tail");

    let (relay_init, encrypted) = encrypted_relay_packet(ProtoTag::Abridged, &plain_stream);
    let mut splitter = MsgSplitter::new(&relay_init, ProtoTag::Abridged);

    assert_eq!(splitter.split(&encrypted), vec![encrypted.clone()]);
    assert_eq!(splitter.split(b"raw bytes"), vec![b"raw bytes".to_vec()]);
}

fn encrypted_relay_packet(proto: ProtoTag, plain_packet: &[u8]) -> ([u8; 64], Vec<u8>) {
    let secret = test_secret();
    let (handshake, _, _) = generate_client_handshake(&secret, 2, proto);
    let parsed = parse_handshake(&handshake, &secret).expect("generated handshake parses");
    let relay_init = generate_relay_init(parsed.proto, parsed.dc_id as i16);
    let mut ciphers = build_connection_ciphers(&parsed.prekey_and_iv, &secret, &relay_init);

    let mut encrypted = plain_packet.to_vec();
    ciphers.tg_enc.apply_keystream(&mut encrypted);

    (relay_init, encrypted)
}

fn test_secret() -> Vec<u8> {
    hex::decode("2a519e5be6c3219c69879e5fa2a0eab8").unwrap()
}
