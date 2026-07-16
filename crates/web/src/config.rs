//! Ortam sabitleri.

/// BULK staging REST kökü (sonda `/` yok).
pub const BULK_URL: &str = "https://staging-api.bulk.trade/api/v1";

/// PUSU ingress (Watched alarmları buraya yazılıyor). Lokalde `pusu-api`.
pub const PUSU_API_URL: &str = "http://localhost:3000";

/// PUSU'nun builder pubkey'i — fee'nin yazılacağı hesap.
///
/// PUSU'nun staging builder keypair'inin public key'i. Kullanıcılar `abc` ile
/// fee'yi bu adrese onaylıyor; fee buraya akıyor. Private key repoda değil
/// (fee cüzdanı; yalnızca çekim için gerekli). Mainnet'te ayrı anahtar.
pub const BUILDER_PUBKEY: &str = "8nQev8LQfVMAECPy2KteMHEZqXAGbDWkLSY6n7o7YwSE";

/// Kestiğimiz builder fee (bps). `abc`'de onaylatılan ile aynı — onay=tahsilat.
pub const BUILDER_FEE_BPS: u8 = pusu_core::BUILDER_FEE_BPS;
