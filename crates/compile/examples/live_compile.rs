//! Compiler'ın çıktısını **gerçek borsaya** gönderir.
//!
//! Birim testler payload'ın şeklini kanıtlıyor; bu örnek asıl soruyu
//! cevaplıyor: borsa kabul ediyor mu, tetikleniyor mu, fee işliyor mu?
//!
//! Uçtan uca zincir: domain → compiler → keychain → borsa → builder fee.
//!
//! ```bash
//! cargo run -p pusu-compile --example live_compile
//! ```
//!
//! `spike-keys.json` gerektirir (repo kökünde, gitignore'da).

use bulk_keychain::{Keypair, Signer};
use pusu_compile::{compile, Compiled};
use pusu_core::{
    Alert, AlertAction, AlertId, AlertState, Bracket, Condition, Cross, Interval, Side, Symbol,
    TradeSpec,
};

const API: &str = "https://staging-api.bulk.trade/api/v1";

#[derive(serde::Deserialize)]
struct Keys {
    master: String,
    builder: String,
}

fn alert(condition: Condition, bracket: Option<Bracket>) -> Alert {
    Alert {
        id: AlertId::new("live"),
        owner: String::new(),
        account: String::new(),
        condition,
        action: AlertAction::Trade(TradeSpec {
            symbol: Symbol::new("BTC-USD"),
            side: Side::Buy,
            size: 0.001,
            bracket,
        }),
        state: AlertState::Armed,
    }
}

async fn bakiye(pubkey: &str) -> Result<f64, Box<dyn std::error::Error>> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{API}/account"))
        .json(&serde_json::json!({ "type": "fullAccount", "user": pubkey }))
        .send()
        .await?
        .json()
        .await?;
    Ok(v[0]["fullAccount"]["margin"]["totalBalance"]
        .as_f64()
        .unwrap_or(0.0))
}

async fn mark_price() -> Result<f64, Box<dyn std::error::Error>> {
    // Sembol path parametresi, query değil.
    let v: serde_json::Value = reqwest::Client::new()
        .get(format!("{API}/ticker/BTC-USD"))
        .send()
        .await?
        .json()
        .await?;
    v["markPrice"]
        .as_f64()
        .ok_or_else(|| "ticker yanıtında markPrice yok".into())
}

async fn gonder(
    signer: &mut Signer,
    items: Vec<bulk_keychain::OrderItem>,
) -> Result<String, Box<dyn std::error::Error>> {
    let signed = signer
        .sign_group(items, None)
        .map_err(|e| format!("imza: {e:?}"))?;
    let body = serde_json::json!({
        "actions": signed.actions, "nonce": signed.nonce,
        "account": signed.account, "signer": signed.signer, "signature": signed.signature,
    });
    Ok(reqwest::Client::new()
        .post(format!("{API}/order"))
        .json(&body)
        .send()
        .await?
        .text()
        .await?)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let keys: Keys = serde_json::from_str(&std::fs::read_to_string("spike-keys.json")?)?;
    let kp = Keypair::from_base58(&keys.master).map_err(|e| format!("{e:?}"))?;
    let builder_pk = Keypair::from_base58(&keys.builder)
        .map_err(|e| format!("{e:?}"))?
        .pubkey()
        .to_base58();
    let mut signer = Signer::new(kp);

    let px = mark_price().await?;
    println!("BTC mark: {px}\n");

    // ─── 1. OnChain: mark price eşiği kesince al ───────────────────────────
    // Eşiği fiyatın üstüne koyup "altına inerse" diyoruz → anında tetiklenir.
    let a = alert(
        Condition::MarkCross {
            symbol: Symbol::new("BTC-USD"),
            cross: Cross::Below,
            price: px * 1.05,
        },
        Some(Bracket {
            stop: px * 0.90,
            take_profit: px * 1.10,
        }),
    );

    let Compiled::OnChain { items } = compile(&a, &builder_pk)? else {
        return Err("OnChain bekleniyordu".into());
    };
    println!("=== 1. OnChain (trig basket) ===");
    println!("derlenen: {} item", items.len());

    let once = bakiye(&builder_pk).await?;
    let resp = gonder(&mut signer, items).await?;
    println!("{resp}");
    if resp.contains("rejected") || resp.contains("\"ok\":false") || resp.contains("error") {
        return Err(format!("borsa reddetti: {resp}").into());
    }

    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    let sonra = bakiye(&builder_pk).await?;
    let beklenen = px * 0.001 * 0.0002;
    println!(
        "builder fee: {:.6} (beklenen ~{:.6})",
        sonra - once,
        beklenen
    );
    assert!(sonra > once, "fee işlemedi");

    // ─── 2. Watched: saatlik kapanış — kullanıcının kendi derdi ────────────
    // Watcher normalde koşulu bekler; burada blob'un geçerliliğini kanıtlamak
    // için doğrudan gönderiyoruz (watcher'ın yapacağı şeyin aynısı).
    let a = alert(
        Condition::CandleClose {
            symbol: Symbol::new("BTC-USD"),
            interval: Interval::H1,
            cross: Cross::Above,
            price: 90_000.0,
        },
        Some(Bracket {
            stop: px * 0.90,
            take_profit: px * 1.10,
        }),
    );

    let Compiled::Watched { items } = compile(&a, &builder_pk)? else {
        return Err("Watched bekleniyordu".into());
    };
    println!("\n=== 2. Watched (ön-imzalı [m, of]) ===");
    println!("derlenen: {} item", items.len());

    let once = bakiye(&builder_pk).await?;
    let resp = gonder(&mut signer, items).await?;
    println!("{resp}");
    if resp.contains("rejected") || resp.contains("\"ok\":false") {
        return Err(format!("borsa reddetti: {resp}").into());
    }

    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    let sonra = bakiye(&builder_pk).await?;
    println!(
        "builder fee: {:.6} (beklenen ~{:.6})",
        sonra - once,
        beklenen
    );
    assert!(sonra > once, "fee işlemedi");

    println!("\n✅ domain → compiler → keychain → borsa → fee zinciri çalışıyor");
    Ok(())
}
