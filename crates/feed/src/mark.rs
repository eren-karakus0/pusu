//! Mark price kaynağı.
//!
//! # Ne zaman gerekiyor
//!
//! Tek başına bir `MarkCross` koşulu watcher'a hiç düşmüyor — zincire `trig`
//! olarak gömülüyor ve borsa yürütüyor. Ama **bileşik** bir koşulun bacağı
//! olarak gelebilir: "BTC saatlik 90 binin üstünde kapatsın **ve** mark 88.4k'nın
//! altına inmemiş olsun". Böyle bir koşul zincire sığmadığı için watcher'a
//! düşüyor ve mark'ı bizim beslememiz gerekiyor.
//!
//! # Mark neden özel bir fiyat
//!
//! Borsanın tetikleyici fiyatı bu: üç değerin medyanı — premium'a göre
//! düzeltilmiş Pyth oracle'ı, gürültüsü alınmış defter fiyatı ve bunun 30
//! saniyelik EMA'sı. Son işlem fiyatı **değil**. `trig` emirleri mark üzerinden
//! tetiklendiği için, watcher tarafında da aynı fiyatı okumak zorundayız;
//! yoksa aynı koşul iki sınıfta farklı davranır.

use crate::source::FeedError;
use pusu_core::Symbol;

/// Mark price kaynağı. Trait olması testlerin ağa çıkmamasını sağlıyor.
#[allow(async_fn_in_trait)]
pub trait MarkSource {
    async fn mark(&self, symbol: &Symbol) -> Result<f64, FeedError>;
}

/// BULK REST API üzerinden mark price.
#[derive(Debug, Clone)]
pub struct HttpMarkSource {
    client: reqwest::Client,
    base_url: String,
}

impl HttpMarkSource {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
        }
    }
}

impl MarkSource for HttpMarkSource {
    async fn mark(&self, symbol: &Symbol) -> Result<f64, FeedError> {
        // Dikkat: sembol path parametresi. `/ticker?symbol=BTC-USD` 404 veriyor.
        let v: serde_json::Value = self
            .client
            .get(format!("{}/ticker/{}", self.base_url, symbol.as_str()))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        v["markPrice"]
            .as_f64()
            .ok_or_else(|| FeedError::Decode(format!("{} için markPrice yok", symbol.as_str())))
    }
}
