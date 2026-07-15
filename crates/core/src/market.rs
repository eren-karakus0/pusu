//! Market temelleri: sembol, zaman dilimi, yön.

use serde::{Deserialize, Serialize};
use std::fmt;

/// İşlem çifti, örn. `BTC-USD`.
///
/// BULK sembolleri serbest metin değil; borsanın `exchangeInfo`'da döndürdüğü
/// listeyle sınırlı. Doğrulama burada değil, market kataloğunda yapılır —
/// bu tip yalnızca taşır.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Symbol(String);

impl Symbol {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Symbol {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

/// Mum zaman dilimi. BULK'ın `candle.{symbol}.{interval}` stream'inin
/// desteklediği 16 değerin birebir karşılığı.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Interval {
    #[serde(rename = "10s")]
    S10,
    #[serde(rename = "1m")]
    M1,
    #[serde(rename = "3m")]
    M3,
    #[serde(rename = "5m")]
    M5,
    #[serde(rename = "15m")]
    M15,
    #[serde(rename = "30m")]
    M30,
    #[serde(rename = "1h")]
    H1,
    #[serde(rename = "2h")]
    H2,
    #[serde(rename = "4h")]
    H4,
    #[serde(rename = "6h")]
    H6,
    #[serde(rename = "8h")]
    H8,
    #[serde(rename = "12h")]
    H12,
    #[serde(rename = "1d")]
    D1,
    #[serde(rename = "3d")]
    D3,
    #[serde(rename = "1w")]
    W1,
    #[serde(rename = "1M")]
    Mo1,
}

impl Interval {
    /// BULK'ın wire formatındaki karşılığı.
    pub const fn as_wire(&self) -> &'static str {
        match self {
            Self::S10 => "10s",
            Self::M1 => "1m",
            Self::M3 => "3m",
            Self::M5 => "5m",
            Self::M15 => "15m",
            Self::M30 => "30m",
            Self::H1 => "1h",
            Self::H2 => "2h",
            Self::H4 => "4h",
            Self::H6 => "6h",
            Self::H8 => "8h",
            Self::H12 => "12h",
            Self::D1 => "1d",
            Self::D3 => "3d",
            Self::W1 => "1w",
            Self::Mo1 => "1M",
        }
    }

    /// Kullanıcıya gösterilecek Türkçe ad.
    pub const fn label(&self) -> &'static str {
        match self {
            Self::S10 => "10 saniyelik",
            Self::M1 => "1 dakikalık",
            Self::M3 => "3 dakikalık",
            Self::M5 => "5 dakikalık",
            Self::M15 => "15 dakikalık",
            Self::M30 => "30 dakikalık",
            Self::H1 => "saatlik",
            Self::H2 => "2 saatlik",
            Self::H4 => "4 saatlik",
            Self::H6 => "6 saatlik",
            Self::H8 => "8 saatlik",
            Self::H12 => "12 saatlik",
            Self::D1 => "günlük",
            Self::D3 => "3 günlük",
            Self::W1 => "haftalık",
            Self::Mo1 => "aylık",
        }
    }
}

impl fmt::Display for Interval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_wire())
    }
}

/// İşlem yönü.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub const fn is_buy(&self) -> bool {
        matches!(self, Self::Buy)
    }

    pub const fn label(&self) -> &'static str {
        match self {
            Self::Buy => "long",
            Self::Sell => "short",
        }
    }
}

/// Fiyatın eşiği hangi yönden kestiği.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cross {
    /// Eşiğin üstüne çıkıyor.
    Above,
    /// Eşiğin altına iniyor.
    Below,
}

impl Cross {
    /// BULK'ın conditional emirlerindeki `d` alanı.
    pub const fn is_above(&self) -> bool {
        matches!(self, Self::Above)
    }

    pub const fn label(&self) -> &'static str {
        match self {
            Self::Above => "üstüne çıkarsa",
            Self::Below => "altına inerse",
        }
    }

    /// Verilen fiyat eşiği bu yönde geçmiş mi?
    pub fn is_met(&self, price: f64, threshold: f64) -> bool {
        match self {
            Self::Above => price >= threshold,
            Self::Below => price <= threshold,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cross_above_esigin_ustunde_ve_tam_esikte_saglanir() {
        assert!(Cross::Above.is_met(100.0, 90.0));
        assert!(Cross::Above.is_met(90.0, 90.0));
        assert!(!Cross::Above.is_met(89.9, 90.0));
    }

    #[test]
    fn cross_below_esigin_altinda_ve_tam_esikte_saglanir() {
        assert!(Cross::Below.is_met(80.0, 90.0));
        assert!(Cross::Below.is_met(90.0, 90.0));
        assert!(!Cross::Below.is_met(90.1, 90.0));
    }

    #[test]
    fn interval_wire_formati_bulk_ile_ayni() {
        assert_eq!(Interval::H1.as_wire(), "1h");
        assert_eq!(Interval::Mo1.as_wire(), "1M");
        assert_eq!(Interval::S10.as_wire(), "10s");
    }
}
