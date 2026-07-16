//! Ön-imzalı blob payload'larının **at-rest** şifrelemesi (AES-256-GCM).
//!
//! # Neden şifreliyoruz
//!
//! `presigned_blobs.payload` kullanıcının imzaladığı, değiştirilemez emirler.
//! DB sızarsa saldırgan bunları **koşul sağlanmadan** borsaya postalayıp
//! kullanıcıyı istemediği anda/fiyatta işleme sokabilir; ayrıca tüm işlem
//! niyetini (yön, boyut, seviyeler) okur. Nonce idempotency (§8.11) yalnızca
//! *tekrar* göndermeyi eler — henüz gönderilmemiş bir blob'un ilk gönderimini
//! değil. Şifreleme bu ikisini birden kapatıyor.
//!
//! # Anahtar
//!
//! `PUSU_BLOB_KEY` ortam değişkeni: base64 kodlu **32 bayt** (AES-256). Blob
//! yazan (api) ve okuyan (node) süreçler **aynı** anahtarı taşımalı. Anahtar
//! her çağrıda okunuyor; blob işlemleri seyrek olduğu için önbelleğe gerek yok.
//!
//! Format: `nonce(12) ‖ ciphertext(+16 bayt GCM etiketi)`.

use crate::StoreError;
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::prelude::*;
use serde_json::Value;

/// Payload'ı şifrele. `PUSU_BLOB_KEY` gerekli.
pub fn encrypt(payload: &Value) -> Result<Vec<u8>, StoreError> {
    encrypt_with(&key()?, payload)
}

/// Şifreli blob'u çöz. `PUSU_BLOB_KEY` gerekli.
pub fn decrypt(blob: &[u8]) -> Result<Value, StoreError> {
    decrypt_with(&key()?, blob)
}

/// Anahtarın tanımlı ve 32 bayt olduğunu **açılışta** doğrula (fail-fast).
///
/// Blob yazan/okuyan süreçler anahtarsız başlarsa hata ilk blob işleminde,
/// belki gecelerce sonra patlar. Bunu başlangıca çekiyoruz.
pub fn check_key() -> Result<(), StoreError> {
    key().map(|_| ())
}

/// Ortamdan 32 baytlık anahtarı çöz.
fn key() -> Result<[u8; 32], StoreError> {
    let b64 = std::env::var("PUSU_BLOB_KEY")
        .map_err(|_| StoreError::Crypto("PUSU_BLOB_KEY tanımlı değil".into()))?;
    let bytes = BASE64_STANDARD
        .decode(b64.trim())
        .map_err(|_| StoreError::Crypto("PUSU_BLOB_KEY geçerli base64 değil".into()))?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| StoreError::Crypto(format!("PUSU_BLOB_KEY 32 bayt olmalı ({})", bytes.len())))
}

fn encrypt_with(key: &[u8; 32], payload: &Value) -> Result<Vec<u8>, StoreError> {
    let plaintext = serde_json::to_vec(payload)?;
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|_| StoreError::Crypto("anahtar uzunluğu".into()))?;

    let mut nonce_bytes = [0u8; 12];
    getrandom::getrandom(&mut nonce_bytes)
        .map_err(|_| StoreError::Crypto("nonce üretilemedi".into()))?;
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_ref())
        .map_err(|_| StoreError::Crypto("şifreleme başarısız".into()))?;

    let mut out = Vec::with_capacity(12 + ciphertext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

fn decrypt_with(key: &[u8; 32], blob: &[u8]) -> Result<Value, StoreError> {
    if blob.len() < 12 {
        return Err(StoreError::Crypto("şifreli blob çok kısa".into()));
    }
    let (nonce_bytes, ciphertext) = blob.split_at(12);
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|_| StoreError::Crypto("anahtar uzunluğu".into()))?;
    let nonce = Nonce::from_slice(nonce_bytes);

    let plaintext = cipher.decrypt(nonce, ciphertext).map_err(|_| {
        StoreError::Crypto("çözme başarısız (anahtar yanlış ya da veri bozuk)".into())
    })?;
    Ok(serde_json::from_slice(&plaintext)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7u8; 32];

    #[test]
    fn sifrele_coz_ayni_degeri_dondurur() {
        let payload = serde_json::json!({
            "actions": [{ "m": { "c": "BTC-USD", "b": true } }],
            "nonce": 1_784_000_000_000u64,
            "account": "sub-pk",
            "signer": "master-pk",
            "signature": "abc",
        });
        let ct = encrypt_with(&KEY, &payload).unwrap();
        // Şifreli hâl düz metinden farklı (gerçekten şifrelenmiş).
        assert_ne!(ct, serde_json::to_vec(&payload).unwrap());
        assert_eq!(decrypt_with(&KEY, &ct).unwrap(), payload);
    }

    #[test]
    fn her_sifreleme_farkli_ciktivar_uretir() {
        // Rastgele nonce: aynı payload iki kez şifrelenince farklı ciphertext.
        let p = serde_json::json!({ "x": 1 });
        assert_ne!(
            encrypt_with(&KEY, &p).unwrap(),
            encrypt_with(&KEY, &p).unwrap()
        );
    }

    #[test]
    fn yanlis_anahtar_cozemez() {
        let p = serde_json::json!({ "x": 1 });
        let ct = encrypt_with(&KEY, &p).unwrap();
        let wrong = [9u8; 32];
        assert!(decrypt_with(&wrong, &ct).is_err());
    }

    #[test]
    fn bozuk_veri_cozemez() {
        assert!(decrypt_with(&KEY, b"kisa").is_err());
        assert!(decrypt_with(&KEY, &[0u8; 40]).is_err());
    }
}
