//! The RSA key the computer shows the phone, as `adb` does: the phone asks to "Allow USB
//! debugging" for this computer's key and remembers it. The key lives in a file of its own, owner
//! only, and is made on first use.

use base64ct::{Base64, Encoding};
use rsa::pkcs1::{DecodeRsaPrivateKey, EncodeRsaPrivateKey};
use rsa::traits::PublicKeyParts;
use rsa::{Pkcs1v15Sign, PrivateKey};
use sha1::Sha1;
use std::io::{self, Write};
use std::path::Path;

/// adb's keys are 2048 bits, and so is the format it puts the public key in.
const BITS: usize = 2048;
const WORDS: usize = BITS / 32;

pub struct AdbKey(PrivateKey);

fn other(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

impl AdbKey {
    /// The key in `path`, made if it is missing. A file other users can read is refused.
    pub fn load_or_create(path: &Path) -> io::Result<Self> {
        match std::fs::File::open(path) {
            Ok(mut file) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    // On the opened file, so it is the one that is read.
                    if file.metadata()?.permissions().mode() & 0o077 != 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            format!(
                                "{} is open to other users; run chmod 600 on it",
                                path.display()
                            ),
                        ));
                    }
                }
                // A key is a few hundred bytes; the cap only keeps a wrong file from being read whole.
                let mut der = zeroize::Zeroizing::new(Vec::new());
                std::io::Read::read_to_end(
                    &mut std::io::Read::take(&mut file, 16 * 1024),
                    &mut der,
                )?;
                let key = PrivateKey::from_pkcs1_der(&der).map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "the ADB key file is damaged; remove it",
                    )
                })?;
                if key.size() != BITS / 8
                    || key.e().to_be_bytes_trimmed_vartime().as_ref() != [1, 0, 1]
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "the ADB key is not a 2048-bit key with exponent 65537; remove it",
                    ));
                }
                Ok(Self(key))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let key = PrivateKey::new(&mut getrandom::SysRng, BITS).map_err(other)?;
                let der = key.to_pkcs1_der().map_err(other)?;
                // Written under a name of our own and linked into place: a hard link never
                // replaces a file, so if another kite made the key first, that one is used, and
                // nobody ever reads a half-written key.
                // Unique within this process too: two threads may both find the key missing.
                static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let tmp = path.with_extension(format!("tmp{}-{n}", std::process::id()));
                let _ = std::fs::remove_file(&tmp);
                let mut opts = std::fs::OpenOptions::new();
                opts.write(true).create_new(true);
                #[cfg(unix)]
                std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
                let mut file = opts.open(&tmp)?;
                file.write_all(der.as_bytes())?;
                file.sync_all()?;
                let linked = std::fs::hard_link(&tmp, path);
                let _ = std::fs::remove_file(&tmp);
                match linked {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                        return Self::load_or_create(path)
                    }
                    Err(e) => return Err(e),
                }
                Ok(Self(key))
            }
            Err(e) => Err(e),
        }
    }

    /// The signature the phone checks for a token it sent. The token is already the digest: adb
    /// signs it as a SHA-1 hash without hashing it again.
    pub fn sign(&self, token: &[u8]) -> io::Result<Vec<u8>> {
        self.0
            .sign(Pkcs1v15Sign::new::<Sha1>(), token)
            .map_err(other)
    }

    /// The public key in the form the phone wants it for its prompt: adb's own structure,
    /// base64, a name for the user to recognise, and a zero byte.
    pub fn public_payload(&self) -> Vec<u8> {
        let n = self.0.n().to_be_bytes_trimmed_vartime();
        let mut text = Base64::encode_string(&adb_public_key(&n));
        text.push_str(" kite@computer");
        let mut out = text.into_bytes();
        out.push(0);
        out
    }
}

/// adb's `RSAPublicKey`: the number of words, `-1/n[0]` mod 2^32, the modulus and `R^2 mod n` as
/// little-endian words (R = 2^bits), and the exponent 65537. `n` is big-endian, `WORDS` long
/// after padding with zeros in front.
fn adb_public_key(n_be: &[u8]) -> Vec<u8> {
    adb_public_key_words(n_be, WORDS)
}

fn adb_public_key_words(n_be: &[u8], words: usize) -> Vec<u8> {
    let mut n = vec![0u32; words];
    for (i, chunk) in n_be.rchunks(4).take(words).enumerate() {
        let mut w = [0u8; 4];
        w[4 - chunk.len()..].copy_from_slice(chunk);
        n[i] = u32::from_be_bytes(w);
    }
    // n[0] is odd, so it has an inverse mod 2^32: Newton's iteration doubles the correct bits.
    let mut inv = n[0];
    for _ in 0..5 {
        inv = inv.wrapping_mul(2u32.wrapping_sub(n[0].wrapping_mul(inv)));
    }
    // R^2 mod n by doubling 1 for 2 * bits steps, subtracting n whenever the value reaches it.
    let mut rr = vec![0u32; words];
    rr[0] = 1;
    for _ in 0..words * 32 * 2 {
        let mut carry = 0;
        for w in rr.iter_mut() {
            let next = *w >> 31;
            *w = (*w << 1) | carry;
            carry = next;
        }
        if carry == 1 || !less(&rr, &n) {
            let mut borrow = 0u64;
            for (w, m) in rr.iter_mut().zip(&n) {
                let d = u64::from(*w)
                    .wrapping_sub(u64::from(*m))
                    .wrapping_sub(borrow);
                *w = d as u32;
                borrow = (d >> 63) & 1;
            }
        }
    }
    let mut out = Vec::with_capacity(4 * (2 * words + 3));
    out.extend_from_slice(&(words as u32).to_le_bytes());
    out.extend_from_slice(&inv.wrapping_neg().to_le_bytes());
    for w in n.iter().chain(&rr) {
        out.extend_from_slice(&w.to_le_bytes());
    }
    out.extend_from_slice(&65537u32.to_le_bytes());
    out
}

/// `a < b` for little-endian words of the same length.
fn less(a: &[u32], b: &[u32]) -> bool {
    for (x, y) in a.iter().zip(b).rev() {
        if x != y {
            return x < y;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_public_key_structure_is_what_adb_expects() {
        // A 64-bit modulus in two words: the numbers can be checked with u128 arithmetic.
        let n: u64 = 0xc3a5_1f27_9d4b_6e81;
        let bytes = adb_public_key_words(&n.to_be_bytes(), 2);
        let word = |i: usize| u32::from_le_bytes(bytes[4 * i..4 * i + 4].try_into().unwrap());
        assert_eq!(word(0), 2);
        // n0inv * n[0] = -1 mod 2^32
        assert_eq!(word(1).wrapping_mul(n as u32), u32::MAX);
        assert_eq!([word(2), word(3)], [n as u32, (n >> 32) as u32]);
        let r = (1u128 << 64) % u128::from(n);
        let rr = (r * r) % u128::from(n);
        assert_eq!([word(4), word(5)], [rr as u32, (rr >> 32) as u32]);
        assert_eq!(word(6), 65537);
        assert_eq!(bytes.len(), 4 * 7);
    }

    #[test]
    fn signs_a_token_as_a_digest_and_keeps_its_key() {
        let dir = std::env::temp_dir().join(format!("kite-adbkey-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("adbkey");
        let key = AdbKey::load_or_create(&path).unwrap();
        let token = [7u8; 20];
        let sig = key.sign(&token).unwrap();
        assert_eq!(sig.len(), 256);
        // The signature verifies over the token as a ready-made SHA-1 digest.
        key.0
            .to_public_key()
            .verify(Pkcs1v15Sign::new::<Sha1>(), &token, &sig)
            .unwrap();
        // The same file gives the same key again.
        let again = AdbKey::load_or_create(&path).unwrap();
        assert_eq!(again.public_payload(), key.public_payload());
        let payload = key.public_payload();
        assert_eq!(payload.last(), Some(&0));
        assert!(payload.ends_with(b" kite@computer\0"));
        std::fs::write(&path, b"junk").unwrap();
        assert!(AdbKey::load_or_create(&path).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::remove_file(&path).unwrap();
            AdbKey::load_or_create(&path).unwrap();
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(AdbKey::load_or_create(&path).is_err());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
