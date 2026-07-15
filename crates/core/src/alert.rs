//! Alarmın kendisi: koşul + yapılacak iş + yaşam döngüsü.

use crate::condition::{Condition, Execution};
use crate::market::{Side, Symbol};
use serde::{Deserialize, Serialize};

/// PUSU'nun builder fee'si, baz puan cinsinden.
///
/// BULK'ın izin verdiği aralık 1–15 bps; borsanın kendi taker fee'si 3.5 bps.
/// 2 bps, kullanıcının işlem maliyetini ~%57 artırıyor — savunulabilir sınır.
/// 15 bps olsa maliyeti 5 katına çıkarırdı.
///
/// **Onay = tahsilat:** `abc`'de onaylattığımız tavan ile emre iliştirdiğimiz
/// fee aynı. Fazlasını onaylatıp azını kesmek, sonradan sessizce yükseltme
/// payı bırakırdı; ürünün tüm güven hikâyesi buna dayandığı için yapmıyoruz.
pub const BUILDER_FEE_BPS: u8 = 2;

/// Alarm tetiklendiğinde ne olacak?
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AlertAction {
    /// Sadece haber ver. Ücretsiz katman; builder fee yok, imza yok.
    ///
    /// Dikkat: bu her zaman watcher gerektirir — `trig` basket'i bildirim
    /// gönderemez, yalnızca emir atar. Yani watcher gün birden temel altyapı.
    Notify,

    /// İşleme gir.
    Trade(TradeSpec),
}

impl AlertAction {
    /// Bu aksiyon builder fee üretir mi?
    pub const fn earns_fee(&self) -> bool {
        matches!(self, Self::Trade(_))
    }
}

/// Tetiklendiğinde girilecek işlem.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TradeSpec {
    pub symbol: Symbol,
    pub side: Side,
    /// Baz varlık cinsinden miktar (örn. 0.01 BTC).
    ///
    /// Ön-imzalı tasarımda miktar **imza anında** sabitlenir; "bakiyemin %10'u"
    /// gibi dinamik bir ifade imzalanamaz, çünkü tetiklendiği andaki bakiyeyi
    /// bugünden bilemeyiz.
    pub size: f64,
    /// Emir dolduğunda otomatik kurulacak koruma. `of` (on-fill) ile
    /// aynı imzalı tx'e gömülür — parent dolmadan çocuklar uykuda bekler.
    pub bracket: Option<Bracket>,
}

/// Giriş dolduğunda kurulacak stop/hedef çifti.
///
/// BULK'ta `rng` (OCO) olarak kuruluyor: bir bacak tetiklenince diğeri
/// otomatik iptal oluyor.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Bracket {
    /// Zarar durdur.
    pub stop: f64,
    /// Kâr al.
    pub take_profit: f64,
}

impl Bracket {
    /// Stop ve hedef, girişin doğru taraflarında mı?
    ///
    /// Long'da stop girişin altında, hedef üstünde olmalı; short'ta tersi.
    /// Ters kurulmuş bir bracket, emir dolar dolmaz kendini tetikler.
    pub fn is_coherent(&self, entry: f64, side: Side) -> bool {
        match side {
            Side::Buy => self.stop < entry && self.take_profit > entry,
            Side::Sell => self.stop > entry && self.take_profit < entry,
        }
    }
}

/// Alarmın yaşam döngüsü.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertState {
    /// Koşul bekleniyor.
    Armed,
    /// Koşul sağlandı, emir gönderildi.
    Fired,
    /// Kullanıcı iptal etti.
    Cancelled,
    /// Tetiklendi ama emir borsa tarafından reddedildi (örn. yetersiz marjin).
    ///
    /// Ayrı bir durum, çünkü kullanıcıya söylemek zorundayız: alarmı çalıştı
    /// ama işlemi girmedi. Sessizce `Fired` demek yalan olur.
    Rejected,

    /// Gönderildi, **sonucu bilinmiyor**. İnsan bakmalı.
    ///
    /// Ağ yanıtı kaybolduğunda ya da borsa anlaşılmayan bir cevap döndüğünde
    /// buraya düşüyor. `Fired` demek uydurma olurdu; `Armed` bırakmak daha
    /// beter — bir sonraki tur aynı emri tekrar gönderir ve kullanıcı aynı
    /// işleme iki kez girer. Emir gerçekten geçmiş olabileceği için tekrar
    /// denemiyoruz; nihai sayıp işaretliyoruz.
    Uncertain,
}

impl AlertState {
    /// Bu durum nihai mi? (Nihaiyse watcher bir daha göndermez.)
    pub const fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Fired | Self::Cancelled | Self::Rejected | Self::Uncertain
        )
    }
}

/// Bir alarm.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Alert {
    pub id: AlertId,
    /// Kullanıcının ana hesabı — builder onayı burada duruyor.
    pub owner: String,
    /// İşlemin gireceği hesap. **Daima bir sub-account**, asla master.
    ///
    /// Gerekçe: sunucu sızarsa zarar tavanı kullanıcının o hesaba ayırdığı
    /// miktarla sınırlı kalsın. Staging'de doğrulandı: sub'a yetkili bir
    /// imza master'a dokunamıyor.
    pub account: String,
    pub condition: Condition,
    pub action: AlertAction,
    pub state: AlertState,
    /// Alarmın kurulduğu an (unix ms).
    ///
    /// Sadece kayıt değil, **doğruluk taşıyor**: "saatlik kapanış" bir olaydır,
    /// bir durum değil. Kullanıcı 14:30'da alarm kurduğunda 14:00'te kapanmış
    /// mumu kastetmiyor — bir sonrakini bekliyor. Bu alan olmadan, elinde taze
    /// kapanış bulunan bir watcher yeni alarmı kurulduğu saniye ateşler ve
    /// kullanıcıyı bir saat önceki fiyata dayanarak işleme sokar.
    pub armed_at_ms: u64,
}

impl Alert {
    /// Bu alarmı kim yürütecek? Koşul belirler.
    pub fn execution(&self) -> Execution {
        match self.action {
            // Bildirim zincire gömülemez: trig basket'i emir atar, bildirim
            // göndermez. Koşul ne olursa olsun watcher'a düşer.
            AlertAction::Notify => Execution::Watched {
                reason: crate::condition::WatchReason::MultipleConditions,
            },
            AlertAction::Trade(_) => self.condition.execution(),
        }
    }
}

/// Alarm kimliği.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AlertId(String);

impl AlertId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::condition::WatchReason;
    use crate::market::{Cross, Interval};

    fn trade_spec() -> TradeSpec {
        TradeSpec {
            symbol: "BTC-USD".into(),
            side: Side::Buy,
            size: 0.01,
            bracket: Some(Bracket {
                stop: 87_200.0,
                take_profit: 94_000.0,
            }),
        }
    }

    fn alert(condition: Condition, action: AlertAction) -> Alert {
        Alert {
            id: AlertId::new("a1"),
            owner: "master".into(),
            account: "sub".into(),
            condition,
            action,
            state: AlertState::Armed,
            armed_at_ms: 0,
        }
    }

    #[test]
    fn fee_onaylanabilir_aralikta() {
        // BULK 1..=15 bps kabul ediyor; dışına çıkarsak imzalayıcı reddeder.
        assert!((1..=15).contains(&BUILDER_FEE_BPS));
    }

    #[test]
    fn mark_cross_islemi_zincirde_yasar() {
        let a = alert(
            Condition::MarkCross {
                symbol: "BTC-USD".into(),
                cross: Cross::Below,
                price: 88_400.0,
            },
            AlertAction::Trade(trade_spec()),
        );
        assert!(a.execution().is_onchain());
    }

    #[test]
    fn bildirim_kosul_zincire_uygun_olsa_bile_watchera_duser() {
        // trig basket'i bildirim gönderemez — koşul MarkCross olsa bile.
        let a = alert(
            Condition::MarkCross {
                symbol: "BTC-USD".into(),
                cross: Cross::Below,
                price: 88_400.0,
            },
            AlertAction::Notify,
        );
        assert!(!a.execution().is_onchain());
    }

    #[test]
    fn mum_kapanisli_islem_watchera_duser() {
        // Kullanıcının kendi derdi.
        let a = alert(
            Condition::CandleClose {
                symbol: "BTC-USD".into(),
                interval: Interval::H1,
                cross: Cross::Above,
                price: 90_000.0,
            },
            AlertAction::Trade(trade_spec()),
        );
        assert_eq!(
            a.execution(),
            Execution::Watched {
                reason: WatchReason::CandleClose
            }
        );
    }

    #[test]
    fn sadece_islem_fee_uretir() {
        assert!(AlertAction::Trade(trade_spec()).earns_fee());
        assert!(!AlertAction::Notify.earns_fee());
    }

    #[test]
    fn long_brackette_stop_altta_hedef_ustte_olmali() {
        let b = Bracket {
            stop: 87_200.0,
            take_profit: 94_000.0,
        };
        assert!(b.is_coherent(90_000.0, Side::Buy));
        // Ters çevrilmişi long için tutarsız.
        assert!(!b.is_coherent(90_000.0, Side::Sell));
    }

    #[test]
    fn short_brackette_stop_ustte_hedef_altta_olmali() {
        let b = Bracket {
            stop: 94_000.0,
            take_profit: 87_200.0,
        };
        assert!(b.is_coherent(90_000.0, Side::Sell));
        assert!(!b.is_coherent(90_000.0, Side::Buy));
    }

    #[test]
    fn stop_girisin_yanlis_tarafindaysa_tutarsiz() {
        // Emir dolar dolmaz kendini tetikleyecek bir bracket.
        let b = Bracket {
            stop: 91_000.0,
            take_profit: 94_000.0,
        };
        assert!(!b.is_coherent(90_000.0, Side::Buy));
    }

    #[test]
    fn reddedilme_ayri_bir_durum() {
        // Tetiklenip reddedilen alarm "Fired" değil — kullanıcıya söylenmeli.
        assert!(AlertState::Rejected.is_terminal());
        assert!(!AlertState::Armed.is_terminal());
    }
}
