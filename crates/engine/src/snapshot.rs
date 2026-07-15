//! Piyasanın bilinen durumu ve koşul değerlendirme.
//!
//! # Tek kural: bilmiyorsak ateşlemeyiz
//!
//! Değerlendirme `Option<bool>` döndürüyor, `bool` değil. `None` = "henüz
//! bilmiyorum". Bu ayrım ürünün doğruluğunu taşıyor:
//!
//! Kullanıcı "BTC saatlik 90 binin üstünde **ve** ETH saatlik 3 binin üstünde
//! kapatırsa al" dediğinde, elimizde yalnızca BTC'nin kapanışı varsa cevap
//! "hayır" değil, **"bilmiyorum"**. `false` deyip geçmek zararsız görünür ama
//! `Any` (veya) koşulunda tam tersine döner: bilinmeyen bacağı `false` saymak,
//! alarmı yanlış bilgiyle ateşletir.
//!
//! Bu yüzden eksik veri her yerde `None` olarak yayılıyor ve motor yalnızca
//! `Some(true)` gördüğünde ateşliyor.
//!
//! # Bayat kapanışla ateşlememek
//!
//! Snapshot bir kapanışı gördükten sonra onu kalıcı olarak tutuyor — bileşik
//! koşullar için şart: "BTC 1h > 90k **ve** ETH 1h > 3k" derken iki mum aynı
//! tick'te gelmeyebilir, ilkini hatırlamamız gerekir.
//!
//! Ama bu, yeni kurulan alarm için tuzak: saatlerdir çalışan bir watcher'ın
//! elinde 14:00 kapanışı varken 14:30'da alarm kurulursa, koşul o saniye
//! sağlanmış görünür ve alarm anında ateşler. Kullanıcı bir sonraki saatlik
//! kapanışı bekliyordu; bir saat önceki fiyata dayanarak işleme sokulur.
//!
//! Bu yüzden [`evaluate`] alarmın kurulma anını alıyor ve yalnızca **ondan
//! sonra kapanmış** mumları sayıyor. Daha eskisi `None` — "henüz uygun bir
//! kapanış görmedim".
//!
//! Mark price'a bu kapı uygulanmıyor: mark bir olay değil, anlık durum.
//! Zincirdeki `trig` de koşul zaten sağlanmışsa hemen tetikliyor; aynı
//! davranışı koruyoruz.

use pusu_core::{Condition, Interval, Symbol};
use std::collections::HashMap;

/// Kapanmış bir mumun bıraktığı iz.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Close {
    price: f64,
    /// Mumun kapandığı an (unix ms). Tazelik kapısı bunun üzerinden işliyor.
    at_ms: u64,
}

/// Motorun piyasa hakkında bildikleri.
///
/// Yalnızca **kapanmış** mumların kapanışları ve son mark price'lar.
/// Devam eden mum buraya asla girmez — [`pusu_feed`] onu zaten eliyor.
#[derive(Debug, Default, Clone)]
pub struct Snapshot {
    closes: HashMap<(Symbol, Interval), Close>,
    marks: HashMap<Symbol, f64>,
}

impl Snapshot {
    pub fn new() -> Self {
        Self::default()
    }

    /// Kapanmış mumu kaydet. `at_ms` mumun kapanış zamanı.
    ///
    /// **Geriye gitmez.** Daha eski (ya da aynı) bir kapanış gelirse yok
    /// sayılır. Sunucu ara sıra gecikmiş yanıt döndürüyor; onu kaydetseydik
    /// snapshot güncel kapanıştan eskisine düşer ve alarm, artık geçerli
    /// olmayan bir fiyata dayanarak ateşleyebilirdi.
    pub fn set_close(&mut self, symbol: &Symbol, interval: Interval, price: f64, at_ms: u64) {
        let key = (symbol.clone(), interval);
        if self.closes.get(&key).is_some_and(|c| c.at_ms >= at_ms) {
            return;
        }
        self.closes.insert(key, Close { price, at_ms });
    }

    /// Son mark price'ı kaydet.
    pub fn set_mark(&mut self, symbol: &Symbol, mark: f64) {
        self.marks.insert(symbol.clone(), mark);
    }

    /// `since_ms`'ten **sonra** kapanmış mumun kapanış fiyatı.
    ///
    /// Daha eski bir kapanış varsa `None` — bilgi var ama bu alarm için
    /// geçerli değil.
    pub fn close_after(&self, symbol: &Symbol, interval: Interval, since_ms: u64) -> Option<f64> {
        self.closes
            .get(&(symbol.clone(), interval))
            .filter(|c| c.at_ms > since_ms)
            .map(|c| c.price)
    }

    pub fn mark(&self, symbol: &Symbol) -> Option<f64> {
        self.marks.get(symbol).copied()
    }
}

/// Koşulu bilinen duruma göre değerlendir.
///
/// `since_ms`: alarmın kurulma anı. Bundan önce kapanmış mumlar sayılmaz
/// (modül dokümanına bak).
///
/// - `Some(true)` — koşul sağlandı, ateşle
/// - `Some(false)` — sağlanmadı
/// - `None` — **yeterli veri yok**, karar verme
pub fn evaluate(condition: &Condition, snap: &Snapshot, since_ms: u64) -> Option<bool> {
    match condition {
        // Mark anlık durum, olay değil — tazelik kapısı yok.
        Condition::MarkCross {
            symbol,
            cross,
            price,
        } => snap.mark(symbol).map(|m| cross.is_met(m, *price)),

        Condition::CandleClose {
            symbol,
            interval,
            cross,
            price,
        } => snap
            .close_after(symbol, *interval, since_ms)
            .map(|c| cross.is_met(c, *price)),

        // Bir bacak bile false ise sonuç false — eksik bacaklara bakmaya gerek yok.
        // Ama hiçbiri false değilken bir bacak bilinmiyorsa cevap None:
        // "hepsi sağlandı" diyemeyiz.
        Condition::All(inner) => {
            let mut eksik = false;
            for c in inner {
                match evaluate(c, snap, since_ms) {
                    Some(false) => return Some(false),
                    None => eksik = true,
                    Some(true) => {}
                }
            }
            if eksik {
                None
            } else {
                Some(true)
            }
        }

        // Bir bacak bile true ise sonuç true. Hiçbiri true değilken bir bacak
        // bilinmiyorsa None: o bacak true olabilir.
        Condition::Any(inner) => {
            let mut eksik = false;
            for c in inner {
                match evaluate(c, snap, since_ms) {
                    Some(true) => return Some(true),
                    None => eksik = true,
                    Some(false) => {}
                }
            }
            if eksik {
                None
            } else {
                Some(false)
            }
        }
    }
}

/// Sağlanmış bir koşulun kanıtı — "neye dayanarak ateşliyoruz?"
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Evidence {
    /// Zamana bağlı olmayan kanıt (mark price). Hep taze.
    Fresh,
    /// Belirli bir anda oluşmuş kanıt (mum kapanışı) ve geçerlilik penceresi.
    At { at_ms: u64, window_ms: u64 },
}

impl Evidence {
    /// Kanıt penceresi dolmuş mu? Dolduysa alarm "kaçırıldı" sayılır.
    pub const fn is_stale(&self, now_ms: u64) -> bool {
        match self {
            Self::Fresh => false,
            Self::At { at_ms, window_ms } => now_ms > *at_ms + *window_ms,
        }
    }
}

/// Sağlanmış koşulun kanıtını çıkar. `None` = koşul sağlanmıyor.
///
/// # `All` ile `Any` neden ters çalışıyor
///
/// **`All`**: her bacağın kanıtı hâlâ geçerli olmalı → **en eskisi bağlar**.
/// "BTC 1h > 90k ve ETH 1h > 3k" koşulunda BTC'nin kanıtı 06:00'dan, ETH'ninki
/// 11:00'den geliyorsa, koşul 11:00'de sağlanmış görünür ama BTC beş saatlik
/// bayat veriye dayanıyor — bu arada 70k'ya inmiş olabilir. En eski bacağı
/// ölçmek aynı zamanda "bir feed'im donmuş" halini de yakalıyor: sağlıklı bir
/// watcher'da aynı periyottaki tüm bacakların kanıtı aynı andan gelir.
///
/// **`Any`**: tek bir bacağın sağlanması yetiyor → **en yenisi bağlar**. Biri
/// az önce sağlandıysa ateşleme haklı; diğer bacağın eski olması önemsiz.
///
/// Pencere her iki halde de bacakların **en kısa periyodu**: en sıkı zaman
/// dilimi aciliyeti belirler.
pub fn evidence(condition: &Condition, snap: &Snapshot, since_ms: u64) -> Option<Evidence> {
    match condition {
        Condition::MarkCross {
            symbol,
            cross,
            price,
        } => snap
            .mark(symbol)
            .filter(|m| cross.is_met(*m, *price))
            .map(|_| Evidence::Fresh),

        Condition::CandleClose {
            symbol,
            interval,
            cross,
            price,
        } => snap
            .closes
            .get(&(symbol.clone(), *interval))
            .filter(|c| c.at_ms > since_ms && cross.is_met(c.price, *price))
            .map(|c| Evidence::At {
                at_ms: c.at_ms,
                window_ms: interval.duration_ms(),
            }),

        // Her bacak sağlanmalı; en eski kanıt bağlar.
        Condition::All(inner) => {
            let mut birlesik: Option<Evidence> = None;
            for c in inner {
                let ev = evidence(c, snap, since_ms)?;
                birlesik = Some(match (birlesik, ev) {
                    (None, e) => e,
                    (Some(Evidence::Fresh), e) => e,
                    (Some(e), Evidence::Fresh) => e,
                    (Some(a), b) => en_eski(a, b),
                });
            }
            birlesik
        }

        // Sağlanan bacaklar yeter; en yeni kanıt bağlar.
        Condition::Any(inner) => {
            let mut birlesik: Option<Evidence> = None;
            for c in inner {
                let Some(ev) = evidence(c, snap, since_ms) else {
                    continue;
                };
                // Taze bir mark bacağı tek başına ateşlemeyi haklı kılar.
                if matches!(ev, Evidence::Fresh) {
                    return Some(Evidence::Fresh);
                }
                birlesik = Some(match birlesik {
                    None => ev,
                    Some(a) => en_yeni(a, ev),
                });
            }
            birlesik
        }
    }
}

fn en_eski(a: Evidence, b: Evidence) -> Evidence {
    let (
        Evidence::At {
            at_ms: aa,
            window_ms: aw,
        },
        Evidence::At {
            at_ms: ba,
            window_ms: bw,
        },
    ) = (a, b)
    else {
        return a;
    };
    Evidence::At {
        at_ms: aa.min(ba),
        window_ms: aw.min(bw),
    }
}

fn en_yeni(a: Evidence, b: Evidence) -> Evidence {
    let (
        Evidence::At {
            at_ms: aa,
            window_ms: aw,
        },
        Evidence::At {
            at_ms: ba,
            window_ms: bw,
        },
    ) = (a, b)
    else {
        return a;
    };
    Evidence::At {
        at_ms: aa.max(ba),
        window_ms: aw.min(bw),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pusu_core::Cross;

    /// Alarmın kurulduğu an. Testlerdeki tüm kapanışlar buna göre konumlanıyor.
    const KURULDU: u64 = 1_000;
    /// Alarm kurulduktan sonra kapanan mum.
    const SONRA: u64 = 2_000;
    /// Alarm kurulmadan önce kapanmış mum.
    const ONCE: u64 = 500;

    fn btc() -> Symbol {
        "BTC-USD".into()
    }
    fn eth() -> Symbol {
        "ETH-USD".into()
    }

    fn btc_saatlik_ustunde(price: f64) -> Condition {
        Condition::CandleClose {
            symbol: btc(),
            interval: Interval::H1,
            cross: Cross::Above,
            price,
        }
    }
    fn eth_saatlik_ustunde(price: f64) -> Condition {
        Condition::CandleClose {
            symbol: eth(),
            interval: Interval::H1,
            cross: Cross::Above,
            price,
        }
    }

    /// Alarm kurulduktan sonra kapanmış mumlarla dolu snapshot.
    fn snap(kapanislar: &[(Symbol, f64)]) -> Snapshot {
        let mut s = Snapshot::new();
        for (sym, px) in kapanislar {
            s.set_close(sym, Interval::H1, *px, SONRA);
        }
        s
    }

    fn degerlendir(c: &Condition, s: &Snapshot) -> Option<bool> {
        evaluate(c, s, KURULDU)
    }

    #[test]
    fn veri_yoksa_karar_yok() {
        // Kullanıcının derdi: "saatlik kapanış 90 binin üstünde olmalı".
        // Henüz hiç kapanış görmediysek "hayır" demek yanlış — bilmiyoruz.
        assert_eq!(
            degerlendir(&btc_saatlik_ustunde(90_000.0), &Snapshot::new()),
            None
        );
    }

    #[test]
    fn kapanis_esigin_ustundeyse_atesler() {
        let s = snap(&[(btc(), 90_500.0)]);
        assert_eq!(degerlendir(&btc_saatlik_ustunde(90_000.0), &s), Some(true));
    }

    #[test]
    fn kapanis_esigin_altindaysa_atesetmez() {
        let s = snap(&[(btc(), 89_900.0)]);
        assert_eq!(degerlendir(&btc_saatlik_ustunde(90_000.0), &s), Some(false));
    }

    #[test]
    fn alarm_kurulmadan_once_kapanan_mum_atesletmez() {
        // Bu modülün ikinci var oluş sebebi. Watcher saatlerdir çalışıyor,
        // elinde 14:00 kapanışı var. Kullanıcı 14:30'da alarm kuruyor.
        // Kapanış eşiği geçmiş olsa bile ateşlememeliyiz — kullanıcı
        // 15:00 kapanışını bekliyor.
        let mut s = Snapshot::new();
        s.set_close(&btc(), Interval::H1, 90_500.0, ONCE);
        assert_eq!(degerlendir(&btc_saatlik_ustunde(90_000.0), &s), None);
    }

    #[test]
    fn kurulmadan_once_kapanan_mum_yerini_yenisine_birakinca_atesler() {
        // Bayat mum bizi sonsuza dek kilitlememeli: bir sonraki kapanış gelince
        // alarm normal çalışmalı.
        let mut s = Snapshot::new();
        s.set_close(&btc(), Interval::H1, 90_500.0, ONCE);
        assert_eq!(degerlendir(&btc_saatlik_ustunde(90_000.0), &s), None);

        s.set_close(&btc(), Interval::H1, 90_600.0, SONRA);
        assert_eq!(degerlendir(&btc_saatlik_ustunde(90_000.0), &s), Some(true));
    }

    #[test]
    fn tam_kurulma_aninda_kapanan_mum_sayilmaz() {
        // Sınır: mum, alarm kurulmasıyla aynı milisaniyede kapandıysa
        // kullanıcı onu görerek alarmı kurmuş olabilir. Sıkı büyüklük
        // kullanıyoruz — şüphede ateşleme.
        let mut s = Snapshot::new();
        s.set_close(&btc(), Interval::H1, 90_500.0, KURULDU);
        assert_eq!(degerlendir(&btc_saatlik_ustunde(90_000.0), &s), None);
    }

    #[test]
    fn mark_tazelik_kapisina_takilmaz() {
        // Mark anlık durum: "şu an 88.4k'nın altında mı?" Sorunun geçmişi yok.
        // Zincirdeki trig de böyle davranıyor.
        let mut s = Snapshot::new();
        s.set_mark(&btc(), 88_000.0);
        let c = Condition::MarkCross {
            symbol: btc(),
            cross: Cross::Below,
            price: 88_400.0,
        };
        assert_eq!(evaluate(&c, &s, u64::MAX), Some(true));
    }

    #[test]
    fn timeframe_karismaz() {
        // 4h kapanışı biliniyor ama alarm 1h istiyor → hâlâ bilmiyoruz.
        let mut s = Snapshot::new();
        s.set_close(&btc(), Interval::H4, 95_000.0, SONRA);
        assert_eq!(degerlendir(&btc_saatlik_ustunde(90_000.0), &s), None);
    }

    #[test]
    fn sembol_karismaz() {
        let s = snap(&[(eth(), 95_000.0)]);
        assert_eq!(degerlendir(&btc_saatlik_ustunde(90_000.0), &s), None);
    }

    #[test]
    fn all_eksik_bacakla_karar_vermez() {
        // BTC sağlandı ama ETH bilinmiyor → "ikisi de sağlandı" DİYEMEYİZ.
        // Some(true) dönseydi kullanıcı istemediği işleme girerdi.
        let s = snap(&[(btc(), 90_500.0)]);
        let c = Condition::All(vec![
            btc_saatlik_ustunde(90_000.0),
            eth_saatlik_ustunde(3_000.0),
        ]);
        assert_eq!(degerlendir(&c, &s), None);
    }

    #[test]
    fn all_bir_bacak_false_ise_eksik_olsa_bile_false() {
        // ETH bilinmiyor ama BTC zaten sağlanmadı → sonuç kesin: hayır.
        // Burada None dönmek gereksiz bekleme olurdu.
        let s = snap(&[(btc(), 80_000.0)]);
        let c = Condition::All(vec![
            btc_saatlik_ustunde(90_000.0),
            eth_saatlik_ustunde(3_000.0),
        ]);
        assert_eq!(degerlendir(&c, &s), Some(false));
    }

    #[test]
    fn all_hepsi_saglandiysa_atesler() {
        let s = snap(&[(btc(), 90_500.0), (eth(), 3_100.0)]);
        let c = Condition::All(vec![
            btc_saatlik_ustunde(90_000.0),
            eth_saatlik_ustunde(3_000.0),
        ]);
        assert_eq!(degerlendir(&c, &s), Some(true));
    }

    #[test]
    fn all_bacaklar_ayri_tiklerde_gelse_de_birikir() {
        // Bileşik koşulun bütün mesele bu: BTC 1h ile ETH 1h aynı anda
        // gelmeyebilir. İlkini hatırlamazsak koşul asla sağlanmaz.
        let mut s = Snapshot::new();
        let c = Condition::All(vec![
            btc_saatlik_ustunde(90_000.0),
            eth_saatlik_ustunde(3_000.0),
        ]);

        s.set_close(&btc(), Interval::H1, 90_500.0, SONRA);
        assert_eq!(degerlendir(&c, &s), None, "ETH henüz gelmedi");

        s.set_close(&eth(), Interval::H1, 3_100.0, SONRA + 1);
        assert_eq!(degerlendir(&c, &s), Some(true), "BTC hatırlandı");
    }

    #[test]
    fn any_bir_bacak_true_ise_eksik_olsa_bile_true() {
        // BTC sağlandı; ETH'yi beklemeye gerek yok.
        let s = snap(&[(btc(), 90_500.0)]);
        let c = Condition::Any(vec![
            btc_saatlik_ustunde(90_000.0),
            eth_saatlik_ustunde(3_000.0),
        ]);
        assert_eq!(degerlendir(&c, &s), Some(true));
    }

    #[test]
    fn any_eksik_bacakla_hayir_demez() {
        // BTC sağlanmadı, ETH bilinmiyor. `false` desek alarmı yanlış bilgiyle
        // kapatırdık — ETH sağlanmış olabilir. Bu, `All`'ın tam tersi davranış.
        let s = snap(&[(btc(), 80_000.0)]);
        let c = Condition::Any(vec![
            btc_saatlik_ustunde(90_000.0),
            eth_saatlik_ustunde(3_000.0),
        ]);
        assert_eq!(degerlendir(&c, &s), None);
    }

    #[test]
    fn any_hepsi_biliniyor_ve_hicbiri_saglanmadiysa_false() {
        let s = snap(&[(btc(), 80_000.0), (eth(), 2_000.0)]);
        let c = Condition::Any(vec![
            btc_saatlik_ustunde(90_000.0),
            eth_saatlik_ustunde(3_000.0),
        ]);
        assert_eq!(degerlendir(&c, &s), Some(false));
    }

    #[test]
    fn ic_ice_kosullar_degerlendiriliyor() {
        // (BTC 1h > 90k) VE ((ETH 1h > 3k) VEYA (BTC mark < 88.4k))
        let mut s = snap(&[(btc(), 90_500.0)]);
        s.set_mark(&btc(), 88_000.0);
        let c = Condition::All(vec![
            btc_saatlik_ustunde(90_000.0),
            Condition::Any(vec![
                eth_saatlik_ustunde(3_000.0),
                Condition::MarkCross {
                    symbol: btc(),
                    cross: Cross::Below,
                    price: 88_400.0,
                },
            ]),
        ]);
        // ETH bilinmiyor ama iç Any zaten mark ile sağlandı → dış All da sağlandı.
        assert_eq!(degerlendir(&c, &s), Some(true));
    }

    #[test]
    fn snapshot_son_degeri_tutar() {
        let mut s = Snapshot::new();
        s.set_close(&btc(), Interval::H1, 80_000.0, SONRA);
        s.set_close(&btc(), Interval::H1, 91_000.0, SONRA + 1);
        assert_eq!(s.close_after(&btc(), Interval::H1, KURULDU), Some(91_000.0));
    }

    #[test]
    fn gecikmis_yanit_snapshotu_geriye_dondurmez() {
        // 11:00 kapanışı 89k geldi (ateşleme yok). Ardından gecikmiş bir yanıt
        // 10:00 kapanışını 95k olarak getiriyor. Kaydetseydik alarm, bir saat
        // önceki fiyata dayanarak ateşlerdi — üstelik tazelik kapısı buna engel
        // olmaz, çünkü 10:00 da alarmın kurulmasından sonra.
        let mut s = Snapshot::new();
        s.set_close(&btc(), Interval::H1, 89_000.0, 11_000);
        s.set_close(&btc(), Interval::H1, 95_000.0, 10_000);

        assert_eq!(s.close_after(&btc(), Interval::H1, 9_000), Some(89_000.0));
        assert_eq!(
            evaluate(&btc_saatlik_ustunde(90_000.0), &s, 9_000),
            Some(false),
            "gecikmiş mumla ateşlendi"
        );
    }

    #[test]
    fn ayni_mum_tekrar_gelirse_degismez() {
        let mut s = Snapshot::new();
        s.set_close(&btc(), Interval::H1, 89_000.0, 11_000);
        s.set_close(&btc(), Interval::H1, 95_000.0, 11_000);
        assert_eq!(s.close_after(&btc(), Interval::H1, 9_000), Some(89_000.0));
    }

    // -- kanıt / bayatlık ---------------------------------------------------

    const SAAT: u64 = 3_600_000;

    #[test]
    fn saglanmayan_kosulun_kaniti_yok() {
        let s = snap(&[(btc(), 80_000.0)]);
        assert_eq!(evidence(&btc_saatlik_ustunde(90_000.0), &s, KURULDU), None);
    }

    #[test]
    fn taze_kapanis_bayat_degil() {
        let mut s = Snapshot::new();
        s.set_close(&btc(), Interval::H1, 90_500.0, 10 * SAAT);
        let ev = evidence(&btc_saatlik_ustunde(90_000.0), &s, KURULDU).unwrap();
        // Kapanıştan 5 dakika sonra: pencere içinde.
        assert!(!ev.is_stale(10 * SAAT + 300_000));
    }

    #[test]
    fn periyot_dolunca_bayat_olur() {
        // Saatlik alarmın penceresi 1 saat: watcher 6 saat düşüp geri gelirse
        // o kapanışa dayanarak market emri göndermek, kullanıcının alarmının
        // değil bizim gecikmemizin sonucu olur.
        let mut s = Snapshot::new();
        s.set_close(&btc(), Interval::H1, 90_500.0, 10 * SAAT);
        let ev = evidence(&btc_saatlik_ustunde(90_000.0), &s, KURULDU).unwrap();

        assert!(!ev.is_stale(11 * SAAT), "tam sınırda henüz bayat değil");
        assert!(ev.is_stale(11 * SAAT + 1), "pencere doldu");
        assert!(ev.is_stale(16 * SAAT), "6 saat sonra kesinlikle bayat");
    }

    #[test]
    fn pencere_periyoda_gore_degisiyor() {
        // Kullanıcının kuralı: saatlik girdiysek 1 saat, 15m girdiysek 15 dk.
        let mut s = Snapshot::new();
        s.set_close(&btc(), Interval::M15, 90_500.0, 10 * SAAT);
        let c = Condition::CandleClose {
            symbol: btc(),
            interval: Interval::M15,
            cross: Cross::Above,
            price: 90_000.0,
        };
        let ev = evidence(&c, &s, KURULDU).unwrap();
        assert!(!ev.is_stale(10 * SAAT + 900_000), "15 dk sınırında");
        assert!(ev.is_stale(10 * SAAT + 900_001), "15 dk doldu");
    }

    #[test]
    fn mark_kosulu_hic_bayatlamaz() {
        // Mark anlık durum: "şu an altında mı?" sorusunun bayatlığı olmaz.
        let mut s = Snapshot::new();
        s.set_mark(&btc(), 88_000.0);
        let c = Condition::MarkCross {
            symbol: btc(),
            cross: Cross::Below,
            price: 88_400.0,
        };
        let ev = evidence(&c, &s, KURULDU).unwrap();
        assert_eq!(ev, Evidence::Fresh);
        assert!(!ev.is_stale(u64::MAX));
    }

    #[test]
    fn all_en_eski_bacaga_gore_bayatlar() {
        // BTC'nin kanıtı 06:00'dan, ETH'ninki 11:00'den. Koşul 11:00'de
        // sağlanmış görünüyor ama BTC beş saatlik bayat veri — bu arada
        // 70k'ya inmiş olabilir. En eski bacak bağlamalı.
        let mut s = Snapshot::new();
        s.set_close(&btc(), Interval::H1, 90_500.0, 6 * SAAT);
        s.set_close(&eth(), Interval::H1, 3_100.0, 11 * SAAT);
        let c = Condition::All(vec![
            btc_saatlik_ustunde(90_000.0),
            eth_saatlik_ustunde(3_000.0),
        ]);

        assert_eq!(degerlendir(&c, &s), Some(true), "koşul sağlanıyor");
        let ev = evidence(&c, &s, KURULDU).unwrap();
        assert!(
            ev.is_stale(11 * SAAT),
            "BTC'nin 5 saatlik kanıtına dayanarak ateşlendi"
        );
    }

    #[test]
    fn all_bacaklarin_hepsi_tazeyse_bayat_degil() {
        let mut s = Snapshot::new();
        s.set_close(&btc(), Interval::H1, 90_500.0, 11 * SAAT);
        s.set_close(&eth(), Interval::H1, 3_100.0, 11 * SAAT);
        let c = Condition::All(vec![
            btc_saatlik_ustunde(90_000.0),
            eth_saatlik_ustunde(3_000.0),
        ]);
        assert!(!evidence(&c, &s, KURULDU)
            .unwrap()
            .is_stale(11 * SAAT + 60_000));
    }

    #[test]
    fn any_en_yeni_bacaga_gore_bayatlar() {
        // All'ın tam tersi: BTC'nin kanıtı eski ama ETH az önce sağlandı.
        // Tek bacağın sağlanması yettiği için ateşleme haklı.
        let mut s = Snapshot::new();
        s.set_close(&btc(), Interval::H1, 90_500.0, 6 * SAAT);
        s.set_close(&eth(), Interval::H1, 3_100.0, 11 * SAAT);
        let c = Condition::Any(vec![
            btc_saatlik_ustunde(90_000.0),
            eth_saatlik_ustunde(3_000.0),
        ]);
        assert!(
            !evidence(&c, &s, KURULDU)
                .unwrap()
                .is_stale(11 * SAAT + 60_000),
            "taze ETH bacağı varken bayat sayıldı"
        );
    }

    #[test]
    fn any_hepsi_eskiyse_bayat() {
        let mut s = Snapshot::new();
        s.set_close(&btc(), Interval::H1, 90_500.0, 6 * SAAT);
        s.set_close(&eth(), Interval::H1, 3_100.0, 6 * SAAT);
        let c = Condition::Any(vec![
            btc_saatlik_ustunde(90_000.0),
            eth_saatlik_ustunde(3_000.0),
        ]);
        assert!(evidence(&c, &s, KURULDU).unwrap().is_stale(11 * SAAT));
    }

    #[test]
    fn any_icindeki_taze_mark_bayatligi_kaldirir() {
        let mut s = Snapshot::new();
        s.set_close(&btc(), Interval::H1, 90_500.0, 6 * SAAT);
        s.set_mark(&eth(), 2_900.0);
        let c = Condition::Any(vec![
            btc_saatlik_ustunde(90_000.0),
            Condition::MarkCross {
                symbol: eth(),
                cross: Cross::Below,
                price: 3_000.0,
            },
        ]);
        assert_eq!(evidence(&c, &s, KURULDU), Some(Evidence::Fresh));
    }

    #[test]
    fn bilesik_kosulda_en_kisa_periyot_pencereyi_belirler() {
        // 1h ve 15m bacakları var: kullanıcı 15m hassasiyeti istiyorsa
        // aciliyet ona göre.
        let mut s = Snapshot::new();
        s.set_close(&btc(), Interval::H1, 90_500.0, 10 * SAAT);
        s.set_close(&btc(), Interval::M15, 90_500.0, 10 * SAAT);
        let c = Condition::All(vec![
            btc_saatlik_ustunde(90_000.0),
            Condition::CandleClose {
                symbol: btc(),
                interval: Interval::M15,
                cross: Cross::Above,
                price: 90_000.0,
            },
        ]);
        let ev = evidence(&c, &s, KURULDU).unwrap();
        assert!(ev.is_stale(10 * SAAT + 900_001), "15 dk penceresi geçerli");
    }
}
