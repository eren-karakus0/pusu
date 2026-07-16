//! Açılışta borsayla mutabakat.
//!
//! # Kapatılan çatlak
//!
//! `submit` niyeti postalamadan **önce** yazıyor (bkz. [`crate::HttpDispatch`]).
//! Aradaki çökme şu izi bırakıyor: alarm hâlâ `Armed` ama giriş blob'u
//! `dispatched` işaretli. Yani emri postaladık, sonucunu kaydedemedik.
//!
//! Mutabakat olmasa watcher koşulu yeniden değerlendirir, hâlâ tutuyordur ve
//! emri **tekrar** postalar. Nonce çift girişi engelliyor (§8.11) ama tekrar
//! gönderim `504` döndürdüğü için watcher sonucu okuyamaz ve alarmı boş yere
//! `Uncertain` işaretler — aslında emir çoktan girmiştir.
//!
//! Çözüm tekrar göndermek değil, **sormak**: `entry_oid` gönderimden önce
//! biliniyor (§8.9), o yüzden `openOrders`'ı o oid için sorgulayıp gerçeği
//! öğreniyoruz.
//!
//! # Neden yalnızca giriş
//!
//! İptal blob'unu tekrar göndermek zararsız (dolmuş emri iptal etmek etkisiz,
//! bekleyeni iptal etmek zaten amaç). Tehlikeli olan **giriş**in ikinci kez
//! işlem açması; mutabakat onu hedefliyor. İptal tarafını watcher'ın `track`
//! döngüsü zaten tekrar deneyerek toparlıyor.

use pusu_core::{Alert, AlertState, Condition, Interval};
use pusu_feed::OrderSource;
use pusu_store::{BlobRole, Store, StoreError};

/// Mutabakat sonucu — hangi alarm hangi duruma taşındı.
#[derive(Debug, Default, PartialEq)]
pub struct Reconciled {
    /// Giriş postalanmış ve borsada dolmuş/gitmiş → `Fired`.
    pub fired: Vec<String>,
    /// Giriş limit olarak defterde bekliyor → `Working`.
    pub working: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ReconcileError {
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("açık emirler okunamadı ({account}): {source}")]
    Orders {
        account: String,
        source: pusu_feed::FeedError,
    },
}

/// Canlı alarmları borsayla uzlaştır. `now_ms` deadline yeniden hesabı için.
///
/// Yalnızca **çatlağa düşenlere** dokunuyor: `Armed` görünüp giriş blob'u
/// gönderilmiş olanlar. Temiz `Armed` (henüz gönderilmemiş) ve zaten `Working`
/// olanlar watcher'ın normal döngüsüne bırakılıyor.
pub async fn reconcile<O: OrderSource>(
    store: &Store,
    orders: &O,
    alerts: &mut [Alert],
    now_ms: u64,
) -> Result<Reconciled, ReconcileError> {
    let mut sonuc = Reconciled::default();

    for alert in alerts.iter_mut() {
        if alert.state != AlertState::Armed {
            continue;
        }
        if !store.was_dispatched(&alert.id, BlobRole::Entry).await? {
            continue; // temiz Armed — henüz göndermedik, dokunma
        }
        let Some(oid) = alert.entry_oid.clone() else {
            continue; // gönderildi ama oid yok: izleyemeyiz, watcher'a bırak
        };

        let acik =
            orders
                .open_orders(&alert.account)
                .await
                .map_err(|e| ReconcileError::Orders {
                    account: alert.account.clone(),
                    source: e,
                })?;

        let yeni = match acik.iter().find(|o| o.oid == oid) {
            // Defterde yok → doldu ya da gitti. İşlem girmiştir; tekrar
            // GÖNDERMEK yerine Fired yazıyoruz. Nonce zaten çift girişi
            // engelliyor ama asıl kazanç: boşuna Uncertain dememek.
            None => AlertState::Fired,
            // Kısmen dolmuş → emir alınmış, pozisyon var.
            Some(o) if !o.untouched() => AlertState::Fired,
            // Hiç dolmamış, defterde bekliyor → limit retest bekliyor.
            Some(_) => {
                alert.fill_deadline_ms = Some(now_ms + window_ms(&alert.condition));
                AlertState::Working
            }
        };

        alert.state = yeni;
        store.update_runtime(alert).await?;
        store
            .audit(
                &alert.id,
                "reconcile",
                &serde_json::json!({ "oid": oid, "to": state_label(yeni) }),
            )
            .await?;

        match yeni {
            AlertState::Working => sonuc.working.push(alert.id.as_str().to_string()),
            _ => sonuc.fired.push(alert.id.as_str().to_string()),
        }
    }

    Ok(sonuc)
}

/// Koşulun kaçırılma penceresi: mum tabanlı bacakların **en kısa** periyodu.
///
/// `evidence()` ile aynı mantık ama snapshot gerektirmiyor — mutabakat anında
/// taze veri elimizde olmayabilir, pencereyi koşulun yapısından çıkarıyoruz.
/// Mark tabanlı koşulda periyot yok; makul bir varsayılan (1 saat) veriyoruz
/// ki limit sonsuza asılı kalmasın.
fn window_ms(condition: &Condition) -> u64 {
    fn en_kisa(c: &Condition, acc: &mut Option<u64>) {
        match c {
            Condition::CandleClose { interval, .. } => {
                let d = interval.duration_ms();
                *acc = Some(acc.map_or(d, |x| x.min(d)));
            }
            Condition::MarkCross { .. } => {}
            Condition::All(inner) | Condition::Any(inner) => {
                for c in inner {
                    en_kisa(c, acc);
                }
            }
        }
    }
    let mut acc = None;
    en_kisa(condition, &mut acc);
    acc.unwrap_or_else(|| Interval::H1.duration_ms())
}

const fn state_label(s: AlertState) -> &'static str {
    match s {
        AlertState::Armed => "armed",
        AlertState::Working => "working",
        AlertState::Fired => "fired",
        AlertState::Cancelled => "cancelled",
        AlertState::Rejected => "rejected",
        AlertState::Uncertain => "uncertain",
        AlertState::Missed => "missed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pusu_core::{Cross, Symbol};

    fn candle(interval: Interval) -> Condition {
        Condition::CandleClose {
            symbol: Symbol::new("BTC-USD"),
            interval,
            cross: Cross::Above,
            price: 90_000.0,
        }
    }

    fn mark() -> Condition {
        Condition::MarkCross {
            symbol: Symbol::new("BTC-USD"),
            cross: Cross::Below,
            price: 88_000.0,
        }
    }

    #[test]
    fn tek_mum_penceresi_kendi_periyodu() {
        assert_eq!(
            window_ms(&candle(Interval::M15)),
            Interval::M15.duration_ms()
        );
        assert_eq!(window_ms(&candle(Interval::H1)), Interval::H1.duration_ms());
    }

    #[test]
    fn kompozit_en_kisa_periyodu_alir() {
        // "15m VE 1h kapanış" — retest deadline'ı en kısa periyoda göre;
        // 15m'lik ayak dolmadıysa erken sormak, geç sormaktan iyi.
        let c = Condition::All(vec![candle(Interval::H1), candle(Interval::M15)]);
        assert_eq!(window_ms(&c), Interval::M15.duration_ms());
    }

    #[test]
    fn ic_ice_kompozitte_de_en_kisa() {
        let c = Condition::Any(vec![
            candle(Interval::D1),
            Condition::All(vec![mark(), candle(Interval::M5)]),
        ]);
        assert_eq!(window_ms(&c), Interval::M5.duration_ms());
    }

    #[test]
    fn periyotsuz_kosul_makul_varsayilana_duser() {
        // Saf mark cross'un periyodu yok; limit sonsuza asılı kalmasın diye
        // 1 saatlik güvenli varsayılan.
        assert_eq!(window_ms(&mark()), Interval::H1.duration_ms());
        // Hepsi periyotsuzsa da aynı.
        let c = Condition::All(vec![mark(), mark()]);
        assert_eq!(window_ms(&c), Interval::H1.duration_ms());
    }
}
