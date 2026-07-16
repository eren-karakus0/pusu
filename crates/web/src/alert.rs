//! Alarm kurma mantığı: formdan `Alert` üret, imzala, yönlendir.
//!
//! Akış tek yerde toplanıyor ki UI yalnızca durumu tutsun:
//!
//! ```text
//! form → build_alert → prepare_alert (compile+prepare)
//!      → cüzdan imzası → finalize
//!      → OnChain: BULK'a POST · Watched: PUSU api'ye POST
//! ```

use crate::config::BUILDER_PUBKEY;
use crate::{api, bulk, wallet};
use pusu_core::{
    Alert, AlertAction, AlertId, AlertState, Condition, Cross, Entry, Exits, Interval, Side,
    Symbol, TradeSpec,
};
use pusu_sign::{finalize_bundle, prepare_alert, Routing};
use serde_json::json;

/// Formdan gelen ham girdi (UI string'lerini parse edip dolduruyor).
pub struct Form {
    pub symbol: String,
    /// true → mum kapanışı (Watched); false → anlık mark (OnChain).
    pub use_candle: bool,
    pub interval: Interval,
    /// true → eşiğin üstüne çıkarsa; false → altına inerse.
    pub above: bool,
    pub price: f64,
    pub side: Side,
    pub size: f64,
    pub limit_entry: bool,
    pub limit_price: f64,
    /// 0 → yok. İkisi de doluysa basit stop+hedef (OCO) ekleniyor.
    pub stop: f64,
    pub target: f64,
}

/// Alarmın nereye yerleştiği — kullanıcıya geri bildirim.
pub enum Placed {
    /// Borsaya gönderildi, borsa tutuyor.
    OnChain,
    /// PUSU'ya kaydedildi, watcher izliyor.
    Watched,
}

fn now_ms() -> u64 {
    js_sys::Date::now() as u64
}

/// Çakışmayan bir alarm kimliği: zaman damgası + rastgele.
fn gen_id() -> String {
    let r = (js_sys::Math::random() * 1_000_000.0) as u64;
    format!("{}-{r}", now_ms())
}

/// Formu doğrulayıp `Alert`'e çevir.
pub fn build_alert(f: &Form, owner: &str, account: &str) -> Result<Alert, String> {
    if f.symbol.trim().is_empty() {
        return Err("Sembol boş olamaz.".into());
    }
    if f.size <= 0.0 {
        return Err("Miktar 0'dan büyük olmalı.".into());
    }
    if f.price <= 0.0 {
        return Err("Eşik fiyatı geçersiz.".into());
    }

    let symbol = Symbol::new(f.symbol.trim());
    let cross = if f.above { Cross::Above } else { Cross::Below };
    let condition = if f.use_candle {
        Condition::CandleClose {
            symbol: symbol.clone(),
            interval: f.interval,
            cross,
            price: f.price,
        }
    } else {
        Condition::MarkCross {
            symbol: symbol.clone(),
            cross,
            price: f.price,
        }
    };

    let entry = if f.limit_entry {
        if f.limit_price <= 0.0 {
            return Err("Limit fiyatı geçersiz.".into());
        }
        Entry::Limit {
            price: f.limit_price,
        }
    } else {
        Entry::Market
    };

    let exits = if f.stop > 0.0 && f.target > 0.0 {
        let e = Exits::simple(f.stop, f.target);
        if !e.is_coherent(f.side) {
            return Err("Stop ile hedef işlemin yanlış tarafında.".into());
        }
        Some(e)
    } else {
        None
    };

    Ok(Alert {
        id: AlertId::new(gen_id()),
        owner: owner.into(),
        account: account.into(),
        condition,
        invalidate: None,
        action: AlertAction::Trade(TradeSpec {
            symbol,
            side: f.side,
            size: f.size,
            entry,
            exits,
        }),
        state: AlertState::Armed,
        armed_at_ms: now_ms(),
        entry_oid: None,
        fill_deadline_ms: None,
    })
}

/// Alarmı imzala ve yerine yönlendir.
pub async fn submit(alert: Alert, master: &str, sub: &str) -> Result<Placed, String> {
    let bundle =
        prepare_alert(&alert, BUILDER_PUBKEY, sub, master, now_ms()).map_err(|e| e.to_string())?;

    // finalize bundle'ı tüketiyor; lazım olanları önce oku.
    let routing = bundle.routing;
    let entry_oid = bundle.entry_oid();
    let entry_bytes = bundle.entry.as_ref().map(|p| p.message_bytes.clone());
    let entry_nonce = bundle.entry.as_ref().map(|p| p.nonce);
    let cancel_bytes = bundle.cancel.as_ref().map(|p| p.message_bytes.clone());
    let cancel_nonce = bundle.cancel.as_ref().map(|p| p.nonce);

    // Cüzdan imzaları (giriş, sonra varsa ön-imzalı iptal).
    let entry_sig = match &entry_bytes {
        Some(b) => Some(wallet::sign_message(b).await.map_err(|e| e.to_string())?),
        None => None,
    };
    let cancel_sig = match &cancel_bytes {
        Some(b) => Some(wallet::sign_message(b).await.map_err(|e| e.to_string())?),
        None => None,
    };

    let signed = finalize_bundle(bundle, entry_sig.as_deref(), cancel_sig.as_deref());

    match routing {
        // Borsaya hemen: kullanıcı imzaladı, biz sadece iletiyoruz.
        Routing::OnChain => {
            let body = signed.entry.ok_or("giriş blob'u yok")?;
            bulk::submit(&body).await.map_err(|e| e.to_string())?;
            Ok(Placed::OnChain)
        }
        // Watcher yürütecek: alarmı + blob'ları PUSU'ya yaz.
        Routing::WatchedMarket | Routing::WatchedLimit | Routing::Notify => {
            let mut alert = alert;
            alert.entry_oid = entry_oid; // watcher takibi buna bağlı

            let entry = signed
                .entry
                .zip(entry_nonce)
                .map(|(payload, nonce)| json!({ "nonce": nonce, "payload": payload }));
            let cancel = signed
                .cancel
                .zip(cancel_nonce)
                .map(|(payload, nonce)| json!({ "nonce": nonce, "payload": payload }));

            let body = json!({
                "alert": serde_json::to_value(&alert).map_err(|e| e.to_string())?,
                "entry": entry,
                "cancel": cancel,
            });
            api::create_alert(&body).await?;
            Ok(Placed::Watched)
        }
    }
}
