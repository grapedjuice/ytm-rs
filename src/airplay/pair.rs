//! AirPlay 2's transient pairing: the handshake Apple TVs, HomePods and AirPlay 2 speakers
//! (Sonos and the like) want before they take a stream from a device they don't know.
//! HomeKit's pair-setup, with the fixed PIN every sender uses and nothing kept after: it
//! proves nothing about who's asking, but sets up keys that encrypt the RTSP connection
//! from then on. The audio itself goes as before.
//!
//! An Apple TV set to let only people sharing its home AirPlay to it wants a real pairing,
//! with a PIN it shows; this doesn't do that.

use std::collections::HashMap;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::ChaCha20Poly1305;
use num_bigint::BigUint;
use sha2::{Digest, Sha512};

const USER: &[u8] = b"Pair-Setup";
const PIN: &[u8] = b"3939";

// TLV8 item types, and the values used here.
pub const METHOD: u8 = 0x00;
pub const SALT: u8 = 0x02;
pub const PUBLIC_KEY: u8 = 0x03;
pub const PROOF: u8 = 0x04;
pub const STATE: u8 = 0x06;
pub const ERROR: u8 = 0x07;
pub const FLAGS: u8 = 0x13;
pub const TRANSIENT: u8 = 0x10;

/// HomeKit's TLV8: type, length, value, with values over 255 bytes split across items of
/// the same type in a row.
pub fn tlv_encode(items: &[(u8, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (kind, value) in items {
        if value.is_empty() {
            out.extend_from_slice(&[*kind, 0]);
        }
        for chunk in value.chunks(255) {
            out.push(*kind);
            out.push(chunk.len() as u8);
            out.extend_from_slice(chunk);
        }
    }
    out
}

pub fn tlv_decode(mut data: &[u8]) -> HashMap<u8, Vec<u8>> {
    let mut out: HashMap<u8, Vec<u8>> = HashMap::new();
    let mut last = None;
    while data.len() >= 2 {
        let (kind, len) = (data[0], data[1] as usize);
        let value = &data[2..(2 + len).min(data.len())];
        // A continuation follows its first part directly; the same type later starts anew.
        if last == Some(kind) {
            out.entry(kind).or_default().extend_from_slice(value);
        } else {
            out.insert(kind, value.to_vec());
        }
        last = Some(kind);
        data = &data[(2 + len).min(data.len())..];
    }
    out
}

/// RFC 5054's 3072-bit group, HomeKit's: the prime (generator 5).
const N_3072: &str = concat!(
    "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B139B22514A08798E3404DD",
    "EF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7ED",
    "EE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163BF0598DA48361C55D39A69163FA8FD24CF5F",
    "83655D23DCA3AD961C62F356208552BB9ED529077096966D670C354E4ABC9804F1746C08CA18217C32905E462E36CE3B",
    "E39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF6955817183995497CEA956AE515D2261898FA0510",
    "15728E5A8AAAC42DAD33170D04507A33A85521ABDF1CBA64ECFB850458DBEF0A8AEA71575D060C7DB3970F85A6E1E4C7",
    "ABF5AE8CDB0933D71E8C94E04A25619DCEE3D2261AD2EE6BF12FFA06D98A0864D87602733EC86A64521F2B18177B200C",
    "BBE117577A615D6C770988C0BAD946E208E24FA074E5AB3143DB5BFCE0FD108E4B82D120A93AD2CAFFFFFFFFFFFFFFFF",
);

fn group() -> &'static (BigUint, BigUint) {
    static GROUP: std::sync::LazyLock<(BigUint, BigUint)> = std::sync::LazyLock::new(|| (BigUint::parse_bytes(N_3072.as_bytes(), 16).expect("a valid prime"), BigUint::from(5u32)));
    &GROUP
}

fn h(parts: &[&[u8]]) -> Vec<u8> {
    let mut d = Sha512::new();
    for p in parts {
        d.update(p);
    }
    d.finalize().to_vec()
}

/// `n` as big-endian bytes, as long as the group's prime.
fn pad(n: &BigUint, len: usize) -> Vec<u8> {
    let b = n.to_bytes_be();
    let mut out = vec![0; len.saturating_sub(b.len())];
    out.extend_from_slice(&b);
    out
}

/// What the client sends in M3 and keeps: its public key, its proof, the proof the
/// speaker should answer with, and the shared key.
pub struct Proofs {
    pub a_pub: Vec<u8>,
    pub m1: Vec<u8>,
    pub m2: Vec<u8>,
    pub key: Vec<u8>,
}

/// SRP-6a over RFC 5054's 3072-bit group with SHA-512, as HomeKit does it, given the
/// speaker's salt and public key (M2) and our secret.
pub fn srp_client(salt: &[u8], b_pub: &[u8], secret: &[u8]) -> Option<Proofs> {
    let (n, g) = group();
    let len = n.to_bytes_be().len();
    let b = BigUint::from_bytes_be(b_pub);
    if (&b % n) == BigUint::ZERO {
        return None;
    }
    let a = BigUint::from_bytes_be(secret);
    let a_pub = g.modpow(&a, n);
    let k = BigUint::from_bytes_be(&h(&[&n.to_bytes_be(), &pad(g, len)]));
    let u = BigUint::from_bytes_be(&h(&[&pad(&a_pub, len), &pad(&b, len)]));
    let x = BigUint::from_bytes_be(&h(&[salt, &h(&[USER, b":", PIN])]));
    // S = (B - k·g^x)^(a + u·x), kept positive mod N.
    let kgx = (k * g.modpow(&x, n)) % n;
    let base = ((&b % n) + n - kgx) % n;
    let s = base.modpow(&(&a + &u * &x), n);
    let key = h(&[&s.to_bytes_be()]);
    let hn = h(&[&n.to_bytes_be()]);
    let hg = h(&[&g.to_bytes_be()]);
    let hn_xor_hg: Vec<u8> = hn.iter().zip(&hg).map(|(a, b)| a ^ b).collect();
    let a_bytes = a_pub.to_bytes_be();
    let m1 = h(&[&hn_xor_hg, &h(&[USER]), salt, &a_bytes, &b.to_bytes_be(), &key]);
    let m2 = h(&[&a_bytes, &m1, &key]);
    Some(Proofs { a_pub: a_bytes, m1, m2, key })
}

/// The RTSP connection's encryption once paired: each way in blocks of up to 1024 bytes,
/// each its length (2 bytes, little-endian, authenticated), ciphertext and tag.
pub struct Cipher {
    write: ChaCha20Poly1305,
    read: ChaCha20Poly1305,
    written: u64,
    read_count: u64,
}

const BLOCK: usize = 1024;
const TAG: usize = 16;

fn nonce(n: u64) -> [u8; 12] {
    let mut out = [0u8; 12];
    out[4..].copy_from_slice(&n.to_le_bytes());
    out
}

fn derive(key: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    hkdf::Hkdf::<Sha512>::new(Some(salt), key).expand(info, &mut out).expect("32 bytes is a valid length");
    out
}

impl Cipher {
    /// The RTSP connection's.
    pub fn new(shared: &[u8]) -> Self {
        let write = derive(shared, b"Control-Salt", b"Control-Write-Encryption-Key");
        let read = derive(shared, b"Control-Salt", b"Control-Read-Encryption-Key");
        Self::with(write, read)
    }

    /// The event channel's, which runs the other way (the speaker asks, we answer): what
    /// it sends opens with its "write" key, and we answer with its "read" one.
    pub fn events(shared: &[u8]) -> Self {
        let write = derive(shared, b"Events-Salt", b"Events-Read-Encryption-Key");
        let read = derive(shared, b"Events-Salt", b"Events-Write-Encryption-Key");
        Self::with(write, read)
    }

    fn with(write: [u8; 32], read: [u8; 32]) -> Self {
        Self { write: ChaCha20Poly1305::new(&write.into()), read: ChaCha20Poly1305::new(&read.into()), written: 0, read_count: 0 }
    }

    pub fn seal(&mut self, plain: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(plain.len() + plain.len().div_ceil(BLOCK) * (2 + TAG));
        for chunk in plain.chunks(BLOCK) {
            let aad = (chunk.len() as u16).to_le_bytes();
            let sealed = self.write.encrypt(&nonce(self.written).into(), Payload { msg: chunk, aad: &aad }).expect("encrypting can't fail");
            self.written += 1;
            out.extend_from_slice(&aad);
            out.extend_from_slice(&sealed);
        }
        out
    }

    /// Decrypts the whole blocks at the front of `raw` (taking them out of it) onto `plain`.
    pub fn open(&mut self, raw: &mut Vec<u8>, plain: &mut Vec<u8>) -> Result<(), ()> {
        while raw.len() >= 2 {
            let len = u16::from_le_bytes([raw[0], raw[1]]) as usize;
            if raw.len() < 2 + len + TAG {
                break;
            }
            let opened = self.read.decrypt(&nonce(self.read_count).into(), Payload { msg: &raw[2..2 + len + TAG], aad: &raw[..2] }).map_err(|_| ())?;
            self.read_count += 1;
            plain.extend_from_slice(&opened);
            raw.drain(..2 + len + TAG);
        }
        Ok(())
    }

    /// The other end's half: for tests, a speaker's view.
    #[cfg(test)]
    pub fn speaker(shared: &[u8]) -> Self {
        let mut c = Self::new(shared);
        std::mem::swap(&mut c.write, &mut c.read);
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_values_split_and_join_again() {
        let long = vec![7u8; 384];
        let data = tlv_encode(&[(STATE, &[3]), (PUBLIC_KEY, &long), (PROOF, &[1, 2])]);
        // 255 + 129 bytes of key in two items.
        assert_eq!(data.len(), 3 + 2 + 255 + 2 + 129 + 4);
        let items = tlv_decode(&data);
        assert_eq!(items[&STATE], [3]);
        assert_eq!(items[&PUBLIC_KEY], long);
        assert_eq!(items[&PROOF], [1, 2]);
    }

    /// The speaker's side of SRP, as RFC 5054 has it, to check ours against.
    fn srp_server(salt: &[u8], secret_b: &[u8], a_pub: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let (n, g) = group();
        let len = n.to_bytes_be().len();
        let x = BigUint::from_bytes_be(&h(&[salt, &h(&[USER, b":", PIN])]));
        let v = g.modpow(&x, n);
        let k = BigUint::from_bytes_be(&h(&[&n.to_bytes_be(), &pad(g, len)]));
        let b = BigUint::from_bytes_be(secret_b);
        let b_pub = (k * &v + g.modpow(&b, n)) % n;
        let a = BigUint::from_bytes_be(a_pub);
        let u = BigUint::from_bytes_be(&h(&[&pad(&a, len), &pad(&b_pub, len)]));
        let s = (a * v.modpow(&u, n)).modpow(&b, n);
        (b_pub.to_bytes_be(), h(&[&s.to_bytes_be()]))
    }

    #[test]
    fn both_sides_of_srp_come_to_the_same_key() {
        let salt = [9u8; 16];
        let secret_a = [0x42u8; 32];
        let secret_b = [0x17u8; 32];
        let a_pub = BigUint::from_bytes_be(&secret_a);
        let (n, g) = group();
        let a_pub = g.modpow(&a_pub, n).to_bytes_be();
        let (b_pub, server_key) = srp_server(&salt, &secret_b, &a_pub);
        let ours = srp_client(&salt, &b_pub, &secret_a).unwrap();
        assert_eq!(ours.a_pub, a_pub);
        assert_eq!(ours.key, server_key);
        assert_eq!(ours.m1.len(), 64);
    }

    #[test]
    fn the_group_is_rfc_5054s_3072_bit_one() {
        let (n, g) = group();
        assert_eq!(n.bits(), 3072);
        assert_eq!(*g, BigUint::from(5u32));
        // Like all of RFC 3526's and 5054's primes: 64 ones, then π's digits, ending in 64 ones.
        let hex = n.to_str_radix(16).to_uppercase();
        assert!(hex.starts_with("FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD1"));
        assert!(hex.ends_with("FFFFFFFFFFFFFFFF"));
    }

    #[test]
    fn a_public_key_of_zero_is_refused() {
        let n = group().0.to_bytes_be();
        assert!(srp_client(&[0; 16], &n, &[1; 32]).is_none());
    }

    #[test]
    fn sealed_blocks_open_on_the_other_side_in_pieces() {
        let mut ours = Cipher::new(&[5; 64]);
        let mut theirs = Cipher::speaker(&[5; 64]);
        let message = vec![b'x'; 2500];
        let sealed = ours.seal(&message);
        assert_eq!(sealed.len(), 2500 + 3 * (2 + TAG));
        // Arriving a bit at a time, as TCP may give it.
        let (mut raw, mut plain) = (Vec::new(), Vec::new());
        for piece in sealed.chunks(700) {
            raw.extend_from_slice(piece);
            theirs.open(&mut raw, &mut plain).unwrap();
        }
        assert_eq!(plain, message);
        assert!(raw.is_empty());
        // A second message carries on the count.
        let mut raw = ours.seal(b"OK");
        let mut plain = Vec::new();
        theirs.open(&mut raw, &mut plain).unwrap();
        assert_eq!(plain, b"OK");
    }

    #[test]
    fn a_tampered_block_is_refused() {
        let mut ours = Cipher::new(&[5; 64]);
        let mut theirs = Cipher::speaker(&[5; 64]);
        let mut raw = ours.seal(b"RTSP/1.0 200 OK");
        raw[5] ^= 1;
        assert!(theirs.open(&mut raw, &mut Vec::new()).is_err());
    }
}
