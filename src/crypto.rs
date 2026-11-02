//! Kriptografi, Windows CNG (bcrypt) üzerinde: rastgele sayı, HMAC-SHA256, HKDF-SHA256 (RFC 5869),
//! ECDH P-256 ve AES-256-GCM. Mac tarafı aynı işlemleri CryptoKit ile yapar; açık anahtarlar X9.63
//! (sıkıştırılmamış, 65 bayt) biçiminde, ortak sır ise büyük uçlu (big-endian) X koordinatıdır.

use windows::Win32::Security::Cryptography::*;

/// Sistemin güvenli rastgele kaynağı; çalışmıyorsa tahmin edilebilir bir değerle devam edilmez.
pub fn random<const N: usize>() -> Option<[u8; N]> {
    let mut bytes = [0u8; N];
    let status = unsafe { BCryptGenRandom(BCRYPT_ALG_HANDLE::default(), &mut bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    status.is_ok().then_some(bytes)
}

pub fn hmac(key: &[u8], msg: &[u8]) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    let status = unsafe { BCryptHash(BCRYPT_HMAC_SHA256_ALG_HANDLE, Some(key), msg, &mut out) };
    status.is_ok().then_some(out)
}

pub fn sha256(msg: &[u8]) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    let status = unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, msg, &mut out) };
    status.is_ok().then_some(out)
}

/// HKDF-SHA256; `salt` boşsa RFC 5869'daki gibi 32 sıfır bayt.
pub fn hkdf(ikm: &[u8], salt: &[u8], info: &[u8], len: usize) -> Option<Vec<u8>> {
    let zero = [0u8; 32];
    let prk = hmac(if salt.is_empty() { &zero } else { salt }, ikm)?;
    let mut out = Vec::with_capacity(len);
    let mut t: Vec<u8> = Vec::new();
    let mut i = 1u8;
    while out.len() < len {
        let mut msg = t.clone();
        msg.extend_from_slice(info);
        msg.push(i);
        t = hmac(&prk, &msg)?.to_vec();
        out.extend_from_slice(&t);
        i = i.checked_add(1)?;
    }
    out.truncate(len);
    Some(out)
}

/// Sabit sürede karşılaştırma.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// Oturum başına üretilen geçici ECDH P-256 anahtarı.
pub struct EcdhKey(BCRYPT_KEY_HANDLE);

impl EcdhKey {
    pub fn generate() -> Option<EcdhKey> {
        let mut h = BCRYPT_KEY_HANDLE::default();
        unsafe {
            BCryptGenerateKeyPair(BCRYPT_ECDH_P256_ALG_HANDLE, &mut h, 256, 0).ok().ok()?;
            let key = EcdhKey(h);
            BCryptFinalizeKeyPair(key.0, 0).ok().ok()?;
            Some(key)
        }
    }

    /// X9.63 biçimi: 0x04 || X || Y.
    pub fn public_x963(&self) -> Option<[u8; 65]> {
        let mut blob = [0u8; 72];
        let mut len = 0u32;
        unsafe {
            BCryptExportKey(self.0, BCRYPT_KEY_HANDLE::default(), BCRYPT_ECCPUBLIC_BLOB, Some(&mut blob), &mut len, 0)
                .ok()
                .ok()?;
        }
        if len != 72 {
            return None;
        }
        let mut out = [0u8; 65];
        out[0] = 0x04;
        out[1..].copy_from_slice(&blob[8..72]);
        Some(out)
    }

    /// Karşı tarafın X9.63 açık anahtarıyla ortak sır (X koordinatı, büyük uçlu). Geçersiz
    /// (eğri üzerinde olmayan) anahtar içe aktarılırken reddedilir.
    pub fn agree(&self, peer_x963: &[u8]) -> Option<[u8; 32]> {
        if peer_x963.len() != 65 || peer_x963[0] != 0x04 {
            return None;
        }
        let mut blob = Vec::with_capacity(72);
        blob.extend_from_slice(&BCRYPT_ECDH_PUBLIC_P256_MAGIC.to_le_bytes());
        blob.extend_from_slice(&32u32.to_le_bytes());
        blob.extend_from_slice(&peer_x963[1..]);
        unsafe {
            let mut peer = BCRYPT_KEY_HANDLE::default();
            BCryptImportKeyPair(BCRYPT_ECDH_P256_ALG_HANDLE, BCRYPT_KEY_HANDLE::default(), BCRYPT_ECCPUBLIC_BLOB, &mut peer, &blob, 0)
                .ok()
                .ok()?;
            let peer = EcdhKey(peer);
            let mut secret = BCRYPT_SECRET_HANDLE::default();
            BCryptSecretAgreement(self.0, peer.0, &mut secret, 0).ok().ok()?;
            let mut out = [0u8; 32];
            let mut len = 0u32;
            let status = BCryptDeriveKey(secret, BCRYPT_KDF_RAW_SECRET, None, Some(&mut out), &mut len, 0);
            let _ = BCryptDestroySecret(secret);
            status.ok().ok()?;
            if len != 32 {
                return None;
            }
            // CNG ham sırrı küçük uçlu verir; standart (CryptoKit) gösterim büyük uçlu.
            out.reverse();
            Some(out)
        }
    }
}

impl Drop for EcdhKey {
    fn drop(&mut self) {
        unsafe {
            let _ = BCryptDestroyKey(self.0);
        }
    }
}

/// Tek yönlü AES-256-GCM kanalı. Nonce = 4 sıfır bayt || 64 bitlik sayaç (büyük uçlu); her mesajda
/// bir artar, aynı anahtarla aynı nonce asla kullanılmaz. Her yön kendi anahtarına sahiptir.
pub struct Aead {
    key: BCRYPT_KEY_HANDLE,
    counter: u64,
}

// Anahtar tutamağı tek bir iş parçacığında kullanılır (her yönün okuyucu/yazıcısı).
unsafe impl Send for Aead {}

pub const TAG_LEN: usize = 16;

impl Aead {
    pub fn new(key: &[u8]) -> Option<Aead> {
        if key.len() != 32 {
            return None;
        }
        let mut h = BCRYPT_KEY_HANDLE::default();
        unsafe { BCryptGenerateSymmetricKey(BCRYPT_AES_GCM_ALG_HANDLE, &mut h, None, key, 0).ok().ok()? };
        Some(Aead { key: h, counter: 0 })
    }

    fn next_nonce(&mut self) -> Option<[u8; 12]> {
        let mut n = [0u8; 12];
        n[4..].copy_from_slice(&self.counter.to_be_bytes());
        self.counter = self.counter.checked_add(1)?;
        Some(n)
    }

    /// Şifreli metin || etiket.
    pub fn seal(&mut self, plain: &[u8]) -> Option<Vec<u8>> {
        let mut nonce = self.next_nonce()?;
        let mut tag = [0u8; TAG_LEN];
        let info = auth_info(&mut nonce, &mut tag);
        let mut out = vec![0u8; plain.len() + TAG_LEN];
        let mut len = 0u32;
        unsafe {
            BCryptEncrypt(
                self.key,
                Some(plain),
                Some(&info as *const _ as *const core::ffi::c_void),
                None,
                Some(&mut out[..plain.len()]),
                &mut len,
                BCRYPT_FLAGS(0),
            )
            .ok()
            .ok()?;
        }
        out[plain.len()..].copy_from_slice(&tag);
        Some(out)
    }

    /// Etiket doğrulanmazsa (bozulmuş ya da sahte veri) `None`.
    pub fn open(&mut self, data: &[u8]) -> Option<Vec<u8>> {
        if data.len() < TAG_LEN {
            return None;
        }
        let (cipher, tag) = data.split_at(data.len() - TAG_LEN);
        let mut nonce = self.next_nonce()?;
        let mut tag: [u8; TAG_LEN] = tag.try_into().ok()?;
        let info = auth_info(&mut nonce, &mut tag);
        let mut out = vec![0u8; cipher.len()];
        let mut len = 0u32;
        unsafe {
            BCryptDecrypt(
                self.key,
                Some(cipher),
                Some(&info as *const _ as *const core::ffi::c_void),
                None,
                Some(&mut out),
                &mut len,
                BCRYPT_FLAGS(0),
            )
            .ok()
            .ok()?;
        }
        Some(out)
    }
}

fn auth_info(nonce: &mut [u8; 12], tag: &mut [u8; TAG_LEN]) -> BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO {
    BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO {
        cbSize: std::mem::size_of::<BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO>() as u32,
        dwInfoVersion: 1,
        pbNonce: nonce.as_mut_ptr(),
        cbNonce: 12,
        pbTag: tag.as_mut_ptr(),
        cbTag: TAG_LEN as u32,
        ..Default::default()
    }
}

impl Drop for Aead {
    fn drop(&mut self) {
        unsafe {
            let _ = BCryptDestroyKey(self.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hkdf_rfc5869_case_1() {
        let ikm = [0x0bu8; 22];
        let salt: Vec<u8> = (0x00..=0x0c).collect();
        let info: Vec<u8> = (0xf0..=0xf9).collect();
        let okm = hkdf(&ikm, &salt, &info, 42).unwrap();
        assert_eq!(hex(&okm), "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865");
    }

    #[test]
    fn sha256_known() {
        assert_eq!(hex(&sha256(b"abc").unwrap()), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn hmac_rfc4231_case_2() {
        let mac = hmac(b"Jefe", b"what do ya want for nothing?").unwrap();
        assert_eq!(hex(&mac), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
    }

    #[test]
    fn ecdh_both_sides_agree() {
        let (a, b) = (EcdhKey::generate().unwrap(), EcdhKey::generate().unwrap());
        let (pa, pb) = (a.public_x963().unwrap(), b.public_x963().unwrap());
        assert_eq!(a.agree(&pb).unwrap(), b.agree(&pa).unwrap());
        let mut bad = pb;
        bad[10] ^= 1; // eğri dışı nokta
        assert!(a.agree(&bad).is_none());
    }

    #[test]
    fn aes_gcm_round_trip_and_tamper() {
        let key = [7u8; 32];
        let (mut tx, mut rx) = (Aead::new(&key).unwrap(), Aead::new(&key).unwrap());
        let c1 = tx.seal(b"merhaba").unwrap();
        let c2 = tx.seal(b"merhaba").unwrap();
        assert_ne!(c1, c2, "her mesaj yeni nonce");
        assert_eq!(rx.open(&c1).unwrap(), b"merhaba");
        let mut t = c2.clone();
        t[0] ^= 1;
        assert!(rx.open(&t).is_none());
    }

    #[test]
    fn aes_gcm_known_vector() {
        // NIST GCM test 14 (256 bit anahtar sıfır, nonce sıfır, 16 sıfır bayt).
        let mut tx = Aead::new(&[0u8; 32]).unwrap();
        let c = tx.seal(&[0u8; 16]).unwrap();
        assert_eq!(hex(&c), "cea7403d4d606b6e074ec5d3baf39d18d0d1c8a799996bf0265b98b5d48ab919");
    }
}
