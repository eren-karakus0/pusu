//! `/klines` kaynağı.
//!
//! # Neden WS değil
//!
//! WS candle aboneliği abonelik başına **~1,9 MB**'lık 5.000 mumluk geçmiş
//! döküyor ve 1 MB'lık varsayılan frame limitini aşıp bağlantıyı kopartıyor
//! (`1009 message too big`). 11 sembol × 4 timeframe ≈ 85 MB / reconnect.
//! Ayrıca `bulk-client`'ın candle handler'ı v0.1.2'de kırık: `data` alanını
//! (`{"candles":[...]}`) tek bir mum sanıyor ve `topic`'i hiç okumadığı için
//! çok sembollü abonelikte sembol bilgisi kayboluyor.
//!
//! Latency argümanı da yok: saatlik mumun kapanışını 2 saniye geç öğrenmek
//! hiçbir şeyi değiştirmiyor.
//!
//! # ⚠️ Neden `startTime` kullanmıyoruz
//!
//! Cazip görünüyor — `startTime` ile son saatlik mum 142 byte. **Ama bayat.**
//! Prod'da ölçüldü:
//!
//! ```text
//! now=...117362 | filtreli sonT=...070000 (-47sn) | filtresiz sonT=...120000 (+3sn)
//! now=...123741 | filtreli sonT=...070000 (-54sn) | filtresiz sonT=...120000 (-4sn)
//! now=...130378 | filtreli sonT=...070000 (-60sn) | filtresiz sonT=...130000 (-0sn)
//! now=...136774 | filtreli sonT=...130000 ( -7sn) | filtresiz sonT=...140000 (+3sn)
//! ```
//!
//! Filtreli yanıtın kuyruğu donuyor, ~60 saniyeye kadar geride kalıyor ve
//! partiler halinde sıçrıyor. `Cf-Cache-Status: DYNAMIC` — yani CDN cache'i
//! değil, gecikme origin tarafında. Bayat mumla "saatlik kapanış" alarmını
//! bir dakika geç ateşlemek, kullanıcının fiyatını kaçırması demek.
//!
//! Filtresiz yanıt taze ve hedef timeframe'lerimizde zaten ucuz:
//!
//! | Interval | Mum | Boyut | Kapsam |
//! |---|---|---|---|
//! | 1m | 10.421 | 1.345 KB | 7,2 gün |
//! | 15m | 695 | 92 KB | 7,2 gün |
//! | **1h** | 173 | **23 KB** | 7,2 gün |
//! | 4h | 43 | 6 KB | 7,2 gün |
//! | 1d | 7 | 1 KB | 7 gün |
//!
//! Her timeframe sabit **~7 günlük** geçmiş döndürüyor. 1m (1,3 MB) sık polling
//! için ağır; v1'de 15m ve üstünü destekliyoruz.
//!
//! `since_ms` parametresi API'de var ve geriye dönük veri çekmek için duruyor,
//! ama **canlı kapanış tespitinde kullanılmamalı**.

use crate::kline::Kline;
use pusu_core::{Interval, Symbol};

/// `/klines` çekme hatası.
#[derive(Debug, thiserror::Error)]
pub enum FeedError {
    #[error("klines isteği başarısız: {0}")]
    Request(#[from] reqwest::Error),
    #[error("klines yanıtı çözümlenemedi: {0}")]
    Decode(String),
}

/// Mum kaynağı. Trait olması testlerin ağa çıkmamasını sağlıyor.
#[allow(async_fn_in_trait)]
pub trait KlineSource {
    /// Mumları getir.
    ///
    /// `since_ms`'i **canlı kapanış tespitinde verme** — sunucu filtreli
    /// yanıtları ~60 sn geriden servis ediyor (modül dokümanına bak).
    /// Yalnızca geçmişe dönük veri çekerken anlamlı.
    async fn klines(
        &self,
        symbol: &Symbol,
        interval: Interval,
        since_ms: Option<u64>,
    ) -> Result<Vec<Kline>, FeedError>;

    /// Canlı kapanış tespiti için taze mumlar. `startTime` vermez.
    async fn fresh_klines(
        &self,
        symbol: &Symbol,
        interval: Interval,
    ) -> Result<Vec<Kline>, FeedError> {
        self.klines(symbol, interval, None).await
    }
}

/// BULK REST API üzerinden mum kaynağı.
#[derive(Debug, Clone)]
pub struct HttpKlineSource {
    client: reqwest::Client,
    base_url: String,
}

impl HttpKlineSource {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            client: crate::http::client(),
            base_url: base_url.into(),
        }
    }
}

impl KlineSource for HttpKlineSource {
    async fn klines(
        &self,
        symbol: &Symbol,
        interval: Interval,
        since_ms: Option<u64>,
    ) -> Result<Vec<Kline>, FeedError> {
        let mut req = self
            .client
            .get(format!("{}/klines", self.base_url))
            .query(&[
                ("symbol", symbol.as_str()),
                ("interval", interval.as_wire()),
            ]);

        if let Some(since) = since_ms {
            req = req.query(&[("startTime", since.to_string())]);
        }

        let resp = req.send().await?.error_for_status()?;
        resp.json::<Vec<Kline>>()
            .await
            .map_err(|e| FeedError::Decode(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ağa çıkmayan sahte kaynak.
    struct Fake(Vec<Kline>);

    impl KlineSource for Fake {
        async fn klines(
            &self,
            _symbol: &Symbol,
            _interval: Interval,
            _since_ms: Option<u64>,
        ) -> Result<Vec<Kline>, FeedError> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn sahte_kaynak_trait_uzerinden_calisir() {
        let f = Fake(vec![Kline {
            open_time: 100,
            close_time: 200,
            open: 1.0,
            high: 2.0,
            low: 0.5,
            close: 1.5,
            volume: 10.0,
            num_trades: 3,
        }]);
        let ks = f
            .klines(&"BTC-USD".into(), Interval::H1, None)
            .await
            .unwrap();
        assert_eq!(ks.len(), 1);
        assert_eq!(ks[0].close, 1.5);
    }
}
