//! Bildirim teslimi: outbox'taki Notify bildirimlerini e-posta (Resend) ve
//! Telegram'a gönderir.
//!
//! In-app teslim her zaman açık (api `/notifications` sunuyor). Bu modül
//! **ek** kanallar: her kanal ilgili ortam değişkeni varsa açık, yoksa sessizce
//! atlanır (`None`). Kayıt outbox'ta `*_sent_at` ile idempotent — bir bildirim
//! bir kanaldan yalnız bir kez gider.
//!
//! ```text
//! watcher → outbox (notifications)
//!            ├─ in-app: api /notifications (her zaman)
//!            ├─ email:  Resend      (PUSU_RESEND_API_KEY varsa)
//!            └─ telegram: Bot API   (PUSU_TELEGRAM_BOT_TOKEN varsa)
//! ```

use pusu_store::{PendingDelivery, Store};
use serde_json::{json, Value};
use tracing::warn;

/// Teslim yapılandırması — ortamdan. Anahtar yoksa o kanal kapalı.
#[derive(Clone)]
pub struct NotifyConfig {
    /// Resend API anahtarı; yoksa e-posta kapalı.
    pub resend_api_key: Option<String>,
    /// Gönderen adresi (Resend'de doğrulanmış domain olmalı).
    pub email_from: String,
    /// Telegram bot token'ı; yoksa telegram kapalı.
    pub telegram_bot_token: Option<String>,
}

impl NotifyConfig {
    /// Ortamdan oku. Boş string = tanımsız sayılır.
    pub fn from_env() -> Self {
        let nonempty = |k: &str| std::env::var(k).ok().filter(|s| !s.trim().is_empty());
        Self {
            resend_api_key: nonempty("PUSU_RESEND_API_KEY"),
            email_from: std::env::var("PUSU_EMAIL_FROM")
                .unwrap_or_else(|_| "PUSU <alerts@pusu.trade>".to_string()),
            telegram_bot_token: nonempty("PUSU_TELEGRAM_BOT_TOKEN"),
        }
    }

    /// En az bir ek kanal açık mı? Değilse teslim döngüsü hiç başlamasın.
    pub fn any_enabled(&self) -> bool {
        self.resend_api_key.is_some() || self.telegram_bot_token.is_some()
    }
}

/// Bildirim gövdesindeki insan-okur mesaj (`{message}`), yoksa güvenli varsayılan.
fn message_of(body: &Value) -> &str {
    body["message"].as_str().unwrap_or("Your PUSU alert fired")
}

/// E-posta konusu + düz-metin gövdesi.
fn email_parts(body: &Value) -> (String, String) {
    let msg = message_of(body);
    let subject = format!("PUSU alert fired — {msg}");
    let text = format!(
        "{msg}\n\nYour alert condition just held. Open the PUSU terminal to see what happened.\n\n\
         — PUSU · the alarm that pulls the trigger"
    );
    (subject, text)
}

/// Telegram mesaj metni.
fn telegram_text(body: &Value) -> String {
    format!("🔔 PUSU alert fired\n{}", message_of(body))
}

/// Bekleyen teslimatları bir kez işle. Tek bir bildirimin hatası döngüyü
/// durdurmaz; başarısızlar `*_sent_at` NULL kalıp bir sonraki turda tekrar denenir.
pub async fn deliver_pending(store: &Store, cfg: &NotifyConfig, client: &reqwest::Client) {
    if let Some(key) = &cfg.resend_api_key {
        match store.undelivered_email(50).await {
            Ok(items) => {
                for p in items {
                    match send_email(client, key, &cfg.email_from, &p).await {
                        Ok(()) => {
                            let _ = store.mark_email_sent(p.id).await;
                        }
                        Err(e) => warn!(id = p.id, "e-posta teslim edilemedi: {e}"),
                    }
                }
            }
            Err(e) => warn!("undelivered_email hatası: {e}"),
        }
    }

    if let Some(token) = &cfg.telegram_bot_token {
        match store.undelivered_telegram(50).await {
            Ok(items) => {
                for p in items {
                    match send_telegram(client, token, &p).await {
                        Ok(()) => {
                            let _ = store.mark_telegram_sent(p.id).await;
                        }
                        Err(e) => warn!(id = p.id, "telegram teslim edilemedi: {e}"),
                    }
                }
            }
            Err(e) => warn!("undelivered_telegram hatası: {e}"),
        }
    }
}

/// Bir bildirimi Resend ile e-postala.
async fn send_email(
    client: &reqwest::Client,
    api_key: &str,
    from: &str,
    p: &PendingDelivery,
) -> Result<(), String> {
    let (subject, text) = email_parts(&p.body);
    let resp = client
        .post("https://api.resend.com/emails")
        .bearer_auth(api_key)
        .json(&json!({ "from": from, "to": [p.dest], "subject": subject, "text": text }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("resend {}", resp.status()))
    }
}

/// Bir bildirimi Telegram Bot API ile gönder.
async fn send_telegram(
    client: &reqwest::Client,
    token: &str,
    p: &PendingDelivery,
) -> Result<(), String> {
    let url = format!("https://api.telegram.org/bot{token}/sendMessage");
    let resp = client
        .post(&url)
        .json(&json!({ "chat_id": p.dest, "text": telegram_text(&p.body) }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("telegram {}", resp.status()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body() -> Value {
        json!({ "symbol": "BTC-USD", "message": "BTC-USD · hourly close > 90000" })
    }

    #[test]
    fn email_konusu_ve_govdesi_mesaji_icerir() {
        let (subject, text) = email_parts(&body());
        assert!(subject.contains("BTC-USD · hourly close > 90000"));
        assert!(subject.starts_with("PUSU alert fired"));
        assert!(text.contains("BTC-USD · hourly close > 90000"));
    }

    #[test]
    fn telegram_metni_mesaji_icerir() {
        let t = telegram_text(&body());
        assert!(t.contains("PUSU alert fired"));
        assert!(t.contains("BTC-USD · hourly close > 90000"));
    }

    #[test]
    fn mesaj_yoksa_guvenli_varsayilan() {
        let empty = json!({});
        assert_eq!(message_of(&empty), "Your PUSU alert fired");
    }

    #[test]
    fn config_bos_string_kapali_sayar() {
        // Boş ortam değişkeni "tanımsız" gibi — kazara boş anahtarla kanal açılmasın.
        // (from_env ortamı okuduğu için burada yalnız any_enabled mantığını doğruluyoruz.)
        let cfg = NotifyConfig {
            resend_api_key: None,
            email_from: "x".into(),
            telegram_bot_token: None,
        };
        assert!(!cfg.any_enabled());
        let cfg2 = NotifyConfig {
            resend_api_key: Some("re_x".into()),
            ..cfg
        };
        assert!(cfg2.any_enabled());
    }
}
