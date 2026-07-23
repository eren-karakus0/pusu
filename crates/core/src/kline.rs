//! Mum verisi ve **kapanmış mum tespiti**.
//!
//! Buradaki kural ürünün doğruluğunu taşıyor:
//!
//! > `/klines` bazen **devam eden** mumu da döndürüyor. Son elemanı kapanmış
//! > sanmak, "saatlik kapanış 90 binin üstünde olsun" alarmını erken ateşler —
//! > yani kullanıcının kaçınmak istediği şeyin ta kendisini yapar.
//!
//! Staging'de ölçüldü, tutarsız: ardışık üç çağrıda son elemanın `T`'si sırayla
//! 2,1 sn geçmiş / 2,6 sn **gelecek** / 2,9 sn geçmiş çıktı. Bu yüzden daima
//! `T <= now` filtreliyoruz.
//!
//! Tip `core`'da (feed değil): tarayıcı (wasm) arayüzü de canlı grafik için mum
//! çekiyor ama `pusu-feed`'i (reqwest/tokio) import edemiyor. Saf serde olduğu
//! için burada duruyor; `pusu-feed` re-export ediyor.

use serde::{Deserialize, Serialize};

/// BULK `/klines` yanıtındaki tek mum.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Kline {
    /// Açılış zamanı (ms).
    #[serde(rename = "t")]
    pub open_time: u64,
    /// Kapanış zamanı (ms). `T > now` ise bu mum **hâlâ açık**.
    #[serde(rename = "T")]
    pub close_time: u64,
    #[serde(rename = "o")]
    pub open: f64,
    #[serde(rename = "h")]
    pub high: f64,
    #[serde(rename = "l")]
    pub low: f64,
    #[serde(rename = "c")]
    pub close: f64,
    #[serde(rename = "v")]
    pub volume: f64,
    #[serde(rename = "n")]
    pub num_trades: u64,
}

impl Kline {
    /// Bu mum verilen ana göre kapanmış mı?
    pub const fn is_closed_at(&self, now_ms: u64) -> bool {
        self.close_time <= now_ms
    }
}

/// Kapanmış mumların sonuncusu.
///
/// `/klines` artan sırada dönüyor ama buna güvenmiyoruz — kapanmışlar
/// arasından en geç kapananı seçiyoruz.
pub fn last_closed(klines: &[Kline], now_ms: u64) -> Option<&Kline> {
    klines
        .iter()
        .filter(|k| k.is_closed_at(now_ms))
        .max_by_key(|k| k.close_time)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(open_time: u64, close_time: u64, close: f64) -> Kline {
        Kline {
            open_time,
            close_time,
            open: 0.0,
            high: 0.0,
            low: 0.0,
            close,
            volume: 0.0,
            num_trades: 0,
        }
    }

    #[test]
    fn devam_eden_mum_atlanir() {
        // Staging'de gözlemlenen durum: son eleman henüz kapanmamış.
        // Onu almak "saatlik kapanış" alarmını erken ateşlerdi.
        let ks = [k(100, 200, 90.0), k(200, 300, 95.0)];
        let now = 250; // ikinci mum hâlâ açık
        assert_eq!(last_closed(&ks, now).unwrap().close, 90.0);
    }

    #[test]
    fn tam_sinirda_kapanmis_sayilir() {
        // T == now: mum kapandı. Aksi halde her mumu bir tick geç işlerdik.
        let ks = [k(100, 200, 90.0)];
        assert!(ks[0].is_closed_at(200));
        assert_eq!(last_closed(&ks, 200).unwrap().close, 90.0);
    }

    #[test]
    fn hepsi_acikken_none_doner() {
        let ks = [k(200, 300, 95.0)];
        assert!(last_closed(&ks, 250).is_none());
    }

    #[test]
    fn bos_liste_none_doner() {
        assert!(last_closed(&[], 1000).is_none());
    }

    #[test]
    fn siraya_guvenmiyoruz() {
        // Yanıt ters sırada gelse bile en geç kapananı buluyoruz.
        let ks = [k(200, 300, 95.0), k(100, 200, 90.0)];
        assert_eq!(last_closed(&ks, 350).unwrap().close, 95.0);
    }

    #[test]
    fn json_bulk_formatindan_parse_olur() {
        // /klines yanıtının birebir şekli.
        let raw = r#"[{"t":1784120400000,"T":1784124000000,"o":65141.4,"h":65200.0,
                       "l":65000.0,"c":65085.2,"v":192.4068,"n":314492}]"#;
        let ks: Vec<Kline> = serde_json::from_str(raw).unwrap();
        assert_eq!(ks[0].close_time, 1784124000000);
        assert_eq!(ks[0].close, 65085.2);
        assert_eq!(ks[0].num_trades, 314492);
    }
}
