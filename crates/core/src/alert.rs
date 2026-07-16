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

/// Koşul tuttuğunda emir nasıl girilecek?
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    /// Piyasa fiyatından, hemen. Doldu demek girdi demek.
    Market,

    /// Belirtilen fiyattan bekle (retest).
    ///
    /// Kullanıcının kurgusu: *"15m'de 10'un üstünde kapatsın, sonra retest'te
    /// benim emrimi alsın."* İki ihtimal var ve ikincisi ürünün işi:
    ///
    /// 1. Retest gelir → emir dolar → işlem girer
    /// 2. Retest gelmez, hacimli gider → emir **dolmaz** → bir periyot sonra
    ///    ön-imzalı iptal gönderilip kullanıcıya "kaçırdın, hâlâ istiyor
    ///    musun?" diye sorulur ([`AlertState::Working`])
    ///
    /// İkincisi olmadan emir sonsuza dek defterde asılı kalır ve kullanıcı
    /// günler sonra, senaryosu çoktan bozulmuşken doldurulur.
    Limit { price: f64 },
}

impl Entry {
    pub const fn is_limit(&self) -> bool {
        matches!(self, Self::Limit { .. })
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
    pub entry: Entry,
    /// Emir dolduğunda otomatik kurulacak koruma. `of` (on-fill) ile
    /// aynı imzalı tx'e gömülür — parent dolmadan çocuklar uykuda bekler.
    pub exits: Option<Exits>,
}

/// Tek bir çıkış kademesi: hangi fiyattan, pozisyonun ne kadarı.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ExitLeg {
    pub price: f64,
    /// Pozisyonun yüzdesi (0 < pct <= 100).
    ///
    /// Yüzde, mutlak miktar değil — kullanıcı "TP1'de %30 al" diye düşünüyor,
    /// "0.0012 BTC sat" diye değil. Mutlak boyuta imza anında çevriliyor;
    /// [`TradeSpec::size`] o an sabit olduğu için sonuç da sabit.
    pub pct: f64,
}

impl ExitLeg {
    pub fn new(price: f64, pct: f64) -> Self {
        Self { price, pct }
    }

    /// Bu kademenin mutlak miktarı.
    pub fn size_of(&self, position: f64) -> f64 {
        position * self.pct / 100.0
    }
}

/// Giriş dolduğunda kurulacak kademeli çıkışlar.
///
/// Kullanıcının gerçek kurgusu tek stop + tek hedef değil: *"TP1 şu seviyede
/// %30, TP2 şu seviyede %70; SL1 şurada %50, SL2 şurada %50."* Kaç kademe
/// olacağı kullanıcıya kalmış.
///
/// # Neden yüzdeleri giriş anında sabitleyebiliyoruz
///
/// Staging'de ölçüldü (§8.10): pozisyondan **büyük** bir koruma emri
/// reddedilmiyor, **kırpılıyor** — `min(emir, pozisyon)` kadar kapatıyor,
/// ters pozisyon da açmıyor.
///
/// Bu olmasaydı ladder ön-imzalanamazdı: TP1 %30 dolunca pozisyon %70'e
/// düşer, %100 boyutlu SL reddedilir ve kullanıcı **korumasız** kalırdı.
/// Kırpma sayesinde her kademe orijinal boyutun yüzdesi olarak imzalanıp
/// unutulabiliyor; dolum sırasına göre yeniden hesap gerekmiyor. İmzalı blob
/// zaten değiştirilemez olduğu için bu şart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Exits {
    pub take_profits: Vec<ExitLeg>,
    pub stops: Vec<ExitLeg>,
}

impl Exits {
    /// Klasik tek stop + tek hedef. `rng` (OCO) olarak derleniyor.
    pub fn simple(stop: f64, take_profit: f64) -> Self {
        Self {
            take_profits: vec![ExitLeg::new(take_profit, 100.0)],
            stops: vec![ExitLeg::new(stop, 100.0)],
        }
    }

    /// Tek stop + tek hedef mi? (Derleyici bu hali `rng`'ye çeviriyor.)
    pub fn is_simple(&self) -> bool {
        self.take_profits.len() == 1
            && self.stops.len() == 1
            && self.take_profits[0].pct == 100.0
            && self.stops[0].pct == 100.0
    }

    /// Kademeler kendi içinde tutarlı mı?
    ///
    /// Girişi bilmiyoruz (market emri), o yüzden girişe göre değil kendi
    /// içinde bakıyoruz: long'da **her** stop **her** hedefin altında olmalı.
    /// Ters kurulmuş bir çıkış, emir dolar dolmaz kendini tetikler.
    pub fn is_coherent(&self, side: Side) -> bool {
        let (Some(en_yuksek_stop), Some(en_dusuk_hedef)) = (
            self.stops.iter().map(|l| l.price).reduce(f64::max),
            self.take_profits.iter().map(|l| l.price).reduce(f64::min),
        ) else {
            // Tek taraflı çıkış (sadece TP ya da sadece SL) geçerli.
            return true;
        };
        match side {
            Side::Buy => en_yuksek_stop < en_dusuk_hedef,
            Side::Sell => {
                let en_dusuk_stop = self.stops.iter().map(|l| l.price).fold(f64::MAX, f64::min);
                let en_yuksek_hedef = self
                    .take_profits
                    .iter()
                    .map(|l| l.price)
                    .fold(f64::MIN, f64::max);
                en_dusuk_stop > en_yuksek_hedef
            }
        }
    }

    /// Yüzdeler mantıklı mı?
    ///
    /// Borsa fazlasını kırpıyor ama toplamı %100'ü aşan bir ladder'ı sessizce
    /// kabul etmek yanlış olur: kullanıcı kapatamayacağı bir miktarı
    /// kapattığını sanır ve kademelerin bir kısmı hiç çalışmaz.
    pub fn pcts_ok(&self) -> bool {
        let bacak_ok = |ls: &[ExitLeg]| ls.iter().all(|l| l.pct > 0.0 && l.pct <= 100.0);
        let toplam_ok = |ls: &[ExitLeg]| ls.iter().map(|l| l.pct).sum::<f64>() <= 100.0 + 1e-9;
        bacak_ok(&self.take_profits)
            && bacak_ok(&self.stops)
            && toplam_ok(&self.take_profits)
            && toplam_ok(&self.stops)
    }

    pub fn is_empty(&self) -> bool {
        self.take_profits.is_empty() && self.stops.is_empty()
    }
}

/// Alarmın yaşam döngüsü.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertState {
    /// Koşul bekleniyor.
    Armed,

    /// Limit giriş gönderildi, **defterde dolmayı bekliyor**.
    ///
    /// Nihai değil: watcher izlemeye devam ediyor. Dolarsa [`Self::Fired`],
    /// periyot dolduğu hâlde dolmazsa ön-imzalı iptal gönderilip
    /// [`Self::Missed`] oluyor.
    ///
    /// Market girişte bu durum hiç görülmüyor — market emri ya dolar ya
    /// reddedilir, beklemez.
    Working,

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

    /// Koşul sağlandı **ama biz çok geç gördük**. Emir gönderilmedi.
    ///
    /// Watcher uzun süre düşüp geri geldiğinde, saatler önce kapanmış bir mum
    /// hâlâ kuralı sağlıyor olabilir — ama piyasa çoktan başka yerde.
    /// O fiyata market emriyle girmek, kullanıcının kurduğu alarmın değil,
    /// bizim gecikmemizin sonucu olur.
    ///
    /// Pencere periyodun kendisi: saatlik alarm 1 saat, 15 dakikalık alarm
    /// 15 dakika. "Saatlik kapanışta gir" diyen kullanıcı için o saat
    /// geçtiyse premis de geçmiştir.
    ///
    /// Sessizce düşürmüyoruz: kullanıcıya "kaçırdın, hâlâ istiyor musun?"
    /// diye soruluyor. Kararı o veriyor, biz onun adına işleme girmiyoruz.
    ///
    /// İki yoldan buraya düşülüyor:
    /// 1. Koşul çok geç görüldü (watcher düşmüştü)
    /// 2. Limit giriş bir periyot boyunca dolmadı — retest gelmedi
    Missed,
}

impl AlertState {
    /// Bu durum nihai mi? (Nihaiyse watcher bir daha dokunmaz.)
    pub const fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Fired | Self::Cancelled | Self::Rejected | Self::Uncertain | Self::Missed
        )
    }

    /// Watcher'ın hâlâ ilgilenmesi gereken bir durum mu?
    pub const fn is_live(&self) -> bool {
        matches!(self, Self::Armed | Self::Working)
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

    /// Setup'ı geçersiz kılan koşul. Sağlanırsa alarm iptal edilir, emir girmez.
    ///
    /// Kullanıcının gerçek kurgusu tek bir tetikten ibaret değil: "saatlik 10'un
    /// üstünde kapatırsa al — **ama 9'un altına düşerse bu setup ölmüştür,
    /// iptal et**". İkinci yarısı olmadan alarm, senaryosu çoktan bozulmuş bir
    /// işleme günler sonra girebilir.
    ///
    /// Zincire gömülemiyor: borsaya bırakılan `trig` kendi kendini iptal
    /// edemez. Bu yüzden iptal koşulu olan alarm daima watcher'a düşüyor
    /// ([`Alert::execution`]).
    pub invalidate: Option<Condition>,

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

    /// Limit girişin `oid`'i — **imza anında** hesaplanır.
    ///
    /// Watcher'ın dolumu takip edebilmesi ve dolmazsa iptal edebilmesi için
    /// şart. Staging'de doğrulandı (§8.9): `oid = SHA256(seqno ‖
    /// bincode(action) ‖ account ‖ nonce)`, yani gönderimden **önce**
    /// biliniyor. Bu sayede iptal de ön-imzalanabiliyor — sunucuya imza
    /// yetkisi vermeden.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_oid: Option<String>,

    /// Limit girişin dolması için son an (unix ms).
    ///
    /// Kullanıcının kuralı: *"saatlik girdiysek 1 saat, 15m'lik girdiysek 15
    /// dakika sonra sor."* Pencere koşulun periyodundan geliyor; ateşleme
    /// anında hesaplanıp buraya yazılıyor.
    ///
    /// `None` = süre sınırı yok. Mark tabanlı koşulda periyot kavramı yok,
    /// o yüzden limit normal bir GTC emri gibi bekliyor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_deadline_ms: Option<u64>,

    /// Kullanıcı **defterde bekleyen** (Working) girişin iptalini istedi mi?
    ///
    /// Working alarmın borsada canlı bir limit emri var; iptali ön-imzalı `cx`'in
    /// watcher tarafından gönderilmesini gerektiriyor (api'nin imza yetkisi yok).
    /// Bu yüzden api bayrağı kaldırıyor, watcher bir sonraki turda görüp emri
    /// geri çekiyor — tam da süre dolunca yaptığı gibi, ama sonuç `Missed` değil
    /// `Cancelled`. Emir bu arada dolduysa dolum kazanır (bkz. `Watcher::track`).
    ///
    /// Armed alarmda anlamsız (henüz borsada emir yok, iptal yerel ve anında);
    /// yalnızca Working için okunuyor.
    #[serde(default)]
    pub cancel_requested: bool,
}

impl Alert {
    /// Bu alarmı kim yürütecek? Koşul belirler.
    pub fn execution(&self) -> Execution {
        use crate::condition::WatchReason;

        match self.action {
            // Bildirim zincire gömülemez: trig basket'i emir atar, bildirim
            // göndermez. Koşul ne olursa olsun watcher'a düşer.
            AlertAction::Notify => Execution::Watched {
                reason: WatchReason::MultipleConditions,
            },
            AlertAction::Trade(_) if self.invalidate.is_some() => Execution::Watched {
                reason: WatchReason::Invalidation,
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
            entry: Entry::Market,
            exits: Some(Exits::simple(87_200.0, 94_000.0)),
        }
    }

    fn alert(condition: Condition, action: AlertAction) -> Alert {
        Alert {
            id: AlertId::new("a1"),
            owner: "master".into(),
            account: "sub".into(),
            condition,
            invalidate: None,
            action,
            state: AlertState::Armed,
            armed_at_ms: 0,
            entry_oid: None,
            fill_deadline_ms: None,
            cancel_requested: false,
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
    fn long_cikista_stop_altta_hedef_ustte_olmali() {
        let e = Exits::simple(87_200.0, 94_000.0);
        assert!(e.is_coherent(Side::Buy));
        // Aynı fiyatlar short için tutarsız.
        assert!(!e.is_coherent(Side::Sell));
    }

    #[test]
    fn short_cikista_stop_ustte_hedef_altta_olmali() {
        let e = Exits::simple(94_000.0, 87_200.0);
        assert!(e.is_coherent(Side::Sell));
        assert!(!e.is_coherent(Side::Buy));
    }

    #[test]
    fn kademeli_cikis_kurulabiliyor() {
        // Kullanıcının kurgusu: TP1 %30, TP2 %70; SL1 %50, SL2 %50.
        let e = Exits {
            take_profits: vec![ExitLeg::new(94_000.0, 30.0), ExitLeg::new(98_000.0, 70.0)],
            stops: vec![ExitLeg::new(88_000.0, 50.0), ExitLeg::new(86_000.0, 50.0)],
        };
        assert!(e.is_coherent(Side::Buy));
        assert!(e.pcts_ok());
        assert!(!e.is_simple(), "kademeli — rng'ye sığmaz");
    }

    #[test]
    fn kademe_sayisi_serbest() {
        // Kaç kademe olacağı kullanıcıya kalmış; ikiyle sınırlı değil.
        let e = Exits {
            take_profits: (1..=5)
                .map(|i| ExitLeg::new(94_000.0 + f64::from(i) * 1_000.0, 20.0))
                .collect(),
            stops: vec![ExitLeg::new(88_000.0, 100.0)],
        };
        assert!(e.is_coherent(Side::Buy));
        assert!(e.pcts_ok());
    }

    #[test]
    fn kademeli_cikista_her_stop_her_hedefin_altinda_olmali() {
        // SL2 (95k) hedeflerin arasına düşmüş: emir dolar dolmaz tetiklenir.
        let e = Exits {
            take_profits: vec![ExitLeg::new(94_000.0, 50.0), ExitLeg::new(98_000.0, 50.0)],
            stops: vec![ExitLeg::new(88_000.0, 50.0), ExitLeg::new(95_000.0, 50.0)],
        };
        assert!(!e.is_coherent(Side::Buy));
    }

    #[test]
    fn yuzde_toplami_100u_asamaz() {
        // Borsa fazlasını kırpıyor ama sessizce kabul etmek yanlış: kullanıcı
        // kapatamayacağı miktarı kapattığını sanır, son kademeler hiç çalışmaz.
        let e = Exits {
            take_profits: vec![ExitLeg::new(94_000.0, 70.0), ExitLeg::new(98_000.0, 70.0)],
            stops: vec![],
        };
        assert!(!e.pcts_ok());
    }

    #[test]
    fn sifir_ve_negatif_yuzde_reddedilir() {
        let e = Exits {
            take_profits: vec![ExitLeg::new(94_000.0, 0.0)],
            stops: vec![],
        };
        assert!(!e.pcts_ok());

        let e = Exits {
            take_profits: vec![ExitLeg::new(94_000.0, -10.0)],
            stops: vec![],
        };
        assert!(!e.pcts_ok());
    }

    #[test]
    fn tek_tarafli_cikis_gecerli() {
        // Sadece stop, hedef yok — tutarlılık kontrolü karşılaştıracak
        // bir şey bulamayınca reddetmemeli.
        let e = Exits {
            take_profits: vec![],
            stops: vec![ExitLeg::new(88_000.0, 100.0)],
        };
        assert!(e.is_coherent(Side::Buy));
        assert!(e.pcts_ok());
    }

    #[test]
    fn kademe_mutlak_miktara_cevriliyor() {
        // Kullanıcı yüzde düşünüyor, borsa miktar istiyor.
        assert!((ExitLeg::new(94_000.0, 30.0).size_of(0.004) - 0.0012).abs() < 1e-12);
        assert!((ExitLeg::new(94_000.0, 100.0).size_of(0.004) - 0.004).abs() < 1e-12);
    }

    #[test]
    fn reddedilme_ayri_bir_durum() {
        // Tetiklenip reddedilen alarm "Fired" değil — kullanıcıya söylenmeli.
        assert!(AlertState::Rejected.is_terminal());
        assert!(!AlertState::Armed.is_terminal());
    }
}
