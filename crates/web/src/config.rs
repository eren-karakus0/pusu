//! Ortam sabitleri.

/// BULK staging REST kökü (sonda `/` yok).
pub const BULK_URL: &str = "https://staging-api.bulk.trade/api/v1";

/// PUSU'nun builder pubkey'i — fee'nin yazılacağı hesap.
///
/// ⚠️ PLACEHOLDER (system program). Staging'de PUSU'nun **kendi** anahtarıyla
/// değiştir; onay bu adrese yapılıyor ve fee buraya akıyor.
pub const BUILDER_PUBKEY: &str = "11111111111111111111111111111111";

/// Kestiğimiz builder fee (bps). `abc`'de onaylatılan ile aynı — onay=tahsilat.
pub const BUILDER_FEE_BPS: u8 = pusu_core::BUILDER_FEE_BPS;
