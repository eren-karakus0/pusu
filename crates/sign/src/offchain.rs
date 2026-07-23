//! **Offchain imza zarfı** — Phantom'un guardrail'ını aşan imza modu.
//!
//! # Neden var
//!
//! `raw` modda imzalanan baytlar `bincode(actions)‖nonce‖account` — Solana
//! transaction'ına benziyor, Phantom `signMessage` bunu **reddediyor**
//! (anti-phishing: "You cannot sign solana transactions using sign message").
//! Kullanıcının canlı blocker'ı tam bu. Çözüm sunucu tarafında: BULK'un
//! `x-bulk-sig-mode: offchain` modu. İmzalanan baytlar bir **Solana v0
//! off-chain message zarfı** oluyor; `0xff "solana offchain"` domain ayracı
//! Phantom'un "bu bir transaction" sezgisini durduruyor → imza ALINIYOR.
//!
//! Kaynak: `docs.bulk.trade/api-reference/signing` ve
//! `docs/research/04-signing-blocker.md` (§ÇÖZÜM).
//!
//! # Zarf düzeni
//!
//! ```text
//! 0xff "solana offchain"          (16 bayt)
//! version            0x00         (1)
//! app_domain         32×0x00      (32)
//! format             0x01=UTF-8   (1)
//! signer_count       0x01         (1)
//! signer_pubkey                   (32)
//! payload_len        u16 LE       (2)
//! payload                         (N)
//! ```
//!
//! payload (UTF-8):
//! ```text
//! Bulk Exchange Transaction
//! Account: <base58>
//! Nonce: <u64>
//! Actions: <count>
//! Signable-Hash: <sha256_hex( bincode(actions)‖nonce‖account )>
//! [0] <action_line_0>
//! [1] <action_line_1>
//! ...
//! ```
//!
//! `Signable-Hash`, siparişi kriptografik olarak bağlayan alan; tam olarak
//! `sha256(message_bytes)` — ve `message_bytes`'ı `bulk-keychain::prepare`
//! zaten üretiyor. Bu yüzden zarfın **tamamı** elimizdeki girdilerden kurulabiliyor.
//!
//! # ⚠️ Tek boşluk: `<action_line_N>`
//!
//! Action-line'ların **birebir sunucu formatı dokümante değil.** Sunucu tam
//! zarf baytlarını doğruladığından (chunk/truncate yok) bu satırları BULK'un
//! renderer'ıyla aynı üretmek gerekiyor. O format **offchain modu staging'e
//! deploy olunca ampirik oturacak** (cüzdansız: zarfı kur → raw test keypair'iyle
//! imzala → `x-bulk-sig-mode: offchain` ile POST → kabul edene dek ayıkla).
//! Bu yüzden [`build_envelope`] action-line'ları **parametre** alıyor: zarfın
//! doğrulanabilir iskeleti burada ve test edilmiş; render'ı format netleşince
//! takılacak. Staging'de offchain henüz no-op (bkz. doc 04 § STAGING AMPİRİK).

use sha2::{Digest, Sha256};

/// Solana off-chain message imza domain'i: `0xff` + `"solana offchain"`.
const DOMAIN: &[u8] = b"\xffsolana offchain";
/// payload biçimi: UTF-8.
const FORMAT_UTF8: u8 = 0x01;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum OffchainError {
    #[error("geçersiz signer pubkey: {0}")]
    BadPubkey(String),
    #[error("payload çok uzun: {0} bayt (u16 sınırı 65535)")]
    PayloadTooLong(usize),
}

/// `message_bytes` (= `bincode(actions)‖nonce(8,LE)‖account(32)`) → Signable-Hash.
///
/// Küçük harf hex sha256. Zarfı siparişe bağlayan tek alan bu.
pub fn signable_hash(message_bytes: &[u8]) -> String {
    let digest = Sha256::digest(message_bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for b in digest {
        hex.push(nibble(b >> 4));
        hex.push(nibble(b & 0x0f));
    }
    hex
}

fn nibble(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        _ => (b'a' + (n - 10)) as char,
    }
}

/// İnsan-okunur payload metnini kur (zarfın gövdesi).
pub fn build_payload(
    message_bytes: &[u8],
    account_b58: &str,
    nonce: u64,
    action_lines: &[String],
) -> String {
    let mut lines = vec![
        "Bulk Exchange Transaction".to_string(),
        format!("Account: {account_b58}"),
        format!("Nonce: {nonce}"),
        format!("Actions: {}", action_lines.len()),
        format!("Signable-Hash: {}", signable_hash(message_bytes)),
    ];
    for (i, line) in action_lines.iter().enumerate() {
        lines.push(format!("[{i}] {line}"));
    }
    lines.join("\n")
}

/// Phantom'un `signMessage` ile imzalayacağı tam offchain zarf baytlarını kur.
///
/// - `message_bytes`: `prepare`'ın verdiği ham baytlar (Signable-Hash bundan).
/// - `account_b58`: emrin hesabı (sub-account).
/// - `signer_b58`: imzalayan (master) — zarfın `signer_pubkey` alanı.
/// - `action_lines`: ⚠️ sunucu formatıyla birebir olmalı (bkz. modül dokümanı).
pub fn build_envelope(
    message_bytes: &[u8],
    account_b58: &str,
    nonce: u64,
    signer_b58: &str,
    action_lines: &[String],
) -> Result<Vec<u8>, OffchainError> {
    let signer = bs58::decode(signer_b58)
        .into_vec()
        .ok()
        .filter(|v| v.len() == 32)
        .ok_or_else(|| OffchainError::BadPubkey(signer_b58.to_string()))?;

    let payload = build_payload(message_bytes, account_b58, nonce, action_lines);
    let payload_bytes = payload.as_bytes();
    let payload_len: u16 = payload_bytes
        .len()
        .try_into()
        .map_err(|_| OffchainError::PayloadTooLong(payload_bytes.len()))?;

    let mut out = Vec::with_capacity(53 + payload_bytes.len());
    out.extend_from_slice(DOMAIN); // 16
    out.push(0x00); // version
    out.extend_from_slice(&[0u8; 32]); // app_domain
    out.push(FORMAT_UTF8); // format
    out.push(0x01); // signer_count
    out.extend_from_slice(&signer); // signer_pubkey (32)
    out.extend_from_slice(&payload_len.to_le_bytes()); // payload_len u16 LE
    out.extend_from_slice(payload_bytes);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Bilinen vektör: sha256("") — hash boru hattının doğruluğu.
    const SHA256_EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    // 32 baytlık geçerli bir base58 pubkey (bulk-keychain testleriyle aynı tarz).
    fn signer_b58() -> String {
        bs58::encode([7u8; 32]).into_string()
    }

    #[test]
    fn signable_hash_bilinen_vektorle_eslesiyor() {
        assert_eq!(signable_hash(b""), SHA256_EMPTY);
        // Non-boş girdi de 64 hex karakter.
        assert_eq!(signable_hash(b"bulk").len(), 64);
    }

    #[test]
    fn payload_baslik_satirlarini_dogru_koyuyor() {
        let p = build_payload(b"msg", "AcctBase58", 42, &["market buy".into()]);
        let mut it = p.lines();
        assert_eq!(it.next(), Some("Bulk Exchange Transaction"));
        assert_eq!(it.next(), Some("Account: AcctBase58"));
        assert_eq!(it.next(), Some("Nonce: 42"));
        assert_eq!(it.next(), Some("Actions: 1"));
        assert_eq!(
            it.next(),
            Some(format!("Signable-Hash: {}", signable_hash(b"msg")).as_str())
        );
        assert_eq!(it.next(), Some("[0] market buy"));
        assert_eq!(it.next(), None);
    }

    #[test]
    fn zarf_byte_duzeni_spec_ile_ayni() {
        let signer = signer_b58();
        let action_lines = vec!["a0".to_string(), "a1".to_string()];
        let env = build_envelope(b"raw-bytes", "Acct", 9, &signer, &action_lines).unwrap();

        // 0xff + "solana offchain"
        assert_eq!(&env[..16], b"\xffsolana offchain");
        assert_eq!(env[16], 0x00, "version");
        assert_eq!(&env[17..49], &[0u8; 32], "app_domain 32×0x00");
        assert_eq!(env[49], FORMAT_UTF8, "format UTF-8");
        assert_eq!(env[50], 0x01, "signer_count");
        assert_eq!(&env[51..83], &[7u8; 32], "signer_pubkey");

        let plen = u16::from_le_bytes([env[83], env[84]]) as usize;
        let payload = &env[85..];
        assert_eq!(plen, payload.len(), "payload_len gerçek uzunlukla tutmalı");

        // payload içeriği build_payload ile aynı.
        let expected = build_payload(b"raw-bytes", "Acct", 9, &action_lines);
        assert_eq!(payload, expected.as_bytes());
        // Actions sayısı action_lines'a eşit.
        assert!(expected.contains("Actions: 2"));
    }

    #[test]
    fn bozuk_signer_reddedilir() {
        let e = build_envelope(b"x", "Acct", 1, "not-base58-!!", &[]).unwrap_err();
        assert!(matches!(e, OffchainError::BadPubkey(_)));
        // Doğru base58 ama 32 bayt değil → yine ret.
        let short = bs58::encode([1u8; 16]).into_string();
        assert!(matches!(
            build_envelope(b"x", "Acct", 1, &short, &[]),
            Err(OffchainError::BadPubkey(_))
        ));
    }

    #[test]
    fn sifir_action_gecerli_zarf() {
        // Yalnız başlık (onboarding tx'i tek aksiyon olsa da sıfır da kurulabilmeli).
        let env = build_envelope(b"m", "Acct", 0, &signer_b58(), &[]).unwrap();
        assert_eq!(&env[..16], b"\xffsolana offchain");
        let plen = u16::from_le_bytes([env[83], env[84]]) as usize;
        assert_eq!(plen, env.len() - 85);
    }
}
