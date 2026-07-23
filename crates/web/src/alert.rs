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
    Alert, AlertAction, AlertId, AlertState, Condition, Cross, Entry, ExitLeg, Exits, Interval,
    Side, Symbol, TradeSpec,
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
    /// Kâr al kademeleri: (fiyat, yüzde). Boşsa hedef yok. Tek %100 bacak +
    /// tek %100 stop → basit OCO (`rng`); fazlası kademeli (`tp`/`st`).
    pub take_profits: Vec<(f64, f64)>,
    /// Zarar durdur kademeleri: (fiyat, yüzde). Boşsa stop yok.
    pub stops: Vec<(f64, f64)>,
    /// İptal (invalidate) koşulu açık mı — setup bozulursa alarmı düşür.
    pub inv_on: bool,
    /// true → eşiğin üstüne çıkarsa iptal; false → altına inerse.
    pub inv_above: bool,
    pub inv_price: f64,
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

/// Kademe listelerinden `Exits` kur ve doğrula.
///
/// İkisi de boşsa çıkış yok (`None`). Doğrulama `core`'un kurallarıyla aynı:
/// yüzdeler 0-100 ve toplam ≤ %100 (borsa fazlasını kırpsa da sessiz kabul
/// yanlış olur), stop'lar hedeflerin doğru tarafında (yoksa emir dolar dolmaz
/// kendini tetikler).
fn build_exits(
    tps: &[(f64, f64)],
    sls: &[(f64, f64)],
    side: Side,
) -> Result<Option<Exits>, String> {
    if tps.is_empty() && sls.is_empty() {
        return Ok(None);
    }

    let legs = |xs: &[(f64, f64)]| -> Result<Vec<ExitLeg>, String> {
        xs.iter()
            .map(|&(price, pct)| {
                if price <= 0.0 {
                    return Err("Invalid exit price.".to_string());
                }
                if pct <= 0.0 || pct > 100.0 {
                    return Err("Exit percentage must be between 0 and 100.".to_string());
                }
                Ok(ExitLeg::new(price, pct))
            })
            .collect()
    };

    let e = Exits {
        take_profits: legs(tps)?,
        stops: legs(sls)?,
    };
    if !e.pcts_ok() {
        return Err("Tier percentages can't add up past 100%.".into());
    }
    if !e.is_coherent(side) {
        return Err("Stop and target are on the wrong side of the trade.".into());
    }
    Ok(Some(e))
}

/// Formu doğrulayıp `Alert`'e çevir.
pub fn build_alert(f: &Form, owner: &str, account: &str) -> Result<Alert, String> {
    if f.symbol.trim().is_empty() {
        return Err("Symbol can't be empty.".into());
    }
    if f.size <= 0.0 {
        return Err("Size must be greater than 0.".into());
    }
    if f.price <= 0.0 {
        return Err("Invalid trigger price.".into());
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
            return Err("Invalid limit price.".into());
        }
        Entry::Limit {
            price: f.limit_price,
        }
    } else {
        Entry::Market
    };

    let exits = build_exits(&f.take_profits, &f.stops, f.side)?;

    // İptal koşulu: setup bozulursa alarmı düşür. Anlık mark kullanıyoruz —
    // "setup öldü" olayı bir sonraki mum kapanışını bekleyemez, o anda geçerli.
    let invalidate = if f.inv_on {
        if f.inv_price <= 0.0 {
            return Err("Invalid cancel price.".into());
        }
        // Karşı yönlü koruyucu iptal, tetik eşiğinin yanlış tarafındaysa alarm
        // koşul tutmadan anında iptal olur; kullanıcı sessizce kaybeder.
        let opposite = f.above != f.inv_above;
        let wrong_side = if f.above {
            f.inv_price >= f.price
        } else {
            f.inv_price <= f.price
        };
        if opposite && wrong_side {
            return Err(
                "The cancel level is on the wrong side of the trigger — the alert would cancel instantly."
                    .into(),
            );
        }
        let inv_cross = if f.inv_above {
            Cross::Above
        } else {
            Cross::Below
        };
        Some(Condition::MarkCross {
            symbol: symbol.clone(),
            cross: inv_cross,
            price: f.inv_price,
        })
    } else {
        None
    };

    Ok(Alert {
        id: AlertId::new(gen_id()),
        owner: owner.into(),
        account: account.into(),
        condition,
        invalidate,
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
        cancel_requested: false,
    })
}

/// Doğal dil taslağından tam `Alert` kur — id ve kuruluş anı burada atanır.
///
/// Taslak zaten `pusu_nl` tarafından doğrulandı (çıkış tutarlılığı, iptal
/// tarafı); form yolundaki `build_alert` ile aynı runtime alanlarını koyuyoruz,
/// yalnızca girdisi cümle.
pub fn from_draft(draft: pusu_nl::Draft, owner: &str, account: &str) -> Alert {
    draft.into_alert(pusu_nl::AlertCtx {
        id: gen_id(),
        owner: owner.to_string(),
        account: account.to_string(),
        now_ms: now_ms(),
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
            let body = signed.entry.ok_or("no entry blob")?;
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
