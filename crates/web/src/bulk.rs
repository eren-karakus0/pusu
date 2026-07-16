//! BULK REST istemcisi (tarayıcı fetch'i).
//!
//! Onboarding tx'leri (createSubAccount, `abc`) doğrudan borsaya gidiyor —
//! kullanıcı imzalıyor, biz POST ediyoruz, PUSU sunucusu araya girmiyor.
//!
//! ⚠️ **`Ok` ≠ başarı.** Borsa reddedilen işlemde de HTTP 200 + `{"status":
//! "ok"}` dönüyor; gerçek sonuç `statuses` dizisinde (`rejectedInvalid` vb.).
//! `post_order` ham yanıtı verir; [`is_rejected`] onu yorumlar.

use crate::config::BULK_URL;
use serde_json::Value;

#[derive(Debug, Clone)]
pub enum BulkError {
    /// Ağ/fetch hatası.
    Network(String),
    /// Borsa işlemi reddetti (Ok≠başarı; gerçek sebep burada).
    Rejected(String),
    /// Yanıt beklenen şekilde değil.
    Shape(String),
}

impl std::fmt::Display for BulkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(m) => write!(f, "ağ hatası: {m}"),
            Self::Rejected(m) => write!(f, "borsa reddetti: {m}"),
            Self::Shape(m) => write!(f, "beklenmeyen yanıt: {m}"),
        }
    }
}

/// İmzalı gövdeyi `/order`'a POST et, ham JSON yanıtı döndür.
pub async fn post_order(body: &Value) -> Result<Value, BulkError> {
    let resp = gloo_net::http::Request::post(&format!("{BULK_URL}/order"))
        .json(body)
        .map_err(|e| BulkError::Network(e.to_string()))?
        .send()
        .await
        .map_err(|e| BulkError::Network(e.to_string()))?;
    resp.json::<Value>()
        .await
        .map_err(|e| BulkError::Shape(e.to_string()))
}

/// `statuses` dizisinde bir reddedilme var mı? Varsa sebebi döndürür.
///
/// Tek dürüst başarı işareti bu — HTTP 200 ve `status:"ok"` değil.
pub fn rejection_reason(resp: &Value) -> Option<String> {
    let statuses = resp["response"]["data"]["statuses"].as_array()?;
    for s in statuses {
        if let Some(obj) = s.as_object() {
            for key in obj.keys() {
                if key.contains("reject") || key.contains("error") {
                    let reason = s[key]["reason"].as_str().unwrap_or(key);
                    return Some(reason.to_string());
                }
            }
        }
    }
    None
}

/// POST et ve reddi hataya çevir. Onboarding adımları bunu kullanır.
///
/// Not: reddetme yorumunun kapsamlı test edilmiş kanonik hali
/// `pusu-engine::interpret` (gerçek staging yanıtlarıyla). Burası onun
/// onboarding yanıtları için sadeleştirilmiş karşılığı; web crate'i yalnız
/// wasm'a derlendiği için birim testi burada koşamıyor.
pub async fn submit(body: &Value) -> Result<Value, BulkError> {
    let resp = post_order(body).await?;
    match rejection_reason(&resp) {
        Some(reason) => Err(BulkError::Rejected(reason)),
        None => Ok(resp),
    }
}
