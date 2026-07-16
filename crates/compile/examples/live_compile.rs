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
    Alert, AlertAction, AlertId, AlertState, Condition, Cross, ExitLeg, Exits, Interval, Side,
    Symbol, TradeSpec,
};

const API: &str = "https://staging-api.bulk.trade/api/v1";

#[derive(serde::Deserialize)]
struct Keys {
    master: String,
    builder: String,
}

fn alert(condition: Condition, exits: Option<Exits>) -> Alert {
    Alert {
        id: AlertId::new("live"),
        owner: String::new(),
        account: String::new(),
        condition,
        invalidate: None,
        action: AlertAction::Trade(TradeSpec {
            symbol: Symbol::new("BTC-USD"),
            side: Side::Buy,
            size: 0.001,
            exits,
        }),
        state: AlertState::Armed,
        armed_at_ms: 0,
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
    let pubkey = Keypair::from_base58(&keys.master)
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
        Some(Exits::simple(px * 0.90, px * 1.10)),
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
        Some(Exits::simple(px * 0.90, px * 1.10)),
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

    // ─── 3. Kademeli çıkış: TP1 %30 / TP2 %70 + SL1 %50 / SL2 %50 ──────────
    // Kullanıcının gerçek kurgusu. Birim testler payload'ın şeklini gösteriyor;
    // burada borsanın gerçekten kabul edip emirleri kurduğunu kanıtlıyoruz.
    //
    // Önce 1. ve 2. adımın bıraktığı collar'ları temizle, yoksa sayım şişer.
    gonder(
        &mut signer,
        vec![bulk_keychain::OrderItem::CancelAll(
            bulk_keychain::CancelAll::all(),
        )],
    )
    .await?;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let mut a = alert(
        Condition::CandleClose {
            symbol: Symbol::new("BTC-USD"),
            interval: Interval::M15,
            cross: Cross::Above,
            price: 90_000.0,
        },
        Some(Exits {
            take_profits: vec![ExitLeg::new(px * 1.05, 30.0), ExitLeg::new(px * 1.10, 70.0)],
            stops: vec![ExitLeg::new(px * 0.92, 50.0), ExitLeg::new(px * 0.90, 50.0)],
        }),
    );
    if let AlertAction::Trade(spec) = &mut a.action {
        spec.size = 0.004; // kademeler bölünebilsin
    }

    let Compiled::Watched { items } = compile(&a, &builder_pk)? else {
        return Err("Watched bekleniyordu".into());
    };
    println!("\n=== 3. Kademeli çıkış ([m, of{{p:0,[tp,tp,st,st]}}]) ===");
    println!("derlenen: {} item", items.len());

    let resp = gonder(&mut signer, items).await?;
    println!("{resp}");
    if resp.contains("rejected") || resp.contains("\"ok\":false") {
        return Err(format!("borsa reddetti: {resp}").into());
    }

    // ⚠️ "resting" demesi emrin var olduğunu KANITLAMIYOR: yanlış is_buy'lı bir
    // st de "resting" + geçerli oid döndürüp hiç oluşmuyor. Tek doğrulama yolu
    // openOrders'ı ayrıca sorgulamak.
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    let n = koruma_emirleri(&pubkey).await?;
    println!("openOrders'da duran koruma emri: {n} (4 bekleniyor)");
    assert_eq!(
        n, 4,
        "kademeler kurulmadı — is_buy tuzağına düşmüş olabiliriz"
    );

    println!("\n✅ domain → compiler → keychain → borsa → fee zinciri çalışıyor");
    println!("✅ kademeli çıkış borsada kuruldu");
    Ok(())
}

/// BTC-USD'de duran koruma emirlerinin sayısı.
///
/// `openOrders`'ı ayrıca sorgulamak zorundayız: borsa yanlış `is_buy`'lı bir
/// `st`'ye de `"resting"` + geçerli `oid` dönüyor ama emri hiç oluşturmuyor.
async fn koruma_emirleri(pubkey: &str) -> Result<usize, Box<dyn std::error::Error>> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{API}/account"))
        .json(&serde_json::json!({ "type": "fullAccount", "user": pubkey }))
        .send()
        .await?
        .json()
        .await?;
    Ok(v[0]["fullAccount"]["openOrders"]
        .as_array()
        .map_or(0, |os| {
            os.iter()
                .filter(|o| {
                    matches!(
                        o["orderType"].as_str(),
                        Some("stop" | "takeProfit" | "range")
                    )
                })
                .count()
        }))
}
