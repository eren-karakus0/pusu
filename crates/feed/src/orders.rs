//! Açık emir kaynağı.
//!
//! # Neden gerekiyor
//!
//! Limit giriş defterde bekliyor ve kullanıcının kuralı şu: *"retest gelmez de
//! dolmazsa, bir periyot sonra bana sor."* Bunu bilmenin tek yolu emrin hâlâ
//! orada olup olmadığına bakmak.
//!
//! # ⚠️ Gönderim yanıtı emrin varlığını kanıtlamıyor
//!
//! Bu modül yalnızca bir kolaylık değil, **doğrulama aracı**. Staging'de
//! ölçüldü (§8.10): yanlış `is_buy`'lı bir `st` emrine borsa
//! `{"resting":{"oid":"..."}}` + geçerli bir oid döndürüyor — ama emir hiç var
//! olmuyor. `Ok ≠ başarı` kuralının en sinsi hali: burada `status` bile
//! "resting" diyor.
//!
//! Yani "emrim durdu mu?" sorusunun tek dürüst cevabı `openOrders`'ı ayrıca
//! sorgulamak.

use crate::source::FeedError;

/// Defterde duran bir emir.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenOrder {
    pub oid: String,
    pub symbol: String,
    /// Emrin kalan boyutu. Satış tarafı **negatif** geliyor.
    pub size: f64,
    /// Şimdiye kadar dolan miktar.
    pub filled: f64,
    /// `limit` | `stop` | `takeProfit` | `range` | …
    pub order_type: String,
    pub reduce_only: bool,
}

impl OpenOrder {
    /// Hiç dolmamış mı? (Kullanıcının "emir alınmazsa" dediği hal.)
    pub fn untouched(&self) -> bool {
        self.filled.abs() < 1e-12
    }
}

/// Açık emir kaynağı. Trait olması testlerin ağa çıkmamasını sağlıyor.
#[allow(async_fn_in_trait)]
pub trait OrderSource {
    async fn open_orders(&self, account: &str) -> Result<Vec<OpenOrder>, FeedError>;
}

/// BULK REST API üzerinden açık emirler.
#[derive(Debug, Clone)]
pub struct HttpOrderSource {
    client: reqwest::Client,
    base_url: String,
}

impl HttpOrderSource {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            client: crate::http::client(),
            base_url: base_url.into(),
        }
    }
}

impl OrderSource for HttpOrderSource {
    async fn open_orders(&self, account: &str) -> Result<Vec<OpenOrder>, FeedError> {
        // GET /account 405 veriyor; POST + {"type":"fullAccount"} gerekiyor.
        let v: serde_json::Value = self
            .client
            .post(format!("{}/account", self.base_url))
            .json(&serde_json::json!({ "type": "fullAccount", "user": account }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let orders = v[0]["fullAccount"]["openOrders"]
            .as_array()
            .ok_or_else(|| FeedError::Decode("yanıtta openOrders yok".into()))?;

        Ok(orders.iter().filter_map(parse).collect())
    }
}

/// Tek bir emri çözümle. Anlaşılmayanı atlıyoruz — bir alanı okuyamadık diye
/// tüm listeyi düşürmek, tek bir emri kaçırmaktan beter olurdu.
fn parse(o: &serde_json::Value) -> Option<OpenOrder> {
    Some(OpenOrder {
        oid: o["orderId"].as_str()?.to_string(),
        symbol: o["symbol"].as_str().unwrap_or_default().to_string(),
        size: o["size"].as_f64().unwrap_or(0.0),
        filled: o["filledSize"].as_f64().unwrap_or(0.0),
        order_type: o["orderType"].as_str().unwrap_or("?").to_string(),
        reduce_only: o["reduceOnly"].as_bool().unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Staging'den birebir alınmış bir kayıt.
    fn gercek() -> serde_json::Value {
        json!({
            "symbol": "BTC-USD",
            "orderId": "5o8eezPYAoiyMuLJ3QR7VFAeBhB6ygXhi1m2faiXgT6",
            "price": 72005.5875,
            "originalSize": 0.001,
            "size": 0.001,
            "filledSize": 0.0,
            "vwap": 72005.5875,
            "maker": true,
            "reduceOnly": true,
            "iso": false,
            "orderType": "range",
            "trigger": {"px": 58913.6625, "oco": "5o8ee", "pxHi": 72005.5875},
            "tif": "gtc",
            "status": "resting",
            "timestamp": 1784181573848503957i64
        })
    }

    #[test]
    fn gercek_yanit_cozumleniyor() {
        let o = parse(&gercek()).unwrap();
        assert_eq!(o.oid, "5o8eezPYAoiyMuLJ3QR7VFAeBhB6ygXhi1m2faiXgT6");
        assert_eq!(o.order_type, "range");
        assert!(o.reduce_only);
        assert!(o.untouched(), "filledSize 0 → hiç dolmamış");
    }

    #[test]
    fn kismen_dolan_emir_untouched_degil() {
        // Kullanıcı "emir alınmazsa sor" dedi. Kısmen alındıysa alınmıştır.
        let mut v = gercek();
        v["filledSize"] = json!(0.0004);
        assert!(!parse(&v).unwrap().untouched());
    }

    #[test]
    fn satis_tarafi_negatif_geliyor() {
        let mut v = gercek();
        v["size"] = json!(-0.002);
        assert_eq!(parse(&v).unwrap().size, -0.002);
    }

    #[test]
    fn oidsiz_kayit_atlaniyor() {
        // Tek bir bozuk kaydı yüzünden tüm listeyi düşürmek, bir emri
        // kaçırmaktan beter olurdu.
        let mut v = gercek();
        v["orderId"] = json!(null);
        assert!(parse(&v).is_none());
    }
}
