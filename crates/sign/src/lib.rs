//! İmza köprüsü: derlenmiş alarmı **cüzdanın imzalayacağı** mesajlara, oradan
//! da store'a/borsaya gidecek blob'lara çevirir.
//!
//! Bu crate zincirin eksik halkası: `compile` ne emir gerektiğine karar
//! veriyor (`Vec<OrderItem>`), `bulk-keychain::prepare` onu kanonik olarak
//! serileştiriyor, cüzdan imzalıyor, `finalize` blob'u üretiyor. Hepsi tek
//! yerde, framework'ten bağımsız ve **wasm'a derleniyor** — böylece tüm hat
//! kullanıcının tarayıcısında koşabiliyor, sunucu hiçbir adımda anahtar
//! görmüyor.
//!
//! # İmza neden üç parça
//!
//! Cüzdanlar (Phantom vb.) ham private key vermiyor; `bulk-keychain` ise
//! imzayı ikiye bölüyor ve araya cüzdanı koyuyor:
//!
//! ```text
//! prepare  → message_bytes            (private key GEREKMEZ)
//! cüzdan   → wallet.signMessage(...)  → signature
//! finalize → { actions, nonce, account, signer, signature }
//! ```
//!
//! Şema düz Ed25519; Solana anahtarları da Ed25519 olduğu için cüzdanın
//! `message_bytes` üzerine attığı imza borsanın beklediği imza. Ayrıntı:
//! `docs/research/03-onboarding-signing.md`.
//!
//! # account ≠ signer
//!
//! Emir **sub-account'ta** çalışır (`account`), imzayı **master** cüzdanı atar
//! (`signer`). Bu ayrım §7'nin izolasyonunu taşıyor: master imzalar ama zarar
//! tavanı sub'a ayrılan miktarla sınırlı.

use bulk_keychain::{
    finalize_transaction, prepare_approve_builder_code, prepare_create_sub_account, prepare_group,
    prepare_revoke_builder_code, Cancel, CreateSubAccount, Hash, OrderItem, PreparedMessage,
    Pubkey, SignedTransaction,
};
use pusu_compile::{compile, CompileError, Compiled};
use pusu_core::{Alert, AlertAction};

/// İmzalanmış blob nereye gidecek?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Routing {
    /// 🔒 Borsaya **hemen** POST edilir; borsa tutar (sunucumuz ölse de çalışır).
    OnChain,
    /// ⚡ Store'a yazılır; koşul tutunca watcher market girişini gönderir.
    WatchedMarket,
    /// ⚡ Store'a yazılır; watcher limit girişi + ön-imzalı iptali yönetir.
    WatchedLimit,
    /// İmzalanacak bir şey yok (sadece bildirim).
    Notify,
}

/// Cüzdana sunulacak, imza bekleyen hazır mesajlar.
///
/// `entry` her işlemde var; `cancel` yalnızca limit girişte (ön-imzalı `cx`,
/// giriş oid'iyle kurulur). Kullanıcı ikisini de imzalar.
#[derive(Debug, Clone)]
pub struct PreparedBundle {
    pub routing: Routing,
    pub entry: Option<PreparedMessage>,
    pub cancel: Option<PreparedMessage>,
}

impl PreparedBundle {
    /// Girişin oid'i (varsa). Watched alarm store'a yazılırken `alert.entry_oid`
    /// buraya konuyor — watcher'ın dolum takibi ve mutabakatı buna bağlı.
    pub fn entry_oid(&self) -> Option<String> {
        self.entry.as_ref().and_then(entry_oid)
    }
}

/// İmzalanmış, POST'a hazır blob'lar.
#[derive(Debug, Clone, PartialEq)]
pub struct SignedBundle {
    pub routing: Routing,
    pub entry: Option<serde_json::Value>,
    pub cancel: Option<serde_json::Value>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SignError {
    #[error(transparent)]
    Compile(#[from] CompileError),
    #[error("geçersiz pubkey: {0}")]
    BadPubkey(String),
    /// Limit giriş ama oid hesaplanamadı — ön-imzalı iptal kurulamaz.
    #[error("limit giriş için oid hesaplanamadı")]
    NoEntryOid,
    /// bulk-keychain hazırlama hatası (nadir; boş grup, serileştirme).
    #[error("hazırlama başarısız: {0}")]
    Prepare(String),
}

/// Alarmı imzaya hazırla.
///
/// - `builder`: fee alıcısı (PUSU'nun pubkey'i)
/// - `account`: emrin gireceği hesap — **sub-account** (base58)
/// - `signer`: imzayı atacak — **master** cüzdanı (base58)
/// - `base_nonce`: giriş bunu, ön-imzalı iptal `base_nonce + 1`'i kullanır
///
/// `base_nonce` **çağrandan** geliyor (tarayıcıda `Date.now()`); crate saat
/// okumuyor ki wasm'da güvenle koşsun.
pub fn prepare_alert(
    alert: &Alert,
    builder: &str,
    account: &str,
    signer: &str,
    base_nonce: u64,
) -> Result<PreparedBundle, SignError> {
    let acct = pk(account)?;
    let sgnr = pk(signer)?;

    let items = match compile(alert, builder)? {
        Compiled::NotifyOnly => {
            return Ok(PreparedBundle {
                routing: Routing::Notify,
                entry: None,
                cancel: None,
            });
        }
        Compiled::OnChain { items } => {
            let entry = prep(items, &acct, &sgnr, base_nonce)?;
            return Ok(PreparedBundle {
                routing: Routing::OnChain,
                entry: Some(entry),
                cancel: None,
            });
        }
        Compiled::Watched { items } => items,
    };

    let entry = prep(items, &acct, &sgnr, base_nonce)?;

    // Giriş tipini alarmın kendisinden okuyoruz: limit ise ön-imzalı iptal şart.
    if !is_limit_entry(alert) {
        return Ok(PreparedBundle {
            routing: Routing::WatchedMarket,
            entry: Some(entry),
            cancel: None,
        });
    }

    let oid = entry_oid(&entry).ok_or(SignError::NoEntryOid)?;
    let oid_hash = Hash::from_base58(&oid).map_err(|_| SignError::NoEntryOid)?;
    let cancel = prep(
        vec![OrderItem::Cancel(Cancel::new(
            entry_symbol(alert),
            oid_hash,
        ))],
        &acct,
        &sgnr,
        base_nonce + 1,
    )?;

    Ok(PreparedBundle {
        routing: Routing::WatchedLimit,
        entry: Some(entry),
        cancel: Some(cancel),
    })
}

/// Cüzdandan gelen imzalarla blob'ları tamamla.
///
/// İmza yoksa (ör. kullanıcı iptali imzalamadıysa) o blob üretilmez.
pub fn finalize_bundle(
    bundle: PreparedBundle,
    entry_sig: Option<&str>,
    cancel_sig: Option<&str>,
) -> SignedBundle {
    SignedBundle {
        routing: bundle.routing,
        entry: bundle
            .entry
            .zip(entry_sig)
            .map(|(p, s)| body(finalize_transaction(p, s))),
        cancel: bundle
            .cancel
            .zip(cancel_sig)
            .map(|(p, s)| body(finalize_transaction(p, s))),
    }
}

/// Tek bir hazır mesajı imzayla blob'a çevir (onboarding tx'leri için).
pub fn finalize_one(prepared: PreparedMessage, signature: &str) -> serde_json::Value {
    body(finalize_transaction(prepared, signature))
}

// ── onboarding ──────────────────────────────────────────────────────────────

/// Sub-account açma tx'ini hazırla (account = signer = master).
pub fn prepare_create_subaccount(
    name: &str,
    margin: Option<f64>,
    master: &str,
    nonce: u64,
) -> Result<PreparedMessage, SignError> {
    let m = pk(master)?;
    let sa = match margin {
        Some(a) => CreateSubAccount::with_margin(name, a),
        None => CreateSubAccount::new(name),
    };
    prepare_create_sub_account(sa, &m, None, Some(nonce))
        .map_err(|e| SignError::Prepare(e.to_string()))
}

/// Builder onayı (`abc`) tx'ini hazırla. `fee` 1..=15 bps.
pub fn prepare_approve_builder(
    builder: &str,
    fee: u8,
    master: &str,
    nonce: u64,
) -> Result<PreparedMessage, SignError> {
    let to = pk(builder)?;
    let m = pk(master)?;
    prepare_approve_builder_code(&to, fee, &m, None, Some(nonce))
        .map_err(|e| SignError::Prepare(e.to_string()))
}

/// Builder onayını geri çekme (`rbc`) tx'ini hazırla — kill switch (§7).
pub fn prepare_revoke_builder(
    builder: &str,
    master: &str,
    nonce: u64,
) -> Result<PreparedMessage, SignError> {
    let to = pk(builder)?;
    let m = pk(master)?;
    prepare_revoke_builder_code(&to, &m, None, Some(nonce))
        .map_err(|e| SignError::Prepare(e.to_string()))
}

// ── iç yardımcılar ────────────────────────────────────────────────────────────

fn pk(s: &str) -> Result<Pubkey, SignError> {
    Pubkey::from_base58(s).map_err(|_| SignError::BadPubkey(s.to_string()))
}

fn prep(
    items: Vec<OrderItem>,
    account: &Pubkey,
    signer: &Pubkey,
    nonce: u64,
) -> Result<PreparedMessage, SignError> {
    prepare_group(items, account, Some(signer), Some(nonce))
        .map_err(|e| SignError::Prepare(e.to_string()))
}

/// Giriş her zaman index 0. Grup çok emirliyse `order_ids[0]`, tek emirse
/// `order_id`.
fn entry_oid(p: &PreparedMessage) -> Option<String> {
    p.order_ids
        .as_ref()
        .and_then(|v| v.first().cloned())
        .or_else(|| p.order_id.clone())
}

fn is_limit_entry(alert: &Alert) -> bool {
    matches!(&alert.action, AlertAction::Trade(s) if s.entry.is_limit())
}

fn entry_symbol(alert: &Alert) -> String {
    match &alert.action {
        AlertAction::Trade(s) => s.symbol.as_str().to_string(),
        // prepare_alert bu yola yalnızca Trade için giriyor.
        AlertAction::Notify => String::new(),
    }
}

/// `SignedTransaction`'ı borsanın `/order`'da beklediği gövdeye indir.
///
/// Yalnızca beş alan: staging'de birebir bu şekliyle çalıştığı doğrulandı
/// (`order_id`/`order_ids` gönderilmiyor).
fn body(t: SignedTransaction) -> serde_json::Value {
    serde_json::json!({
        "actions": t.actions,
        "nonce": t.nonce,
        "account": t.account,
        "signer": t.signer,
        "signature": t.signature,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pusu_core::{
        Alert, AlertAction, AlertId, AlertState, Condition, Cross, Entry, Exits, Interval, Side,
        Symbol, TradeSpec,
    };

    fn key(n: u8) -> String {
        Pubkey::from_bytes([n; 32]).to_base58()
    }

    const NONCE: u64 = 1_784_000_000_000;

    fn alarm(condition: Condition, entry: Entry) -> Alert {
        Alert {
            id: AlertId::new("a1"),
            owner: "master".into(),
            account: "sub".into(),
            condition,
            invalidate: None,
            action: AlertAction::Trade(TradeSpec {
                symbol: Symbol::new("BTC-USD"),
                side: Side::Buy,
                size: 0.01,
                entry,
                exits: Some(Exits::simple(88_000.0, 95_000.0)),
            }),
            state: AlertState::Armed,
            armed_at_ms: NONCE,
            entry_oid: None,
            fill_deadline_ms: None,
        }
    }

    fn mark_cross() -> Condition {
        Condition::MarkCross {
            symbol: Symbol::new("BTC-USD"),
            cross: Cross::Below,
            price: 88_400.0,
        }
    }

    fn candle() -> Condition {
        Condition::CandleClose {
            symbol: Symbol::new("BTC-USD"),
            interval: Interval::H1,
            cross: Cross::Above,
            price: 90_000.0,
        }
    }

    #[test]
    fn mark_cross_market_onchain_hemen_gonderilir() {
        let a = alarm(mark_cross(), Entry::Market);
        let b = prepare_alert(&a, &key(9), &key(1), &key(2), NONCE).unwrap();
        assert_eq!(b.routing, Routing::OnChain);
        assert!(b.entry.is_some());
        assert!(b.cancel.is_none(), "OnChain'de ön-imzalı iptal yok");
        // Giriş bir trig basket'i.
        assert!(b.entry.unwrap().actions[0].get("trig").is_some());
    }

    #[test]
    fn mum_kapanisi_market_watched_iptalsiz() {
        let a = alarm(candle(), Entry::Market);
        let b = prepare_alert(&a, &key(9), &key(1), &key(2), NONCE).unwrap();
        assert_eq!(b.routing, Routing::WatchedMarket);
        assert!(b.entry.is_some());
        assert!(
            b.cancel.is_none(),
            "market giriş beklemez, iptale gerek yok"
        );
    }

    #[test]
    fn mum_kapanisi_limit_watched_on_imzali_iptalli() {
        // Kullanıcının senaryosu: retest'te dolsun; dolmazsa watcher iptal etsin.
        let a = alarm(candle(), Entry::Limit { price: 89_500.0 });
        let b = prepare_alert(&a, &key(9), &key(1), &key(2), NONCE).unwrap();
        assert_eq!(b.routing, Routing::WatchedLimit);

        let entry = b.entry.as_ref().unwrap();
        let cancel = b.cancel.as_ref().unwrap();

        // İptal, GİRİŞ oid'ini hedeflemeli — yoksa yanlış emri iptal eder.
        let beklenen_oid = entry_oid(entry).unwrap();
        assert_eq!(b.entry_oid().as_deref(), Some(beklenen_oid.as_str()));
        assert_eq!(cancel.actions[0]["cx"]["oid"], beklenen_oid);
        assert_eq!(cancel.actions[0]["cx"]["c"], "BTC-USD");
        // Farklı nonce (nonce tek kullanımlık, §8.11).
        assert_eq!(entry.nonce, NONCE);
        assert_eq!(cancel.nonce, NONCE + 1);
    }

    #[test]
    fn notify_imzasiz() {
        let mut a = alarm(candle(), Entry::Market);
        a.action = AlertAction::Notify;
        let b = prepare_alert(&a, &key(9), &key(1), &key(2), NONCE).unwrap();
        assert_eq!(b.routing, Routing::Notify);
        assert!(b.entry.is_none() && b.cancel.is_none());
    }

    #[test]
    fn bozuk_pubkey_reddedilir() {
        let a = alarm(candle(), Entry::Market);
        let e = prepare_alert(&a, &key(9), "değil-base58-!!", &key(2), NONCE).unwrap_err();
        assert!(matches!(e, SignError::BadPubkey(_)));
    }

    #[test]
    fn finalize_imzalari_govdelere_yaziyor() {
        let a = alarm(candle(), Entry::Limit { price: 89_500.0 });
        let b = prepare_alert(&a, &key(9), &key(1), &key(2), NONCE).unwrap();
        let s = finalize_bundle(b, Some("giris-imza"), Some("iptal-imza"));

        let entry = s.entry.unwrap();
        assert_eq!(entry["signature"], "giris-imza");
        assert!(entry.get("actions").is_some());
        assert_eq!(entry["nonce"], NONCE);
        // Beş alan, fazlası yok (borsanın beklediği gövde).
        assert_eq!(entry.as_object().unwrap().len(), 5);

        assert_eq!(s.cancel.unwrap()["signature"], "iptal-imza");
    }

    #[test]
    fn finalize_imza_yoksa_blob_uretmiyor() {
        // Market giriş: iptal blob'u yok. İptal imzası verilse bile cancel None.
        let a = alarm(candle(), Entry::Market);
        let b = prepare_alert(&a, &key(9), &key(1), &key(2), NONCE).unwrap();
        let s = finalize_bundle(b, Some("giris"), None);
        assert!(s.entry.is_some());
        assert!(s.cancel.is_none());
    }

    #[test]
    fn onboarding_builder_onayi_abc_uretiyor() {
        let p = prepare_approve_builder(&key(9), 2, &key(1), NONCE).unwrap();
        assert_eq!(p.actions[0]["abc"]["fee"], 2);
        assert_eq!(p.actions[0]["abc"]["to"], key(9));
    }

    #[test]
    fn onboarding_sub_account_actiona_donusuyor() {
        let p = prepare_create_subaccount("pusu-1", Some(200.0), &key(1), NONCE).unwrap();
        let obj = &p.actions[0]["createSubAccount"];
        assert_eq!(obj["name"], "pusu-1");
        assert_eq!(obj["marginAmount"], 200.0);
    }

    #[test]
    fn onboarding_gecersiz_fee_reddedilir() {
        // Borsa 1..=15 kabul ediyor; dışını hazırlama aşamasında yakala.
        assert!(prepare_approve_builder(&key(9), 0, &key(1), NONCE).is_err());
        assert!(prepare_approve_builder(&key(9), 16, &key(1), NONCE).is_err());
    }
}
