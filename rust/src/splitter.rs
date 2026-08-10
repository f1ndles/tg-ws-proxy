//! MTProto message splitter.
//!
//! Each Telegram WebSocket message is expected to carry exactly **one**
//! complete MTProto transport packet.  If we forward chunks of the raw TCP
//! stream without respecting packet boundaries, Telegram will reject or
//! misparse the connection.
//!
//! The splitter maintains an internal decrypt copy of the AES-256-CTR cipher
//! (in lockstep with the proxy's `tg_enc` cipher) so it can peek at the
//! plaintext packet-length fields and figure out where each packet ends —
//! while returning the **encrypted** bytes for forwarding.

use cipher::StreamCipher;

use crate::crypto::{AesCtr256, HANDSHAKE_LEN, ProtoTag, SKIP_LEN, make_cipher};

const PREKEY_LEN: usize = 32;
const IV_LEN: usize = 16;

/// Buffer capacity kept between packets.
///
/// A single large media packet (Telegram file chunks run to hundreds of KiB)
/// has to be buffered whole before it can be emitted as one WebSocket frame,
/// and `Vec` never gives that capacity back on its own.  Two buffers per
/// connection, held for the connection's lifetime, is real memory on the
/// low-RAM routers this proxy runs on — so once a connection goes back to
/// small packets, the oversized capacity is released.
const RETAINED_CAPACITY: usize = 16 * 1024;

/// Consecutive fully-drained `split` calls before oversized capacity is
/// released.  Waiting keeps a sustained media transfer — which repeatedly
/// needs the large buffer — from paying for a shrink-then-regrow cycle on
/// every packet.
///
/// Note this counts *calls, not time*: the release happens on the connection's
/// next reads, so a connection that finishes a transfer and then falls
/// completely silent keeps its buffers until it carries traffic again (or
/// closes, which frees them anyway). That is the intended trade — trimming is
/// driven from the data path so it costs nothing to connections that are idle.
const TRIM_AFTER_IDLE_CALLS: u8 = 64;

pub struct MsgSplitter {
    /// Internal cipher that shadows `tg_enc` to decrypt the stream in-band.
    dec: AesCtr256,
    proto: ProtoTag,
    /// Buffered encrypted bytes (ready to be returned as WS frames).
    cipher_buf: Vec<u8>,
    /// Buffered plaintext (used only for length-parsing).  Always the same
    /// length as `cipher_buf`.
    plain_buf: Vec<u8>,
    /// Once set, the splitter stops trying to parse lengths and returns
    /// any buffered data as a single chunk (unknown / unsupported proto).
    disabled: bool,
    /// How many consecutive calls have left the buffers empty and oversized.
    idle_calls: u8,
}

impl MsgSplitter {
    /// Create a new splitter that mirrors the relay encryptor state.
    ///
    /// `relay_init` is the 64-byte obfuscation init sent to Telegram.
    /// The splitter builds the same AES-256-CTR cipher (raw key, no secret
    /// hash) and fast-forwards it by 64 bytes — identical to how `tg_enc`
    /// is initialised in `build_connection_ciphers`.
    pub fn new(relay_init: &[u8; HANDSHAKE_LEN], proto: ProtoTag) -> Self {
        let relay_enc_key = &relay_init[SKIP_LEN..SKIP_LEN + PREKEY_LEN];
        let relay_enc_iv = &relay_init[SKIP_LEN + PREKEY_LEN..SKIP_LEN + PREKEY_LEN + IV_LEN];
        let mut dec = make_cipher(relay_enc_key, relay_enc_iv);

        // Advance past the relay init (same fast-forward as tg_enc).
        let mut dummy = [0u8; HANDSHAKE_LEN];

        dec.apply_keystream(&mut dummy);

        Self {
            dec,
            proto,
            cipher_buf: Vec::new(),
            plain_buf: Vec::new(),
            disabled: false,
            idle_calls: 0,
        }
    }

    /// Feed relay-encrypted bytes and receive back a list of complete
    /// encrypted MTProto packets, each to be sent as one WebSocket frame.
    ///
    /// Incomplete packets are buffered internally until the next call.
    pub fn split(&mut self, encrypted: &[u8]) -> Vec<Vec<u8>> {
        if encrypted.is_empty() {
            return Vec::new();
        }

        // If we don't know how to parse this protocol, pass everything through.
        if self.disabled {
            return vec![encrypted.to_vec()];
        }

        // Decrypt straight into the plaintext buffer: the CTR cipher is
        // stateful and already advanced past everything buffered earlier, so
        // only the freshly appended bytes need the keystream applied.  Doing
        // it in place avoids a second full-size allocation and copy on every
        // read.
        let fresh_from = self.plain_buf.len();
        self.plain_buf.extend_from_slice(encrypted);
        self.dec.apply_keystream(&mut self.plain_buf[fresh_from..]);
        self.cipher_buf.extend_from_slice(encrypted);

        let mut parts = Vec::new();
        let mut consumed = 0usize;
        loop {
            match self.next_packet_len(consumed) {
                None => break, // need more bytes
                Some(0) => {
                    // Unsupported / unknown protocol variant — disable parsing
                    // and flush everything buffered so far.  Neither buffer is
                    // ever used again, so release them outright.
                    parts.push(self.cipher_buf[consumed..].to_vec());

                    self.cipher_buf = Vec::new();
                    self.plain_buf = Vec::new();
                    self.disabled = true;

                    return parts;
                }
                Some(len) => {
                    let end = consumed + len;
                    parts.push(self.cipher_buf[consumed..end].to_vec());
                    consumed = end;
                }
            }
        }

        if consumed != 0 {
            self.cipher_buf.drain(..consumed);
            self.plain_buf.drain(..consumed);
        }

        self.trim_idle_buffers();

        parts
    }

    /// Flush any remaining buffered bytes as a single chunk.
    pub fn flush(&mut self) -> Vec<Vec<u8>> {
        if self.cipher_buf.is_empty() {
            return Vec::new();
        }

        let tail = std::mem::take(&mut self.cipher_buf);

        self.plain_buf.clear();
        self.plain_buf.shrink_to(RETAINED_CAPACITY);

        vec![tail]
    }

    /// Release capacity grabbed by an unusually large packet once the stream
    /// settles back down — see [`RETAINED_CAPACITY`].
    fn trim_idle_buffers(&mut self) {
        // Both buffers always hold the same bytes, so emptiness is shared; only
        // `cipher_buf`'s capacity is consulted, and both are shrunk together.
        if !self.cipher_buf.is_empty() || self.cipher_buf.capacity() <= RETAINED_CAPACITY {
            self.idle_calls = 0;
            return;
        }

        self.idle_calls = self.idle_calls.saturating_add(1);
        if self.idle_calls < TRIM_AFTER_IDLE_CALLS {
            return;
        }

        self.cipher_buf.shrink_to(RETAINED_CAPACITY);
        self.plain_buf.shrink_to(RETAINED_CAPACITY);
        self.idle_calls = 0;
    }

    // ── Length parsers ────────────────────────────────────────────────────

    /// Returns the byte length of the next complete packet (header + payload),
    /// `None` if there isn't enough data yet, or `Some(0)` for unknown proto.
    fn next_packet_len(&self, offset: usize) -> Option<usize> {
        let plain = self.plain_buf.get(offset..)?;
        if plain.is_empty() {
            return None;
        }

        match self.proto {
            ProtoTag::Abridged => Self::abridged_len(plain),
            ProtoTag::Intermediate | ProtoTag::PaddedIntermediate => Self::intermediate_len(plain),
        }
    }

    /// Abridged transport length parsing.
    ///
    /// - 1-byte header: payload_len = (byte & 0x7F) * 4
    /// - 4-byte header (first byte is 0x7F or 0xFF): payload_len = next_3_bytes_le * 4
    fn abridged_len(plain: &[u8]) -> Option<usize> {
        let first = plain[0];
        let (payload_len, header_len) = if first == 0x7F || first == 0xFF {
            if plain.len() < 4 {
                return None; // need more data for 4-byte header
            }

            let l = u32::from_le_bytes([plain[1], plain[2], plain[3], 0]) as usize * 4;

            (l, 4)
        } else {
            ((first & 0x7F) as usize * 4, 1)
        };

        if payload_len == 0 {
            return Some(0); // signal to disable splitter
        }

        let total = header_len + payload_len;
        if plain.len() < total {
            None
        } else {
            Some(total)
        }
    }

    /// Intermediate / padded-intermediate transport length parsing.
    ///
    /// 4-byte LE header: payload_len = header & 0x7FFF_FFFF
    fn intermediate_len(plain: &[u8]) -> Option<usize> {
        if plain.len() < 4 {
            return None;
        }

        let payload_len =
            (u32::from_le_bytes([plain[0], plain[1], plain[2], plain[3]]) & 0x7FFF_FFFF) as usize;

        if payload_len == 0 {
            return Some(0);
        }

        let total = 4 + payload_len;
        if plain.len() < total {
            None
        } else {
            Some(total)
        }
    }
}

#[cfg(test)]
mod tests;
