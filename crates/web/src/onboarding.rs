//! Onboarding akışı: sub-account aç → builder onayı.
//!
//! Her adım aynı desende: `pusu_sign::prepare_*` ile cüzdanın imzalayacağı
//! mesajı üret → cüzdana imzalat → `finalize` → BULK'a POST. Sunucu (PUSU)
//! araya girmiyor; kullanıcı doğrudan borsayla konuşuyor, biz sadece istemci
//! mantığıyız.

use crate::config::{BUILDER_FEE_BPS, BUILDER_PUBKEY};
use crate::{bulk, wallet};
use pusu_sign::{finalize_one, prepare_approve_builder, prepare_create_subaccount};
use serde_json::Value;

#[derive(Debug, Clone)]
pub enum FlowError {
    Wallet(String),
    Sign(String),
    Bulk(String),
    Shape(String),
}

impl std::fmt::Display for FlowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wallet(m) => write!(f, "{m}"),
            Self::Sign(m) => write!(f, "imza hazırlanamadı: {m}"),
            Self::Bulk(m) => write!(f, "{m}"),
            Self::Shape(m) => write!(f, "{m}"),
        }
    }
}

/// Tarayıcı saati — nonce kaynağı (bulk-keychain'in beklediği unix ms).
fn now_ms() -> u64 {
    js_sys::Date::now() as u64
}

/// Sub-account aç, oluşan sub pubkey'ini döndür.
///
/// Master'a asla dokunmuyoruz: kullanıcı riske atacağı miktarı bu sub'a koyar,
/// zarar tavanı o kadar (§7).
pub async fn create_subaccount(master: &str, name: &str, margin: f64) -> Result<String, FlowError> {
    let prepared = prepare_create_subaccount(name, Some(margin), master, now_ms())
        .map_err(|e| FlowError::Sign(e.to_string()))?;
    let sig = wallet::sign_message(&prepared.message_bytes)
        .await
        .map_err(|e| FlowError::Wallet(e.to_string()))?;
    let body = finalize_one(prepared, &sig);
    let resp = bulk::submit(&body)
        .await
        .map_err(|e| FlowError::Bulk(e.to_string()))?;
    extract_sub(&resp)
        .ok_or_else(|| FlowError::Shape("sub-account pubkey yanıtta bulunamadı".into()))
}

/// Builder onayı (`abc`, fee=2). Onayladığın = kestiğimiz.
pub async fn approve_builder(master: &str) -> Result<(), FlowError> {
    let prepared = prepare_approve_builder(BUILDER_PUBKEY, BUILDER_FEE_BPS, master, now_ms())
        .map_err(|e| FlowError::Sign(e.to_string()))?;
    let sig = wallet::sign_message(&prepared.message_bytes)
        .await
        .map_err(|e| FlowError::Wallet(e.to_string()))?;
    let body = finalize_one(prepared, &sig);
    bulk::submit(&body)
        .await
        .map_err(|e| FlowError::Bulk(e.to_string()))?;
    Ok(())
}

/// createSubAccount yanıtından yeni sub pubkey'ini çıkar.
///
/// ⚠️ Kesin yol Faz-2'de canlı doğrulanacak; şimdilik `statuses` içinde `sub`
/// anahtarını tarıyoruz (spike S4'te bu şekilde göründü).
fn extract_sub(resp: &Value) -> Option<String> {
    let statuses = resp["response"]["data"]["statuses"].as_array()?;
    statuses
        .iter()
        .find_map(|s| s.get("sub").and_then(Value::as_str).map(String::from))
}
