//! Tam zincir kanıtı: canlı feed → watcher ateşler → **gerçek imzalı emir** borsaya.
//!
//! `live_watch` karar mantığını sahte dispatch'le kanıtlıyor. Bu örnek son
//! halkayı kapatıyor: fonlu bir anahtarla alarmı derler, **raw ed25519** imzalar
//! (cüzdan/Phantom yok), watcher canlı 10s mumunda ateşleyince imzalı blob'u
//! staging'e POST eder. **postgres/store gerekmez** — blob bellekte tutulur.
//!
//! Güvenlik: varsayılan **DRY-RUN** — imzalar, blob'u basar, ama POST ETMEZ.
//! Gerçekten göndermek için `PUSU_LIVE=1`.
//!
//! ```bash
//! # 1) dry-run: imza + karar kanıtı, gönderim yok
//! PUSU_MASTER_SECRET=<base58-veya-json-array> cargo run -p pusu-node --example live_execute
//! # 2) canlı: blob'u gerçekten borsaya POST eder
//! PUSU_MASTER_SECRET=<...> PUSU_LIVE=1 cargo run -p pusu-node --example live_execute
//! ```
//!
//! Env:
//! - `PUSU_MASTER_SECRET` (zorunlu): fonlu master keypair. base58 secret **ya da**
//!   Solana CLI keypair.json biçimi (`[12,34,...]` 64 bayt).
//! - `PUSU_ACCOUNT` (ops): emrin gireceği hesap (base58). Yoksa master'ın kendisi.
//! - `PUSU_BASE` (ops): staging API kökü. Yoksa BULK staging.
//! - `PUSU_LIVE=1` (ops): blob'u gerçekten POST et. Yoksa dry-run.
//! - `PUSU_SIZE` (ops): işlem büyüklüğü (BTC). Yoksa 0.001.
//!
//! ⚠️ Fee'nin kabulü için master'ın builder'ı `abc` ile onaylamış olması gerekir;
//! aksi halde borsa emri reddeder (yanıt yazdırılır, çökme olmaz). Onay web
//! onboarding'de yapılıyor.

use pusu_core::{
    Alert, AlertAction, AlertId, AlertState, Condition, Cross, Entry, Interval, Side, Symbol,
    TradeSpec,
};
use pusu_engine::{Dispatch, DispatchError, Watcher};
use pusu_feed::{HttpKlineSource, HttpMarkSource, HttpOrderSource, MarkSource};
use pusu_sign::{finalize_bundle, prepare_alert, Routing};
use solana_keypair::Keypair;
use solana_signer::Signer;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// PUSU'nun staging builder pubkey'i (fee alıcısı) — `web::config::BUILDER_PUBKEY`.
const BUILDER: &str = "8nQev8LQfVMAECPy2KteMHEZqXAGbDWkLSY6n7o7YwSE";

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|s| !s.trim().is_empty())
}

/// Secret'ı base58 ya da JSON bayt-dizisi (`[..]`) biçiminden keypair'e çöz.
fn load_keypair(secret: &str) -> Result<Keypair, String> {
    let s = secret.trim();
    if s.starts_with('[') {
        // Solana CLI keypair.json: 64 bayt = [seed(32) ‖ pubkey(32)] (ya da düz 32 seed).
        let bytes: Vec<u8> = serde_json::from_str(s).map_err(|e| format!("keypair json: {e}"))?;
        let seed: [u8; 32] = match bytes.len() {
            64 | 32 => bytes[..32].try_into().unwrap(),
            n => return Err(format!("keypair 32 ya da 64 bayt olmalı, {n} geldi")),
        };
        Ok(Keypair::new_from_array(seed))
    } else {
        // from_base58_string geçersizde panikliyor; önce doğrula.
        bs58_len_ok(s)?;
        Ok(Keypair::from_base58_string(s))
    }
}

fn bs58_len_ok(s: &str) -> Result<(), String> {
    match bs58::decode(s).into_vec() {
        Ok(v) if v.len() == 64 => Ok(()),
        Ok(v) => Err(format!("secret 64 bayt olmalı, {} geldi", v.len())),
        Err(e) => Err(format!("secret base58 değil: {e}")),
    }
}

/// Bellekte tek imzalı blob tutan dispatch; ateşleyince (live ise) borsaya POST eder.
struct BlobDispatch {
    blob: serde_json::Value,
    base: String,
    live: bool,
    client: reqwest::Client,
}

impl Dispatch for BlobDispatch {
    async fn submit(&self, _alert: &Alert) -> Result<serde_json::Value, DispatchError> {
        if !self.live {
            println!("      → [dry-run] blob POST EDİLMEDİ (PUSU_LIVE=1 ile gönderilir):");
            println!(
                "        {}",
                serde_json::to_string(&self.blob).unwrap_or_default()
            );
            // Watcher 'Fired' işaretleyip erken çıksın diye sahte 'filled'
            // (live_watch'ün kanıtlı yanıt şekliyle birebir).
            return Ok(
                serde_json::json!({"status":"ok","response":{"data":{"statuses":[
                    {"filled":{"totalSz":0.001,"avgPx":0.0,"oid":"dry-run"}}
                ]}}}),
            );
        }
        println!("      → [LIVE] imzalı blob staging'e POST ediliyor...");
        let resp = self
            .client
            .post(format!("{}/order", self.base))
            .json(&self.blob)
            .send()
            .await
            .map_err(|e| DispatchError::Network(e.to_string()))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| DispatchError::Network(e.to_string()))?;
        println!("        borsa yanıtı: {text}");
        if !status.is_success() {
            return Err(DispatchError::Network(format!("HTTP {status}: {text}")));
        }
        serde_json::from_str(&text)
            .map_err(|e| DispatchError::Network(format!("yanıt çözülemedi: {e}")))
    }

    async fn cancel(&self, _alert: &Alert) -> Result<serde_json::Value, DispatchError> {
        // Market giriş → ön-imzalı iptal yok; bu yol kullanılmaz.
        Err(DispatchError::NoBlob)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let secret = env("PUSU_MASTER_SECRET").ok_or(
        "PUSU_MASTER_SECRET gerekli (fonlu master; base58 secret ya da keypair.json dizisi).",
    )?;
    let kp = load_keypair(&secret)?;
    let master = kp.pubkey().to_string();
    let account = env("PUSU_ACCOUNT").unwrap_or_else(|| master.clone());
    let base =
        env("PUSU_BASE").unwrap_or_else(|| "https://staging-api.bulk.trade/api/v1".to_string());
    let live = env("PUSU_LIVE").as_deref() == Some("1");
    let size: f64 = env("PUSU_SIZE")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.001);

    println!("API:     {base}");
    println!("master:  {master}");
    println!("account: {account}");
    println!("builder: {BUILDER}");
    println!("size:    {size} BTC");
    println!(
        "mode:    {}\n",
        if live {
            "🔴 LIVE (gerçek emir gönderilir)"
        } else {
            "🟡 DRY-RUN (imza var, POST yok)"
        }
    );

    let sym = Symbol::new("BTC-USD");
    let mark = HttpMarkSource::new(&base).mark(&sym).await?;
    println!("BTC-USD mark: {mark:.2}\n");

    let armed = now_ms();
    // 10s kapanış, eşik mark'ın %10 altında → ilk kapanışta ateşler.
    let alert = Alert {
        id: AlertId::new("live-execute"),
        owner: master.clone(),
        account: account.clone(),
        condition: Condition::CandleClose {
            symbol: sym.clone(),
            interval: Interval::S10,
            cross: Cross::Above,
            price: mark * 0.9,
        },
        invalidate: None,
        action: AlertAction::Trade(TradeSpec {
            symbol: sym.clone(),
            side: Side::Buy,
            size,
            entry: Entry::Market,
            exits: None,
        }),
        state: AlertState::Armed,
        armed_at_ms: armed,
        entry_oid: None,
        fill_deadline_ms: None,
        cancel_requested: false,
    };

    // Derle → hazırla → RAW imzala → finalize. (web `alert::submit` ile aynı hat,
    // yalnız cüzdan yerine anahtarla ham ed25519 imza.)
    let bundle = prepare_alert(&alert, BUILDER, &account, &master, now_ms())?;
    assert_eq!(
        bundle.routing,
        Routing::WatchedMarket,
        "mum kapanışı + market → WatchedMarket bekleniyordu"
    );
    let msg = bundle
        .entry
        .as_ref()
        .ok_or("giriş mesajı yok")?
        .message_bytes
        .clone();
    let sig_b58 = kp.sign_message(&msg).to_string(); // Signature Display = base58
    let signed = finalize_bundle(bundle, Some(&sig_b58), None);
    let blob = signed.entry.ok_or("imzalı blob üretilemedi")?;
    println!(
        "✅ alarm derlendi + raw imzalandı (sig {}…)\n",
        &sig_b58[..sig_b58.len().min(12)]
    );

    let mut alerts = vec![alert];
    let mut w = Watcher::new(
        HttpKlineSource::new(&base),
        HttpMarkSource::new(&base),
        HttpOrderSource::new(&base),
        BlobDispatch {
            blob,
            base: base.clone(),
            live,
            client: reqwest::Client::new(),
        },
    );

    println!("10s mumları izleniyor (~36 sn); kapanış eşiği geçince gönderilecek...\n");
    for i in 0..12 {
        let t = w.tick(&mut alerts, now_ms()).await;
        for e in &t.feed_errors {
            println!("  #{i}: ⚠️ feed: {e:?}");
        }
        for r in &t.fired {
            println!("  #{i}: 🔫 {} → {:?}", r.id.as_str(), r.outcome);
        }
        if t.fired.is_empty() && t.feed_errors.is_empty() {
            println!("  #{i}: sessiz");
        }
        if alerts[0].state != AlertState::Armed {
            break;
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    println!("\nSonuç: alarm {:?}", alerts[0].state);
    if live {
        println!("(LIVE: borsa yanıtı yukarıda — fill/reject onu gösterir.)");
    } else {
        println!("(DRY-RUN: imza + canlı karar kanıtlandı. Gerçek gönderim: PUSU_LIVE=1.)");
    }
    Ok(())
}
