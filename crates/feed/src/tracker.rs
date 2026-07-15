//! Yeni kapanan mumu tespit eden durum takipçisi.
//!
//! İki işi var ve ikisi de sessiz hata kaynağı:
//!
//! 1. **Tekrarı engellemek.** Aynı mum birden çok polling'de görülür; alarmı
//!    bir kez ateşlemeliyiz.
//! 2. **Geçmişe ateşlememek.** İlk gözlemde elimize gelen "son kapanmış mum",
//!    alarm kurulmadan *önce* kapanmış olabilir. Onu işlersek kullanıcı alarmı
//!    kurar kurmaz işleme girer — istediği şey bu değil.

use crate::kline::{last_closed, Kline};
use pusu_core::{Interval, Symbol};
use std::collections::HashMap;

/// (sembol, timeframe) başına en son işlenmiş mum kapanışını tutar.
#[derive(Debug, Default)]
pub struct CandleTracker {
    seen: HashMap<(Symbol, Interval), u64>,
}

impl CandleTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Yeni bir `/klines` yanıtını işle.
    ///
    /// - İlk gözlemse: durumu kaydeder, **`None` döner** (geçmişe ateşlemeyiz).
    /// - Daha önce görülmüş mumsa: `None`.
    /// - Yeni kapanmış mum varsa: onu döner.
    pub fn observe(
        &mut self,
        symbol: &Symbol,
        interval: Interval,
        klines: &[Kline],
        now_ms: u64,
    ) -> Option<Kline> {
        let closed = *last_closed(klines, now_ms)?;
        let key = (symbol.clone(), interval);

        match self.seen.insert(key, closed.close_time) {
            // İlk gözlem: sadece hizala. Elimizdeki mum alarmdan önce
            // kapanmış olabilir; işlersek kullanıcıyı istemediği işleme sokarız.
            None => None,
            // Aynı mum ya da geriye gitmiş bir yanıt (yeniden sıralama/lag).
            Some(prev) if closed.close_time <= prev => {
                // Geriye gitmeyi kaydetmeyelim: eski değeri geri koy.
                self.seen.insert((symbol.clone(), interval), prev);
                None
            }
            Some(_) => Some(closed),
        }
    }

    /// Bu (sembol, timeframe) hizalanmış mı?
    pub fn is_primed(&self, symbol: &Symbol, interval: Interval) -> bool {
        self.seen.contains_key(&(symbol.clone(), interval))
    }

    /// Alarm silindiğinde durumu bırak — takip edilmeyen sembol için bellek tutmayalım.
    pub fn forget(&mut self, symbol: &Symbol, interval: Interval) {
        self.seen.remove(&(symbol.clone(), interval));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(close_time: u64, close: f64) -> Kline {
        Kline {
            open_time: close_time - 100,
            close_time,
            open: 0.0,
            high: 0.0,
            low: 0.0,
            close,
            volume: 0.0,
            num_trades: 0,
        }
    }

    fn btc() -> Symbol {
        "BTC-USD".into()
    }

    #[test]
    fn ilk_gozlem_atesetmez_sadece_hizalar() {
        // Alarm 14:30'da kuruldu; elimizdeki son kapanmış mum 13:00-14:00.
        // O mumu işlersek kullanıcı alarmı kurar kurmaz işleme girer.
        let mut t = CandleTracker::new();
        assert_eq!(t.observe(&btc(), Interval::H1, &[k(200, 90.0)], 250), None);
        assert!(t.is_primed(&btc(), Interval::H1));
    }

    #[test]
    fn hizalandiktan_sonra_yeni_kapanis_doner() {
        let mut t = CandleTracker::new();
        t.observe(&btc(), Interval::H1, &[k(200, 90.0)], 250);

        let yeni = t.observe(&btc(), Interval::H1, &[k(200, 90.0), k(300, 95.0)], 350);
        assert_eq!(yeni.unwrap().close, 95.0);
    }

    #[test]
    fn ayni_mum_iki_kez_atesetmez() {
        let mut t = CandleTracker::new();
        t.observe(&btc(), Interval::H1, &[k(200, 90.0)], 250);
        let ks = [k(200, 90.0), k(300, 95.0)];

        assert!(t.observe(&btc(), Interval::H1, &ks, 350).is_some());
        // Aynı yanıt tekrar geldi — alarm ikinci kez ateşlememeli.
        assert!(t.observe(&btc(), Interval::H1, &ks, 360).is_none());
        assert!(t.observe(&btc(), Interval::H1, &ks, 370).is_none());
    }

    #[test]
    fn devam_eden_mum_yeni_kapanis_sayilmaz() {
        let mut t = CandleTracker::new();
        t.observe(&btc(), Interval::H1, &[k(200, 90.0)], 250);

        // 300'de kapanacak mum hâlâ açık (now=280). Kapanmış olan hâlâ 200.
        let ks = [k(200, 90.0), k(300, 999.0)];
        assert!(t.observe(&btc(), Interval::H1, &ks, 280).is_none());

        // Kapanınca ateşler.
        assert_eq!(
            t.observe(&btc(), Interval::H1, &ks, 300).unwrap().close,
            999.0
        );
    }

    #[test]
    fn geriye_giden_yanit_durumu_bozmaz() {
        // Sunucu lag'i / yeniden sıralama: eski bir yanıt gelirse ilerlemeyi
        // geri almamalı, yoksa aynı mumu tekrar ateşleriz.
        let mut t = CandleTracker::new();
        t.observe(&btc(), Interval::H1, &[k(200, 90.0)], 250);
        assert!(t
            .observe(&btc(), Interval::H1, &[k(200, 90.0), k(300, 95.0)], 350)
            .is_some());

        // Eski yanıt geldi.
        assert!(t
            .observe(&btc(), Interval::H1, &[k(200, 90.0)], 350)
            .is_none());
        // Durum hâlâ 300'de: 300 tekrar ateşlenmemeli.
        assert!(t
            .observe(&btc(), Interval::H1, &[k(200, 90.0), k(300, 95.0)], 350)
            .is_none());
    }

    #[test]
    fn timeframeler_birbirinden_bagimsiz() {
        let mut t = CandleTracker::new();
        t.observe(&btc(), Interval::H1, &[k(200, 90.0)], 250);
        // H4 ayrı bir akış; kendi ilk gözlemi de hizalama olmalı.
        assert!(t
            .observe(&btc(), Interval::H4, &[k(200, 90.0)], 250)
            .is_none());
        assert!(t.is_primed(&btc(), Interval::H4));
    }

    #[test]
    fn semboller_birbirinden_bagimsiz() {
        let mut t = CandleTracker::new();
        let eth: Symbol = "ETH-USD".into();
        t.observe(&btc(), Interval::H1, &[k(200, 90.0)], 250);
        assert!(t
            .observe(&eth, Interval::H1, &[k(200, 3000.0)], 250)
            .is_none());
        assert!(t.is_primed(&eth, Interval::H1));
    }

    #[test]
    fn unutulan_sembol_yeniden_hizalanir() {
        let mut t = CandleTracker::new();
        t.observe(&btc(), Interval::H1, &[k(200, 90.0)], 250);
        t.forget(&btc(), Interval::H1);
        assert!(!t.is_primed(&btc(), Interval::H1));
        // Yeniden ilk gözlem: yine ateşlememeli.
        assert!(t
            .observe(&btc(), Interval::H1, &[k(300, 95.0)], 350)
            .is_none());
    }

    #[test]
    fn kapanmis_mum_yoksa_hizalanmaz() {
        // Hepsi açıksa durum kaydetmeyiz; sonraki poll gerçek ilk gözlem olur.
        let mut t = CandleTracker::new();
        assert!(t
            .observe(&btc(), Interval::H1, &[k(300, 95.0)], 250)
            .is_none());
        assert!(!t.is_primed(&btc(), Interval::H1));
    }
}
