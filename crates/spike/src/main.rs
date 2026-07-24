//! PUSU — Faz 0 doğrulama spike'ı
//!
//! Atılacak kod. Amaç: PLAN.md'deki varsayımları staging'de ampirik doğrulamak.
//! Ürün kodu buradan türetilmeyecek; sadece cevaplar alınacak.
//!
//! Çalıştırma:
//!   cargo run -p spike -- setup     # keypair üret + faucet
//!   cargo run -p spike -- probe     # açık soruları test et

use bulk_client::api::parts::HttpConfig;
use bulk_client::api::BulkHttpClient;
use bulk_client::transaction::TransactionSigner;
use clap::{Parser, Subcommand};
use solana_keypair::Keypair;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;

const STAGING_API: &str = "https://staging-api.bulk.trade/api/v1";
const KEYS_FILE: &str = "spike-keys.json";

#[derive(Parser, Debug)]
#[command(name = "spike", about = "PUSU Faz 0 doğrulama spike'ı")]
struct Cli {
    #[command(subcommand)]
    command: Command,

    #[arg(long, default_value = STAGING_API, global = true)]
    api_url: String,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Keypair'leri üret ve faucet'ten fonlamayı dene
    Setup,
    /// Mevcut hesap durumunu göster
    Status,
    /// S1: Builder code onayı + builderCode'lu emir (master'dan)
    BuilderBasic,
    /// S2: Trigger basket içine gömülü builderCode'lu emir
    TriggerNested,
    /// S3: Agent wallet transfer imzalayabiliyor mu?
    AgentTransfer,
    /// S4: Master'daki builder onayı sub-account'ta geçerli mi? ← EN KRİTİK
    SubAccountBuilder,
    /// S5: Aynı trigger basket'i bulk-keychain ile imzala (bulk-client bug hipotezi)
    TriggerKeychain,
    /// S6: Nonce'un ömrü var mı? (ön-imzalı tx tasarımı buna bağlı)
    NonceAge,
    /// S7: of bracket'i trigger'ın içindeki emir dolunca ateşliyor mu?
    OnfillBracket,
    /// S8: bracket'i trigger'a bağlamanın çalışan yolu hangisi?
    BracketVariants,
    /// S9: of'un parent'ı gerçek bir emir olunca çalışıyor mu?
    OnfillRealParent,
    /// S10: oid gönderim ÖNCESİ hesaplanabiliyor mu? (ön-imzalı iptal buna bağlı)
    OidPredict,
    /// S11: kademeli çıkış — TP1 dolup pozisyon küçülünce büyük kalan SL ne yapıyor?
    ExitLadder,
    /// S12: trig içine kademeli çıkış (tp+tp+st+st) sığıyor mu?
    TrigLadder,
    /// S13: aynı imzalı blob iki kez gönderilirse ne oluyor? (storage buna bağlı)
    NonceReplay,
    /// S14: server hangi imza modunu kabul ediyor? raw|base58|base64 — Phantom
    /// guardrail'ının çözümü buna bağlı (ham baytları cüzdan imzalayamıyor).
    SignModes,
    /// S15: x-bulk-sig-mode header'ı staging'de gerçekten uygulanıyor mu?
    SignDiag,
    /// S16: offchain zarf modu artık aktif mi? (ekip "resolved" dedi) — Phantom çözümü.
    SignOffchain,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
struct Keys {
    /// Ana hesap (kullanıcıyı temsil eder)
    master: String,
    /// Sub-account'a kaydedilecek agent (PUSU watcher'ını temsil eder)
    agent: String,
    /// Builder code alıcısı (PUSU'yu temsil eder)
    builder: String,
    /// Oluşturulan sub-account'ın pubkey'i (koşular arası saklanır)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sub: Option<String>,
}

fn save_keys(keys: &Keys) -> eyre::Result<()> {
    fs::write(keys_path(), serde_json::to_string_pretty(keys)?)?;
    Ok(())
}

fn keys_path() -> PathBuf {
    PathBuf::from(KEYS_FILE)
}

fn generate_key() -> String {
    let kp = Keypair::new();
    bs58::encode(kp.to_bytes()).into_string()
}

fn pubkey_of(secret_b58: &str) -> eyre::Result<String> {
    let signer = TransactionSigner::from_private_key(secret_b58)?;
    Ok(signer.public_key_b58())
}

fn load_or_create_keys() -> eyre::Result<Keys> {
    let path = keys_path();
    if path.exists() {
        let raw = fs::read_to_string(&path)?;
        let keys: Keys = serde_json::from_str(&raw)?;
        println!("🔑 Mevcut anahtarlar okundu: {}", path.display());
        Ok(keys)
    } else {
        let keys = Keys {
            master: generate_key(),
            agent: generate_key(),
            builder: generate_key(),
            sub: None,
        };
        fs::write(&path, serde_json::to_string_pretty(&keys)?)?;
        println!("🔑 Yeni anahtarlar üretildi: {}", path.display());
        Ok(keys)
    }
}

fn client(api_url: &str, secret: &str) -> eyre::Result<BulkHttpClient> {
    let signer = TransactionSigner::from_private_key(secret)?;
    BulkHttpClient::new(&HttpConfig {
        base_url: api_url.to_string(),
        signer: Some(signer),
        ..Default::default()
    })
}

async fn cmd_setup(api_url: &str) -> eyre::Result<()> {
    let keys = load_or_create_keys()?;

    println!("\n=== Hesaplar ===");
    println!("master : {}", pubkey_of(&keys.master)?);
    println!("agent  : {}", pubkey_of(&keys.agent)?);
    println!("builder: {}", pubkey_of(&keys.builder)?);

    println!("\n=== Faucet denemesi (master) ===");
    let c = client(api_url, &keys.master)?;
    match c.request_faucet(None, None, None).await {
        Ok(resp) => println!("✅ faucet yanıtı: {:?}", resp),
        Err(e) => println!("❌ faucet hatası: {e}"),
    }

    println!("\n=== Hesap durumu ===");
    let master_pk = TransactionSigner::from_private_key(&keys.master)?.public_key();
    match c.get_account(master_pk).await {
        Ok(acct) => println!("✅ hesap: {acct:#?}"),
        Err(e) => println!("❌ hesap sorgusu hatası: {e}"),
    }

    Ok(())
}

async fn cmd_status(api_url: &str) -> eyre::Result<()> {
    let keys = load_or_create_keys()?;
    let c = client(api_url, &keys.master)?;
    let master_pk = TransactionSigner::from_private_key(&keys.master)?.public_key();

    match c.get_account(master_pk).await {
        Ok(acct) => println!("{acct:#?}"),
        Err(e) => println!("❌ {e}"),
    }
    Ok(())
}

/// Yanıtı doğru yorumla: SDK reddedilen emirde Err DÖNMÜYOR,
/// Ok içinde status="rejectedInvalid" dönüyor. status okunmadan sonuç bilinemez.
fn verdict(label: &str, res: eyre::Result<Vec<bulk_client::msgs::Response>>) -> Option<String> {
    match res {
        Ok(rs) => {
            for r in &rs {
                let reason = r.raw.get("reason").and_then(|v| v.as_str()).unwrap_or("-");
                println!("   {label}: status={} reason={reason}", r.status);
            }
            rs.first().map(|r| r.status.clone())
        }
        Err(e) => {
            println!("   {label}: TRANSPORT HATASI: {e}");
            None
        }
    }
}

/// S1 — Builder code onayı çalışıyor mu, builderCode'lu emir fee kesiyor mu?
async fn cmd_builder_basic(api_url: &str) -> eyre::Result<()> {
    let keys = load_or_create_keys()?;
    let c = client(api_url, &keys.master)?;
    let master_pk = TransactionSigner::from_private_key(&keys.master)?.public_key();
    let builder_pk = TransactionSigner::from_private_key(&keys.builder)?.public_key();

    println!("=== 1. Onay ÖNCESİ builderCode'lu emir denemesi ===");
    println!("(onay yoksa reddedilmeli — reddedilmezse onay mekanizması delik demektir)");
    let res = place_market_with_builder(&c, master_pk, builder_pk, 2).await;
    match res {
        Ok(r) => println!("⚠️  ONAYSIZ GEÇTİ: {r:?}"),
        Err(e) => println!("✅ onaysız reddedildi: {e}"),
    }

    println!("\n=== 2. Builder code onayı (abc, fee=2bps) ===");
    match c.approve_builder_code(builder_pk, 2, None, None).await {
        Ok(r) => println!("✅ onay: {r:?}"),
        Err(e) => {
            println!("❌ onay hatası: {e}");
            return Ok(());
        }
    }

    println!("\n=== 3. Onay hesap snapshot'ında görünüyor mu? ===");
    match c.get_account(master_pk).await {
        Ok(a) => println!("commission_approvals: {:#?}", a.commission_approvals),
        Err(e) => println!("❌ {e}"),
    }

    println!("\n=== 4. Onay SONRASI builderCode'lu emir ===");
    match place_market_with_builder(&c, master_pk, builder_pk, 2).await {
        Ok(r) => println!("✅ emir: {r:?}"),
        Err(e) => println!("❌ emir hatası: {e}"),
    }

    println!("\n=== 5. Onaylanan tavanın ÜSTÜNDE fee (5bps > 2bps) ===");
    println!("(reddedilmeli — geçerse kullanıcı onayı anlamsız demektir)");
    match place_market_with_builder(&c, master_pk, builder_pk, 5).await {
        Ok(r) => println!("🚨 TAVAN AŞILDI, GEÇTİ: {r:?}"),
        Err(e) => println!("✅ tavan üstü reddedildi: {e}"),
    }

    println!("\n=== 6. Fee gerçekten kesildi mi? (builder hesabı) ===");
    match c.get_account(builder_pk).await {
        Ok(a) => println!("builder margin: {:#?}", a.margin),
        Err(e) => println!("❌ {e}"),
    }

    Ok(())
}

async fn place_market_with_builder(
    c: &BulkHttpClient,
    account: solana_pubkey::Pubkey,
    builder: solana_pubkey::Pubkey,
    fee_bps: u8,
) -> eyre::Result<Vec<bulk_client::msgs::Response>> {
    use bulk_client::msgs::order::{BuilderCode, MarketOrder};
    use bulk_client::transaction::{Action, ActionMeta};
    use std::sync::Arc;

    let order = MarketOrder {
        symbol: Arc::from("BTC-USD"),
        is_buy: true,
        size: 0.001,
        reduce_only: false,
        iso: false,
        builder_code: Some(BuilderCode {
            to: builder,
            fee: fee_bps,
        }),
        meta: ActionMeta {
            account,
            nonce: 0,
            seqno: 0,
            hash: None,
        },
    };
    c.place_tx(vec![Action::MarketOrder(order)], Some(account), None)
        .await
}

/// S4 — EN KRİTİK: master'daki builder onayı sub-account emrinde geçerli mi?
/// Hayırsa güvenlik (§7 sub-account izolasyonu) ile gelir modeli çakışır.
async fn cmd_subaccount_builder(api_url: &str) -> eyre::Result<()> {
    use bulk_client::msgs::subaccounts::CreateSubAccount;
    use bulk_client::transaction::{Action, ActionMeta};
    use std::sync::Arc;

    let mut keys = load_or_create_keys()?;
    let c = client(api_url, &keys.master)?;
    let master_pk = TransactionSigner::from_private_key(&keys.master)?.public_key();
    let builder_pk = TransactionSigner::from_private_key(&keys.builder)?.public_key();

    let sub_pk_str = if let Some(existing) = keys.sub.clone() {
        println!("=== 1. Mevcut sub-account kullanılıyor ===");
        existing
    } else {
        println!("=== 1. Sub-account oluştur (200 USDC ile) ===");
        let action = Action::CreateSubAccount(CreateSubAccount {
            name: Arc::from("pusu-test"),
            margin_amount: Some(200.0),
            meta: ActionMeta {
                account: master_pk,
                nonce: 0,
                seqno: 0,
                hash: None,
            },
        });
        let res = c.place_tx(vec![action], Some(master_pk), None).await;
        match &res {
            Ok(rs) => {
                for r in rs {
                    println!("   status={} raw={}", r.status, r.raw);
                }
            }
            Err(e) => println!("   ❌ {e}"),
        }
        let found = res
            .ok()
            .and_then(|rs| {
                rs.iter()
                    .find_map(|r| r.raw.get("sub").and_then(|v| v.as_str()).map(String::from))
            })
            .ok_or_else(|| eyre::eyre!("sub-account pubkey yanıtta yok"))?;
        keys.sub = Some(found.clone());
        save_keys(&keys)?;
        found
    };
    let sub_pk = solana_pubkey::Pubkey::from_str(&sub_pk_str)?;
    println!("   → sub: {sub_pk_str}");

    println!("\n=== 2. Master'ın onayı (2bps) — sub-account'ta da görünüyor mu? ===");
    match c.get_account(sub_pk).await {
        Ok(a) => {
            println!("   sub commission_approvals: {:?}", a.commission_approvals);
            println!("   sub margin: {:?}", a.margin.total_balance);
        }
        Err(e) => println!("   ❌ {e}"),
    }

    println!("\n=== 3. ⭐ SUB-ACCOUNT'TAN builderCode'lu emir (master imzalıyor) ===");
    println!("(geçerse: güvenlik + gelir birlikte çalışıyor. Reddedilirse mimari değişir.)");
    let r = place_market_with_builder(&c, sub_pk, builder_pk, 2).await;
    match r {
        Ok(rs) => {
            for x in &rs {
                let reason = x.raw.get("reason").and_then(|v| v.as_str()).unwrap_or("-");
                println!("   status={} reason={reason} raw={}", x.status, x.raw);
            }
        }
        Err(e) => println!("   ❌ {e}"),
    }

    println!("\n=== 4. Fee builder'a yazıldı mı? ===");
    match c.get_account(builder_pk).await {
        Ok(a) => println!("   builder balance: {}", a.margin.total_balance),
        Err(e) => println!("   ❌ {e}"),
    }

    Ok(())
}

/// S2 — Ürünün asıl mekaniği: trigger basket içine gömülü builderCode'lu emir.
/// Dokümanda yazmıyor; kaynak koddan (conditional.rs Trigger.actions: Vec<Action>) çıkarıldı.
async fn cmd_trigger_nested(api_url: &str) -> eyre::Result<()> {
    use bulk_client::msgs::conditional::Trigger;
    use bulk_client::msgs::order::{BuilderCode, MarketOrder};
    use bulk_client::transaction::{Action, ActionMeta};
    use std::sync::Arc;

    let keys = load_or_create_keys()?;
    let c = client(api_url, &keys.master)?;
    let master_pk = TransactionSigner::from_private_key(&keys.master)?.public_key();
    let builder_pk = TransactionSigner::from_private_key(&keys.builder)?.public_key();

    let px = c.get_ticker("BTC-USD").await?.mark_price;
    println!("BTC mark: {px}");

    let before = c.get_account(builder_pk).await?.margin.total_balance;
    println!("builder bakiyesi (önce): {before}");

    let meta = |acct| ActionMeta {
        account: acct,
        nonce: 0,
        seqno: 0,
        hash: None,
    };

    // Gömülü emir: builderCode taşıyor
    let nested = Action::MarketOrder(MarketOrder {
        symbol: Arc::from("BTC-USD"),
        is_buy: true,
        size: 0.001,
        reduce_only: false,
        iso: false,
        builder_code: Some(BuilderCode {
            to: builder_pk,
            fee: 2,
        }),
        meta: meta(master_pk),
    });

    println!("\n=== 1. UZAK trigger (fiyatın çok altı) — kabul ediliyor mu, bekliyor mu? ===");
    let far = Action::Trigger(Trigger {
        symbol: Arc::from("BTC-USD"),
        is_above: false,
        threshold: px * 0.5,
        actions: vec![nested.clone()],
        meta: meta(master_pk),
    });
    match c.place_tx(vec![far], Some(master_pk), None).await {
        Ok(rs) => {
            for r in &rs {
                let reason = r.raw.get("reason").and_then(|v| v.as_str()).unwrap_or("-");
                println!("   status={} reason={reason} raw={}", r.status, r.raw);
            }
        }
        Err(e) => println!("   ❌ {e}"),
    }

    println!("\n=== 2. ANINDA tetiklenen trigger (eşik fiyatın üstünde, is_above=false) ===");
    println!("(fiyat zaten eşiğin altında → hemen ateşlemeli, gömülü emir dolmalı, fee kesilmeli)");
    let now = Action::Trigger(Trigger {
        symbol: Arc::from("BTC-USD"),
        is_above: false,
        threshold: px * 1.05,
        actions: vec![nested],
        meta: meta(master_pk),
    });
    match c.place_tx(vec![now], Some(master_pk), None).await {
        Ok(rs) => {
            for r in &rs {
                let reason = r.raw.get("reason").and_then(|v| v.as_str()).unwrap_or("-");
                println!("   status={} reason={reason} raw={}", r.status, r.raw);
            }
        }
        Err(e) => println!("   ❌ {e}"),
    }

    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    println!("\n=== 3. Fee gömülü emirden kesildi mi? ===");
    let after = c.get_account(builder_pk).await?.margin.total_balance;
    println!("   builder bakiyesi (sonra): {after}");
    println!("   fark: {}", after - before);
    let expected = px * 0.001 * 0.0002;
    println!("   beklenen (0.001 BTC × {px} × 2bps): ~{expected:.6}");

    println!("\n=== 4. Hesapta açık conditional var mı? ===");
    let a = c.get_account(master_pk).await?;
    println!("   open_orders: {}", a.open_orders.len());
    for o in a.open_orders.iter().take(5) {
        println!("   {o:?}");
    }

    Ok(())
}

/// S5 — Hipotez: bulk-client'ın Trigger'ında `iso` alanı eksik olduğu için imza tutmuyor.
/// Aynı basket'i bulk-keychain (resmi imzalayıcı) ile imzalayıp gönderiyoruz.
/// Geçerse hipotez doğrulanır: bulk-client v0.1.2'de bug var, keychain kanonik.
async fn cmd_trigger_keychain(api_url: &str) -> eyre::Result<()> {
    use bulk_keychain::{
        Commission, Keypair as KcKeypair, Order, OrderItem, OrderType, Signer as KcSigner,
        TriggerBasket,
    };

    let keys = load_or_create_keys()?;
    let c = client(api_url, &keys.master)?;
    let builder_pk_str = pubkey_of(&keys.builder)?;
    let builder_pk = TransactionSigner::from_private_key(&keys.builder)?.public_key();

    let px = c.get_ticker("BTC-USD").await?.mark_price;
    let before = c.get_account(builder_pk).await?.margin.total_balance;
    println!("BTC mark: {px} | builder bakiyesi (önce): {before}");

    let kp =
        KcKeypair::from_base58(&keys.master).map_err(|e| eyre::eyre!("keychain keypair: {e:?}"))?;
    let mut signer = KcSigner::new(kp);

    // Gömülü market emri — builderCode taşıyor
    let nested = OrderItem::Order(Order {
        symbol: "BTC-USD".into(),
        is_buy: true,
        price: 0.0,
        size: 0.001,
        reduce_only: false,
        iso: false,
        order_type: OrderType::market(),
        client_id: None,
        commission: Some(
            Commission::new(
                bulk_keychain::Pubkey::from_base58(&builder_pk_str)
                    .map_err(|e| eyre::eyre!("pubkey: {e:?}"))?,
                2,
            )
            .map_err(|e| eyre::eyre!("commission: {e:?}"))?,
        ),
    });

    // Anında tetiklenecek basket: fiyat zaten eşiğin altında
    let basket = OrderItem::TriggerBasket(TriggerBasket {
        symbol: "BTC-USD".into(),
        is_buy: false, // wire'da "d" — eşiğin altı/üstü
        trigger_price: px * 1.05,
        actions: vec![nested],
        iso: false,
    });

    let signed = signer
        .sign(basket, None)
        .map_err(|e| eyre::eyre!("imzalama: {e:?}"))?;

    println!("\n=== Keychain'in ürettiği payload ===");
    println!("{}", serde_json::to_string_pretty(&signed.actions)?);

    println!("\n=== Gönderiliyor ===");
    let body = serde_json::json!({
        "actions": signed.actions,
        "nonce": signed.nonce,
        "account": signed.account,
        "signer": signed.signer,
        "signature": signed.signature,
    });
    let resp = reqwest::Client::new()
        .post(format!("{api_url}/order"))
        .json(&body)
        .send()
        .await?;
    println!("HTTP {}", resp.status());
    println!("{}", resp.text().await?);

    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let after = c.get_account(builder_pk).await?.margin.total_balance;
    println!(
        "\nbuilder bakiyesi (sonra): {after} | fark: {}",
        after - before
    );
    println!("beklenen: ~{:.6}", px * 0.001 * 0.0002);

    Ok(())
}

/// S3 — Agent wallet ne yapabiliyor, ne yapamıyor?
/// Agent SUB-ACCOUNT'a kaydediliyor (PLAN.md §7: master'a asla).
async fn cmd_agent_transfer(api_url: &str) -> eyre::Result<()> {
    use bulk_client::msgs::subaccounts::{Transfer, TransferKind};
    use bulk_client::transaction::{Action, ActionMeta};

    let keys = load_or_create_keys()?;
    let master_c = client(api_url, &keys.master)?;
    let agent_c = client(api_url, &keys.agent)?;
    let master_pk = TransactionSigner::from_private_key(&keys.master)?.public_key();
    let agent_pk = TransactionSigner::from_private_key(&keys.agent)?.public_key();
    let builder_pk = TransactionSigner::from_private_key(&keys.builder)?.public_key();
    let sub_pk = solana_pubkey::Pubkey::from_str(
        keys.sub
            .as_deref()
            .ok_or_else(|| eyre::eyre!("önce sub-account-builder çalıştır"))?,
    )?;

    println!("master: {master_pk}\nagent : {agent_pk}\nsub   : {sub_pk}");

    // ⚠️ manage_agent_wallet(account: Some(x)) KAPSAMLAMA YAPMIYOR:
    // `account` sadece meta'ya yazılıyor, meta ise #[serde(skip)] — hiç serileştirilmiyor.
    // İçeride place_tx(.., None, ..) çağrıldığı için tx.account = signer = master oluyor.
    // Doğru kapsamlama için place_tx'e account'ı ELDEN geçirmek zorundayız.
    println!("\n=== 0. Master'a yanlışlıkla kaydolmuş agent'ı SİL ===");
    match master_c
        .manage_agent_wallet(agent_pk, true, None, None)
        .await
    {
        Ok(r) => println!("   status={} raw={}", r.status, r.raw),
        Err(e) => println!("   ❌ {e}"),
    }

    println!("\n=== 1. Agent'ı SADECE sub-account'a kaydet (tx.account = sub) ===");
    let reg = Action::AgentWalletCreation(bulk_client::msgs::AgentWalletCreation {
        agent: agent_pk,
        delete: false,
        meta: ActionMeta {
            account: sub_pk,
            nonce: 0,
            seqno: 0,
            hash: None,
        },
    });
    match master_c.place_tx(vec![reg], Some(sub_pk), None).await {
        Ok(rs) => {
            for r in &rs {
                println!("   status={} raw={}", r.status, r.raw);
            }
        }
        Err(e) => println!("   ❌ {e}"),
    }

    println!("\n=== 2. Agent, sub-account adına builderCode'lu emir atabiliyor mu? ===");
    println!("(çalışmalı — Sınıf 2 watcher'ın tüm işi bu)");
    match place_market_with_builder(&agent_c, sub_pk, builder_pk, 2).await {
        Ok(rs) => {
            for r in &rs {
                let reason = r.raw.get("reason").and_then(|v| v.as_str()).unwrap_or("-");
                println!("   status={} reason={reason}", r.status);
            }
        }
        Err(e) => println!("   ❌ {e}"),
    }

    println!("\n=== 3. 🔒 Agent, sub'dan MASTER'a para taşıyabiliyor mu? (internal) ===");
    println!("(REDDEDİLMELİ — geçerse agent key'i sızınca para taşınabilir demektir)");
    let t = Action::Transfer(Transfer {
        kind: TransferKind::Internal,
        from: sub_pk,
        to: master_pk,
        margin_amount: 10.0,
        meta: ActionMeta {
            account: sub_pk,
            nonce: 0,
            seqno: 0,
            hash: None,
        },
    });
    match agent_c.place_tx(vec![t], Some(sub_pk), None).await {
        Ok(rs) => {
            for r in &rs {
                println!("   status={} raw={}", r.status, r.raw);
            }
        }
        Err(e) => println!("   ❌ {e}"),
    }

    println!("\n=== 4. 🔒 Agent, sub'dan DIŞARI para çıkarabiliyor mu? (external) ===");
    println!("(REDDEDİLMELİ — asıl tehlike bu)");
    let hedef = TransactionSigner::from_private_key(&keys.builder)?.public_key();
    let t = Action::Transfer(Transfer {
        kind: TransferKind::External,
        from: sub_pk,
        to: hedef,
        margin_amount: 10.0,
        meta: ActionMeta {
            account: sub_pk,
            nonce: 0,
            seqno: 0,
            hash: None,
        },
    });
    match agent_c.place_tx(vec![t], Some(sub_pk), None).await {
        Ok(rs) => {
            for r in &rs {
                println!("   status={} raw={}", r.status, r.raw);
            }
        }
        Err(e) => println!("   ❌ {e}"),
    }

    println!("\n=== 5. Agent, MASTER adına emir atabiliyor mu? ===");
    println!("(REDDEDİLMELİ — agent sadece sub'a kayıtlı, master'a değil)");
    match place_market_with_builder(&agent_c, master_pk, builder_pk, 2).await {
        Ok(rs) => {
            for r in &rs {
                let reason = r.raw.get("reason").and_then(|v| v.as_str()).unwrap_or("-");
                println!("   status={} reason={reason}", r.status);
            }
        }
        Err(e) => println!("   ❌ {e}"),
    }

    println!("\n=== Bakiyeler ===");
    println!(
        "master: {}",
        master_c.get_account(master_pk).await?.margin.total_balance
    );
    println!(
        "sub   : {}",
        master_c.get_account(sub_pk).await?.margin.total_balance
    );

    Ok(())
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> eyre::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Setup => cmd_setup(&cli.api_url).await,
        Command::Status => cmd_status(&cli.api_url).await,
        Command::BuilderBasic => cmd_builder_basic(&cli.api_url).await,
        Command::SubAccountBuilder => cmd_subaccount_builder(&cli.api_url).await,
        Command::TriggerNested => cmd_trigger_nested(&cli.api_url).await,
        Command::TriggerKeychain => cmd_trigger_keychain(&cli.api_url).await,
        Command::AgentTransfer => cmd_agent_transfer(&cli.api_url).await,
        Command::NonceAge => cmd_nonce_age(&cli.api_url).await,
        Command::OnfillBracket => cmd_onfill_bracket(&cli.api_url).await,
        Command::BracketVariants => cmd_bracket_variants(&cli.api_url).await,
        Command::OnfillRealParent => cmd_onfill_real_parent(&cli.api_url).await,
        Command::OidPredict => cmd_oid_predict(&cli.api_url).await,
        Command::ExitLadder => cmd_exit_ladder(&cli.api_url).await,
        Command::TrigLadder => cmd_trig_ladder(&cli.api_url).await,
        Command::NonceReplay => cmd_nonce_replay(&cli.api_url).await,
        Command::SignModes => cmd_sign_modes(&cli.api_url).await,
        Command::SignDiag => cmd_sign_diag(&cli.api_url).await,
        Command::SignOffchain => cmd_sign_offchain(&cli.api_url).await,
    }
}

/// S15 — `x-bulk-sig-mode` header'ı staging'de GERÇEKTEN uygulanıyor mu?
///
/// S14'te raw ✅, base58/base64 ❌ çıktı. İki açıklama var: (a) header yok
/// sayılıyor, server hep raw doğruluyor; (b) base58 modu farklı çalışıyor
/// (mesajı body'de ayrı alanda bekliyor vб.). Ayırıcı testler:
///   T1: raw içerik, header YOK            → temel (kabul beklenir)
///   T2: raw içerik, header 'base58'       → KABUL ⇒ header yok sayılıyor
///   T3: base58 içerik, header 'base58', body'de message=base58(bytes)
///   T4: base58 içerik, header 'base58', body'de msg=base58(bytes)
async fn cmd_sign_diag(api_url: &str) -> eyre::Result<()> {
    use bulk_keychain::{
        prepare_group, CancelAll, Keypair as KcKeypair, Order, OrderItem, OrderType,
        PreparedMessage, Signer as KcSigner, TimeInForce,
    };
    use solana_signer::Signer as _;

    let keys = load_or_create_keys()?;
    let c = client(api_url, &keys.master)?;
    let px = c.get_ticker("BTC-USD").await?.mark_price;
    let kc = KcKeypair::from_base58(&keys.master).map_err(|e| eyre::eyre!("{e:?}"))?;
    let acct = kc.pubkey();
    let sk = solana_keypair::Keypair::from_base58_string(&keys.master);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;
    let limit_px = (px * 0.80 * 100.0).round() / 100.0;
    println!("BTC mark: {px} | hesap: {}", acct.to_base58());

    let make = |nonce: u64| -> eyre::Result<PreparedMessage> {
        let order = OrderItem::Order(Order {
            symbol: "BTC-USD".into(),
            is_buy: true,
            price: limit_px,
            size: 0.001,
            reduce_only: false,
            iso: false,
            order_type: OrderType::Limit {
                tif: TimeInForce::Gtc,
            },
            client_id: None,
            commission: None,
        });
        prepare_group(vec![order], &acct, Some(&acct), Some(nonce))
            .map_err(|e| eyre::eyre!("prepare: {e:?}"))
    };

    // (etiket, header?, imzalanacak-içerik "raw"|"base58", body'ye eklenecek ekstra alanlar)
    let cases: [(&str, Option<&str>, &str, &[(&str, bool)]); 4] = [
        ("T1 raw içerik / header YOK", None, "raw", &[]),
        ("T2 raw içerik / header base58", Some("base58"), "raw", &[]),
        (
            "T3 base58 içerik / header base58 / body.message=base58",
            Some("base58"),
            "base58",
            &[("message", true)],
        ),
        (
            "T4 base58 içerik / header base58 / body.msg=base58",
            Some("base58"),
            "base58",
            &[("msg", true)],
        ),
    ];

    for (i, (label, header, content_kind, extra)) in cases.iter().enumerate() {
        let nonce = now_ms + i as u64;
        let p = make(nonce)?;
        let content: Vec<u8> = match *content_kind {
            "raw" => p.message_bytes.clone(),
            "base58" => p.message_base58().into_bytes(),
            _ => unreachable!(),
        };
        let signature = sk.sign_message(&content).to_string();
        let mut body = serde_json::json!({
            "actions": p.actions, "nonce": p.nonce,
            "account": p.account, "signer": p.signer, "signature": signature,
        });
        for (field, _) in extra.iter() {
            body[*field] = serde_json::Value::String(p.message_base58());
        }
        let mut req = reqwest::Client::new().post(format!("{api_url}/order"));
        if let Some(h) = header {
            req = req.header("x-bulk-sig-mode", *h);
        }
        let resp = req.json(&body).send().await?;
        let st = resp.status();
        let txt = resp.text().await?;
        let bad = txt.to_lowercase().contains("bad signature");
        let kisa: String = txt.chars().take(200).collect();
        println!("\n=== {label} ===");
        println!("HTTP {st} | {kisa}");
        println!(
            ">>> {}",
            if bad {
                "❌ bad signature"
            } else {
                "✅ imza KABUL"
            }
        );
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    println!("\n=== yorum ===");
    println!("T2 ✅ ise: header YOK SAYILIYOR (staging hep raw doğruluyor).");
    println!("T2 ❌ ama T3/T4 ✅ ise: base58 modu mesajı body'de bekliyor.");
    println!("Hepsi (T2,T3,T4) ❌ ise: base58 modu staging'de yok / farklı.");

    println!("\n=== temizlik (cxa) ===");
    let mut signer = KcSigner::new(kc);
    let _ = gonder_items(
        api_url,
        &mut signer,
        vec![OrderItem::CancelAll(CancelAll::all())],
    )
    .await?;
    Ok(())
}

/// S16 — Offchain zarf modu staging'de artık aktif mi? (ekip "resolved" dedi)
///
/// Phantom `signMessage`'ın ham baytları reddetme sorununun çözümü offchain
/// zarf (`0xff "solana offchain"` domain'li). Burada iki şeyi ayırıyoruz:
///   A: raw baytları imzala + header 'offchain' → KABUL ise header hâlâ no-op;
///      "bad signature" ise offchain mod AKTİF (artık zarf bekliyor).
///   B*: offchain zarfı farklı action-line formatlarıyla imzala + header
///      'offchain' → kabul edeni bulursak action-line formatı ampirik oturur.
async fn cmd_sign_offchain(api_url: &str) -> eyre::Result<()> {
    use bulk_keychain::{
        prepare_group, CancelAll, Keypair as KcKeypair, Order, OrderItem, OrderType,
        PreparedMessage, Signer as KcSigner, TimeInForce,
    };
    use solana_signer::Signer as _;

    let keys = load_or_create_keys()?;
    let c = client(api_url, &keys.master)?;
    let px = c.get_ticker("BTC-USD").await?.mark_price;
    let kc = KcKeypair::from_base58(&keys.master).map_err(|e| eyre::eyre!("{e:?}"))?;
    let acct = kc.pubkey();
    let acct_b58 = acct.to_base58();
    let sk = solana_keypair::Keypair::from_base58_string(&keys.master);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;
    let limit_px = (px * 0.80 * 100.0).round() / 100.0;
    println!("BTC mark: {px} | hesap/signer: {acct_b58}");

    let make = |nonce: u64| -> eyre::Result<PreparedMessage> {
        let order = OrderItem::Order(Order {
            symbol: "BTC-USD".into(),
            is_buy: true,
            price: limit_px,
            size: 0.001,
            reduce_only: false,
            iso: false,
            order_type: OrderType::Limit {
                tif: TimeInForce::Gtc,
            },
            client_id: None,
            commission: None,
        });
        prepare_group(vec![order], &acct, Some(&acct), Some(nonce))
            .map_err(|e| eyre::eyre!("prepare: {e:?}"))
    };

    let post = |sig: String, p: &PreparedMessage| {
        let body = serde_json::json!({
            "actions": p.actions, "nonce": p.nonce,
            "account": p.account, "signer": p.signer, "signature": sig,
        });
        let url = format!("{api_url}/order");
        async move {
            let resp = reqwest::Client::new()
                .post(url)
                .header("x-bulk-sig-mode", "offchain")
                .json(&body)
                .send()
                .await?;
            let st = resp.status();
            let txt = resp.text().await?;
            eyre::Ok((st, txt))
        }
    };

    // --- A: raw baytları + header offchain (mod aktif mi ayırıcı) ---
    {
        let p = make(now_ms)?;
        let sig = sk.sign_message(&p.message_bytes).to_string();
        let (st, txt) = post(sig, &p).await?;
        let bad = txt.to_lowercase().contains("bad signature");
        println!("\n=== A: raw baytlar + header offchain ===");
        println!("HTTP {st} | {}", txt.chars().take(220).collect::<String>());
        println!(
            ">>> {}",
            if bad {
                "❌ bad signature ⇒ OFFCHAIN MOD AKTİF (artık zarf bekliyor)"
            } else {
                "✅ kabul ⇒ offchain header HÂLÂ NO-OP (server raw doğruluyor)"
            }
        );
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    // --- B*: offchain zarf, farklı action-line formatları ---
    // Format bilinmiyor; birkaç makul aday deniyoruz. Kabul eden = doğru format.
    let p0 = make(now_ms + 100)?;
    let compact: Vec<String> = p0
        .actions
        .iter()
        .map(|a| serde_json::to_string(a).unwrap_or_default())
        .collect();
    let variants: Vec<(&str, Vec<String>)> = vec![
        ("bos (yalniz baslik)", vec![]),
        ("compact-json/action", compact.clone()),
        (
            "human: 'Limit Buy 0.001 BTC-USD'",
            vec![format!("Limit Buy 0.001 BTC-USD @ {limit_px}")],
        ),
    ];

    for (i, (label, lines)) in variants.into_iter().enumerate() {
        let nonce = now_ms + 200 + i as u64;
        let p = make(nonce)?;
        let env = pusu_sign::offchain::build_envelope(
            &p.message_bytes,
            &acct_b58,
            p.nonce,
            &acct_b58,
            &lines,
        )
        .map_err(|e| eyre::eyre!("zarf: {e:?}"))?;
        let sig = sk.sign_message(&env).to_string();
        let (st, txt) = post(sig, &p).await?;
        let bad = txt.to_lowercase().contains("bad signature");
        println!("\n=== B{i}: offchain zarf / action-line = {label} ===");
        println!("HTTP {st} | {}", txt.chars().take(220).collect::<String>());
        println!(
            ">>> {}",
            if bad {
                "❌ bad signature"
            } else {
                "✅ imza KABUL 🎉 (bu format tutuyor)"
            }
        );
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    println!("\n=== yorum ===");
    println!("A ❌ (bad sig): offchain mod açık — B*'lerden ✅ olan action-line formatıdır.");
    println!(
        "A ✅ (kabul):   offchain header hâlâ yok sayılıyor — ekip staging'i açmamış olabilir."
    );

    println!("\n=== temizlik (cxa) ===");
    let mut signer = KcSigner::new(kc);
    let _ = gonder_items(
        api_url,
        &mut signer,
        vec![OrderItem::CancelAll(CancelAll::all())],
    )
    .await;
    Ok(())
}

/// S14 — Server hangi imza modunu kabul ediyor?
///
/// Canlı testte Phantom, BULK'un ham `message_bytes`'ını `signMessage` ile
/// imzalamayı reddetti: "You cannot sign solana transactions using sign
/// message" (baytlar transaction-şekilli → anti-phishing guardrail). BULK docs
/// `x-bulk-sig-mode` header'ıyla üç mod sunuyor: `raw | offchain | base58`.
/// `base58`/`base64` modu ham baytların METİN kodlamasını imzalatıyor — ASCII
/// string transaction'a benzemediği için guardrail'ı geçer ve kanonik olduğu
/// için server deterministik doğrular (offchain zarfındaki action-line belirsizliği yok).
///
/// Bu probe cüzdanı TAKLİT ediyor: `bulk-keychain`'in `KcSigner`'ı ham baytları
/// imzalıyor (hep çalışan raw yol); burada bunun yerine cüzdanın imzalayacağı
/// baytları (message_bytes / base58 / base64) ham ed25519 anahtarıyla imzalayıp
/// ilgili header'la POST ediyoruz. Sunucunun cevabında "bad signature" varsa
/// imza TUTMADI; resting/filled/rejectedInvalid gibi her şey imzanın TUTTUĞU
/// anlamına gelir (emrin başka sebeple reddi imzayı ilgilendirmez).
///
/// Cüzdan yok — saf backend ölçümü. Sonuç: hangi mod tutuyorsa `pusu-sign` +
/// `wallet.rs` onu kullanacak.
async fn cmd_sign_modes(api_url: &str) -> eyre::Result<()> {
    use bulk_keychain::{
        prepare_group, CancelAll, Keypair as KcKeypair, Order, OrderItem, OrderType,
        PreparedMessage, Signer as KcSigner, TimeInForce,
    };
    use solana_signer::Signer as _;

    let keys = load_or_create_keys()?;
    let c = client(api_url, &keys.master)?;
    let px = c.get_ticker("BTC-USD").await?.mark_price;

    // Aynı secret'ten iki görünüm: kc → prepare (bulk_keychain::Pubkey),
    // sk → ham bayt imzalama (cüzdanın yaptığı iş).
    let kc = KcKeypair::from_base58(&keys.master).map_err(|e| eyre::eyre!("{e:?}"))?;
    let acct = kc.pubkey();
    let sk = solana_keypair::Keypair::from_base58_string(&keys.master);
    println!(
        "BTC mark: {px} | hesap: {} | signer(solana): {}",
        acct.to_base58(),
        sk.pubkey()
    );

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;
    // Dolmayacak alış limiti (mark'ın %20 altı) — book'ta bekler, imza doğrulaması kesin çalışır.
    let limit_px = (px * 0.80 * 100.0).round() / 100.0;

    let make = |nonce: u64| -> eyre::Result<PreparedMessage> {
        let order = OrderItem::Order(Order {
            symbol: "BTC-USD".into(),
            is_buy: true,
            price: limit_px,
            size: 0.001,
            reduce_only: false,
            iso: false,
            order_type: OrderType::Limit {
                tif: TimeInForce::Gtc,
            },
            client_id: None,
            commission: None,
        });
        prepare_group(vec![order], &acct, Some(&acct), Some(nonce))
            .map_err(|e| eyre::eyre!("prepare: {e:?}"))
    };

    // (mod, açıklama) — her biri cüzdana verilecek farklı bayt.
    let modes = [
        ("raw", "message_bytes (KONTROL — cüzdan bunu reddediyor)"),
        ("base58", "utf8(base58(message_bytes))"),
        ("base64", "utf8(base64(message_bytes))"),
    ];

    for (i, (mode, desc)) in modes.iter().enumerate() {
        let nonce = now_ms + i as u64; // her mod taze nonce (dedup nonce'a bakıyor, §8.11)
        let p = make(nonce)?;
        let content: Vec<u8> = match *mode {
            "raw" => p.message_bytes.clone(),
            "base58" => p.message_base58().into_bytes(),
            "base64" => p.message_base64().into_bytes(),
            _ => unreachable!(),
        };
        let signature = sk.sign_message(&content).to_string();
        let body = serde_json::json!({
            "actions": p.actions, "nonce": p.nonce,
            "account": p.account, "signer": p.signer, "signature": signature,
        });
        let resp = reqwest::Client::new()
            .post(format!("{api_url}/order"))
            .header("x-bulk-sig-mode", *mode)
            .json(&body)
            .send()
            .await?;
        let st = resp.status();
        let txt = resp.text().await?;
        let bad = txt.to_lowercase().contains("bad signature");
        let kisa: String = txt.chars().take(220).collect();
        println!("\n=== mode='{mode}'  ({desc}) ===");
        println!("HTTP {st} | {kisa}");
        println!(
            ">>> imza: {}",
            if bad {
                "❌ REDDEDİLDİ (bad signature)"
            } else {
                "✅ KABUL — server imzayı doğruladı, bu mod cüzdanla kullanılabilir"
            }
        );
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    // Temizlik: modlar book'a resting limit bıraktıysa sil (raw KcSigner yolu hep çalışıyor).
    println!("\n=== temizlik (cxa) ===");
    let mut signer = KcSigner::new(kc);
    let _ = gonder_items(
        api_url,
        &mut signer,
        vec![OrderItem::CancelAll(CancelAll::all())],
    )
    .await?;
    println!("resting limitler iptal edildi");

    Ok(())
}

/// S6 — Nonce'un ömrü var mı? Ön-imzalı tx tasarımı buna bağlı.
async fn cmd_nonce_age(api_url: &str) -> eyre::Result<()> {
    use bulk_keychain::{Keypair as KcKeypair, Order, OrderItem, OrderType, Signer as KcSigner};

    let keys = load_or_create_keys()?;
    let kp = KcKeypair::from_base58(&keys.master).map_err(|e| eyre::eyre!("{e:?}"))?;
    let mut signer = KcSigner::new(kp);

    let now_ns = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64)
        * 1_000_000;

    // 30 gün önceki nonce
    let old_ns = now_ns - 30u64 * 24 * 60 * 60 * 1000 * 1_000_000;
    // 30 gün sonraki nonce
    let future_ns = now_ns + 30u64 * 24 * 60 * 60 * 1000 * 1_000_000;

    for (label, nonce) in [
        ("30 GÜN ESKİ", old_ns),
        ("30 GÜN İLERİ", future_ns),
        ("ŞİMDİ", now_ns),
    ] {
        let item = OrderItem::Order(Order {
            symbol: "BTC-USD".into(),
            is_buy: true,
            price: 0.0,
            size: 0.001,
            reduce_only: false,
            iso: false,
            order_type: OrderType::market(),
            client_id: None,
            commission: None,
        });
        let signed = signer
            .sign(item, Some(nonce))
            .map_err(|e| eyre::eyre!("{e:?}"))?;
        let body = serde_json::json!({
            "actions": signed.actions, "nonce": signed.nonce,
            "account": signed.account, "signer": signed.signer, "signature": signed.signature,
        });
        let resp = reqwest::Client::new()
            .post(format!("{api_url}/order"))
            .json(&body)
            .send()
            .await?;
        let txt = resp.text().await?;
        println!(
            "{label:<14} nonce={nonce} → {}",
            txt.chars().take(160).collect::<String>()
        );
    }
    Ok(())
}

/// S7 — `of` bracket'i, trigger'ın İÇİNDEKİ emir dolunca ateşliyor mu?
///
/// Compiler'ın tamamı buna dayanacak: `of` trigger'ın içine gömülemiyor
/// (keychain: "TriggerBasket nested actions: m, l, mod, cx, cxa, st, tp, rng"
/// — `of` yok), o yüzden kardeş olarak [trig, of{p:0}] kurmak zorundayız.
/// Soru: `of`'un parent'ı trig iken, trig'in içindeki market emri dolunca
/// bracket kuruluyor mu?
async fn cmd_onfill_bracket(api_url: &str) -> eyre::Result<()> {
    use bulk_keychain::{
        Commission, Keypair as KcKeypair, OnFill, Order, OrderItem, OrderType, RangeOco,
        Signer as KcSigner, TriggerBasket,
    };

    let keys = load_or_create_keys()?;
    let c = client(api_url, &keys.master)?;
    let master_pk = TransactionSigner::from_private_key(&keys.master)?.public_key();
    let builder_pk_str = pubkey_of(&keys.builder)?;

    let px = c.get_ticker("BTC-USD").await?.mark_price;
    println!("BTC mark: {px}");
    let acik_once = c.get_account(master_pk).await?.open_orders.len();
    println!("acik emir (once): {acik_once}");

    let kp = KcKeypair::from_base58(&keys.master).map_err(|e| eyre::eyre!("{e:?}"))?;
    let mut signer = KcSigner::new(kp);

    let giris = OrderItem::Order(Order {
        symbol: "BTC-USD".into(),
        is_buy: true,
        price: 0.0,
        size: 0.002,
        reduce_only: false,
        iso: false,
        order_type: OrderType::market(),
        client_id: None,
        commission: Some(
            Commission::new(
                bulk_keychain::Pubkey::from_base58(&builder_pk_str)
                    .map_err(|e| eyre::eyre!("{e:?}"))?,
                2,
            )
            .map_err(|e| eyre::eyre!("{e:?}"))?,
        ),
    });

    // Anında tetiklenecek basket (fiyat zaten eşiğin altında)
    let basket = OrderItem::TriggerBasket(TriggerBasket {
        symbol: "BTC-USD".into(),
        is_buy: false,
        trigger_price: px * 1.05,
        actions: vec![giris],
        iso: false,
    });

    // Bracket: parent = index 0 (trig). Long icin stop altta, hedef ustte.
    let bracket = OrderItem::OnFill(OnFill {
        p: 0,
        actions: vec![OrderItem::RangeOco(RangeOco {
            symbol: "BTC-USD".into(),
            is_buy: true,
            size: 0.002,
            collar_min: px * 0.90,
            collar_max: px * 1.10,
            limit_min: f64::NAN,
            limit_max: f64::NAN,
            iso: false,
        })],
    });

    println!("\n=== [trig, of{{p:0}}] gonderiliyor ===");
    let signed = signer
        .sign_group(vec![basket, bracket], None)
        .map_err(|e| eyre::eyre!("imzalama: {e:?}"))?;
    println!(
        "payload:\n{}",
        serde_json::to_string_pretty(&signed.actions)?
    );

    let body = serde_json::json!({
        "actions": signed.actions, "nonce": signed.nonce,
        "account": signed.account, "signer": signed.signer, "signature": signed.signature,
    });
    let resp = reqwest::Client::new()
        .post(format!("{api_url}/order"))
        .json(&body)
        .send()
        .await?;
    println!("\nHTTP {}", resp.status());
    println!("{}", resp.text().await?);

    tokio::time::sleep(std::time::Duration::from_secs(4)).await;

    println!("\n=== Sonuc ===");
    let a = c.get_account(master_pk).await?;
    println!(
        "acik emir (sonra): {} (once {acik_once})",
        a.open_orders.len()
    );
    for o in a.open_orders.iter().take(8) {
        println!("   {o:?}");
    }
    println!("pozisyonlar:");
    for p in a.positions.iter() {
        println!("   {p:?}");
    }
    Ok(())
}

/// S8 — Bracket'i trigger'a nasıl bağlarız? İki aday:
///   A) trig { actions: [m, of{p:0, actions:[rng]}] }  — of gömülü (keychain izin vermiyor der)
///   B) trig { actions: [m, rng] }                     — rng doğrudan gömülü
async fn cmd_bracket_variants(api_url: &str) -> eyre::Result<()> {
    use bulk_keychain::{
        Commission, Keypair as KcKeypair, OnFill, Order, OrderItem, OrderType, RangeOco,
        Signer as KcSigner, TriggerBasket,
    };

    let keys = load_or_create_keys()?;
    let c = client(api_url, &keys.master)?;
    let builder_pk_str = pubkey_of(&keys.builder)?;
    let px = c.get_ticker("BTC-USD").await?.mark_price;

    let giris = |sz: f64| -> eyre::Result<OrderItem> {
        Ok(OrderItem::Order(Order {
            symbol: "BTC-USD".into(),
            is_buy: true,
            price: 0.0,
            size: sz,
            reduce_only: false,
            iso: false,
            order_type: OrderType::market(),
            client_id: None,
            commission: Some(
                Commission::new(
                    bulk_keychain::Pubkey::from_base58(&builder_pk_str)
                        .map_err(|e| eyre::eyre!("{e:?}"))?,
                    2,
                )
                .map_err(|e| eyre::eyre!("{e:?}"))?,
            ),
        }))
    };
    let rng = |sz: f64| {
        OrderItem::RangeOco(RangeOco {
            symbol: "BTC-USD".into(),
            is_buy: true,
            size: sz,
            collar_min: px * 0.90,
            collar_max: px * 1.10,
            limit_min: f64::NAN,
            limit_max: f64::NAN,
            iso: false,
        })
    };

    let kp = KcKeypair::from_base58(&keys.master).map_err(|e| eyre::eyre!("{e:?}"))?;
    let mut signer = KcSigner::new(kp);

    let mut gonder = |ad: &str, item: OrderItem| -> eyre::Result<()> {
        let signed = signer
            .sign(item, None)
            .map_err(|e| eyre::eyre!("imza: {e:?}"))?;
        let body = serde_json::json!({
            "actions": signed.actions, "nonce": signed.nonce,
            "account": signed.account, "signer": signed.signer, "signature": signed.signature,
        });
        let rt = tokio::runtime::Handle::current();
        let txt = tokio::task::block_in_place(|| {
            rt.block_on(async {
                reqwest::Client::new()
                    .post(format!("{api_url}/order"))
                    .json(&body)
                    .send()
                    .await?
                    .text()
                    .await
            })
        })?;
        println!("\n=== {ad} ===");
        println!("{txt}");
        Ok(())
    };

    // A) of, trigger'ın İÇİNE gömülü
    gonder(
        "A) trig { actions: [m, of{p:0,[rng]}] }",
        OrderItem::TriggerBasket(TriggerBasket {
            symbol: "BTC-USD".into(),
            is_buy: false,
            trigger_price: px * 1.05,
            actions: vec![
                giris(0.001)?,
                OrderItem::OnFill(OnFill {
                    p: 0,
                    actions: vec![rng(0.001)],
                }),
            ],
            iso: false,
        }),
    )?;

    // B) rng doğrudan trigger'ın içinde, market emrin kardeşi
    gonder(
        "B) trig { actions: [m, rng] }",
        OrderItem::TriggerBasket(TriggerBasket {
            symbol: "BTC-USD".into(),
            is_buy: false,
            trigger_price: px * 1.05,
            actions: vec![giris(0.001)?, rng(0.001)],
            iso: false,
        }),
    )?;

    Ok(())
}

/// S9 — `of`'un parent'ı GERÇEK bir emir olunca çalışıyor mu?
/// Watched sınıfında düz market emri gönderdiğimiz için `of` kullanabiliriz.
/// Çalışırsa `[m, rng]`'den güvenli: market reddedilirse rng hiç kurulmaz.
async fn cmd_onfill_real_parent(api_url: &str) -> eyre::Result<()> {
    use bulk_keychain::{
        Commission, Keypair as KcKeypair, OnFill, Order, OrderItem, OrderType, RangeOco,
        Signer as KcSigner,
    };

    let keys = load_or_create_keys()?;
    let c = client(api_url, &keys.master)?;
    let builder_pk_str = pubkey_of(&keys.builder)?;
    let px = c.get_ticker("BTC-USD").await?.mark_price;
    println!("BTC mark: {px}");

    let kp = KcKeypair::from_base58(&keys.master).map_err(|e| eyre::eyre!("{e:?}"))?;
    let mut signer = KcSigner::new(kp);

    let m = OrderItem::Order(Order {
        symbol: "BTC-USD".into(),
        is_buy: true,
        price: 0.0,
        size: 0.001,
        reduce_only: false,
        iso: false,
        order_type: OrderType::market(),
        client_id: None,
        commission: Some(
            Commission::new(
                bulk_keychain::Pubkey::from_base58(&builder_pk_str)
                    .map_err(|e| eyre::eyre!("{e:?}"))?,
                2,
            )
            .map_err(|e| eyre::eyre!("{e:?}"))?,
        ),
    });
    let of = OrderItem::OnFill(OnFill {
        p: 0,
        actions: vec![OrderItem::RangeOco(RangeOco {
            symbol: "BTC-USD".into(),
            is_buy: true,
            size: 0.001,
            collar_min: px * 0.90,
            collar_max: px * 1.10,
            limit_min: f64::NAN,
            limit_max: f64::NAN,
            iso: false,
        })],
    });

    println!("\n=== [m, of{{p:0,[rng]}}] gonderiliyor ===");
    let signed = signer
        .sign_group(vec![m, of], None)
        .map_err(|e| eyre::eyre!("imza: {e:?}"))?;
    let body = serde_json::json!({
        "actions": signed.actions, "nonce": signed.nonce,
        "account": signed.account, "signer": signed.signer, "signature": signed.signature,
    });
    let txt = reqwest::Client::new()
        .post(format!("{api_url}/order"))
        .json(&body)
        .send()
        .await?
        .text()
        .await?;
    println!("{txt}");
    Ok(())
}

/// S10 — oid gönderim ÖNCESİ hesaplanabiliyor mu?
///
/// Kullanıcının senaryosu buna bağlı: "15m kapanışta limit emri gir; retest
/// gelmez de dolmazsa 15 dk sonra bana sor." Dolmayan emri iptal etmek için
/// `cx` gerekiyor, `cx` de `oid` istiyor. oid'i gönderimden sonra öğrenirsek
/// iptali ön-imzalayamayız — ya sunucuya imza yetkisi vereceğiz (custody,
/// olmaz) ya da `cxa` ile o sembolün TÜM emirlerini sileceğiz (dolmuş
/// pozisyonun bracket'ini de öldürür, olmaz).
///
/// keychain'de `compute_order_id(order, nonce, owner)` var: oid =
/// SHA256(seqno || bincode(action) || account || nonce). Hepsi bizim
/// kontrolümüzde. AMA keychain'in kolaylık fonksiyonları `commission: None`
/// varsayıyor; bizim emirlerimizde builder code var ve o da bincode'a giriyor.
/// Borsanın aynı oid'i ürettiğini VARSAYAMAYIZ — ölçüyoruz.
///
/// 1. Limit emri kur (builder code'lu), imzala, oid'i lokalde hesapla
/// 2. Gönder, borsanın döndürdüğü oid ile karşılaştır
/// 3. Tutuyorsa: o oid ile ön-imzalı `cx` gönder, gerçekten iptal ediyor mu?
async fn cmd_oid_predict(api_url: &str) -> eyre::Result<()> {
    use bulk_keychain::{
        compute_order_id, Cancel, Commission, Keypair as KcKeypair, Order, OrderItem, OrderType,
        Signer as KcSigner, TimeInForce,
    };

    let keys = load_or_create_keys()?;
    let c = client(api_url, &keys.master)?;
    let builder_pk_str = pubkey_of(&keys.builder)?;
    let px = c.get_ticker("BTC-USD").await?.mark_price;

    let kp = KcKeypair::from_base58(&keys.master).map_err(|e| eyre::eyre!("{e:?}"))?;
    let owner = kp.pubkey();
    println!("BTC mark: {px} | hesap: {}", owner.to_base58());

    // Dolmayacak bir limit: mark'ın %20 altında alış. Book'ta bekler.
    let limit_px = (px * 0.80 * 100.0).round() / 100.0;
    let order = Order {
        symbol: "BTC-USD".into(),
        is_buy: true,
        price: limit_px,
        size: 0.001,
        reduce_only: false,
        iso: false,
        order_type: OrderType::Limit {
            tif: TimeInForce::Gtc,
        },
        client_id: None,
        // Ürün kodundaki gibi: builder code iliştirilmiş.
        commission: Some(
            Commission::new(
                bulk_keychain::Pubkey::from_base58(&builder_pk_str)
                    .map_err(|e| eyre::eyre!("{e:?}"))?,
                2,
            )
            .map_err(|e| eyre::eyre!("{e:?}"))?,
        ),
    };

    let mut signer = KcSigner::new(kp);
    let signed = signer
        .sign_group(vec![OrderItem::Order(order.clone())], None)
        .map_err(|e| eyre::eyre!("imza: {e:?}"))?;

    // Kritik adım: oid'i GÖNDERMEDEN hesapla.
    let tahmin = compute_order_id(&order, signed.nonce, &owner);
    println!("\n=== 1. limit {limit_px} @ nonce {} ===", signed.nonce);
    println!("lokalde hesaplanan oid: {}", tahmin.to_base58());

    let body = serde_json::json!({
        "actions": signed.actions, "nonce": signed.nonce,
        "account": signed.account, "signer": signed.signer, "signature": signed.signature,
    });
    let txt = reqwest::Client::new()
        .post(format!("{api_url}/order"))
        .json(&body)
        .send()
        .await?
        .text()
        .await?;
    println!("borsa yaniti: {txt}");

    let v: serde_json::Value = serde_json::from_str(&txt)?;
    let gercek = v["response"]["data"]["statuses"][0]["resting"]["oid"]
        .as_str()
        .or_else(|| v["response"]["data"]["statuses"][0]["filled"]["oid"].as_str());

    let Some(gercek) = gercek else {
        println!("\n❌ emir dinlenmedi, oid alinamadi — test sonuçsuz");
        return Ok(());
    };

    println!("\n=== 2. karsilastirma ===");
    println!("borsanin oid'i:  {gercek}");
    println!("tahmin:          {}", tahmin.to_base58());
    if gercek != tahmin.to_base58() {
        println!("\n❌ TUTMADI — oid ön-imzalanamaz. Fill deadline için başka yol lazım.");
        return Ok(());
    }
    println!("\n✅ TUTTU — oid gönderim öncesi biliniyor, iptal ön-imzalanabilir.");

    // 3. Aynı oid ile ön-imzalı iptal: gerçekten çalışıyor mu?
    println!("\n=== 3. ön-imzalı cx deneniyor ===");
    let signed_cx = signer
        .sign_group(
            vec![OrderItem::Cancel(Cancel::new("BTC-USD", tahmin))],
            None,
        )
        .map_err(|e| eyre::eyre!("cx imza: {e:?}"))?;
    let body = serde_json::json!({
        "actions": signed_cx.actions, "nonce": signed_cx.nonce,
        "account": signed_cx.account, "signer": signed_cx.signer, "signature": signed_cx.signature,
    });
    let txt = reqwest::Client::new()
        .post(format!("{api_url}/order"))
        .json(&body)
        .send()
        .await?
        .text()
        .await?;
    println!("cx yaniti: {txt}");
    if txt.contains("rejected") || txt.contains("\"ok\":false") {
        println!("\n⚠️ oid tuttu ama cx reddedildi — sebebe bak");
    } else {
        println!(
            "\n✅ ön-imzalı iptal çalışıyor: kullanıcının 'dolmazsa sor' senaryosu kurulabilir"
        );
    }
    Ok(())
}

// ── S11 yardımcıları ────────────────────────────────────────────────────────

async fn hesap_json(api_url: &str, pk: &str) -> eyre::Result<serde_json::Value> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{api_url}/account"))
        .json(&serde_json::json!({ "type": "fullAccount", "user": pk }))
        .send()
        .await?
        .json()
        .await?;
    Ok(v[0]["fullAccount"].clone())
}

/// BTC-USD pozisyon boyutu (yoksa 0).
async fn pozisyon(api_url: &str, pk: &str) -> eyre::Result<f64> {
    let a = hesap_json(api_url, pk).await?;
    Ok(a["positions"]
        .as_array()
        .and_then(|ps| ps.iter().find(|p| p["symbol"] == "BTC-USD"))
        .and_then(|p| p["size"].as_f64())
        .unwrap_or(0.0))
}

async fn emirleri_yaz(api_url: &str, pk: &str, baslik: &str) -> eyre::Result<()> {
    let a = hesap_json(api_url, pk).await?;
    let poz = a["positions"]
        .as_array()
        .and_then(|ps| ps.iter().find(|p| p["symbol"] == "BTC-USD"))
        .and_then(|p| p["size"].as_f64())
        .unwrap_or(0.0);
    println!("\n--- {baslik} ---");
    println!("pozisyon: {poz}");
    let bos = vec![];
    let os = a["openOrders"].as_array().unwrap_or(&bos);
    if os.is_empty() {
        println!("acik emir yok");
    }
    for o in os {
        println!(
            "  {} sz={} orig={} filled={} reduceOnly={} trigPx={} pxHi={} status={}",
            o["orderType"].as_str().unwrap_or("?"),
            o["size"],
            o["originalSize"],
            o["filledSize"],
            o["reduceOnly"],
            o["trigger"]["px"],
            o["trigger"]["pxHi"],
            o["status"],
        );
    }
    Ok(())
}

async fn gonder_items(
    api_url: &str,
    signer: &mut bulk_keychain::Signer,
    items: Vec<bulk_keychain::OrderItem>,
) -> eyre::Result<String> {
    let signed = signer
        .sign_group(items, None)
        .map_err(|e| eyre::eyre!("imza: {e:?}"))?;
    let body = serde_json::json!({
        "actions": signed.actions, "nonce": signed.nonce,
        "account": signed.account, "signer": signed.signer, "signature": signed.signature,
    });
    Ok(reqwest::Client::new()
        .post(format!("{api_url}/order"))
        .json(&body)
        .send()
        .await?
        .text()
        .await?)
}

fn market(sym: &str, is_buy: bool, size: f64, reduce_only: bool) -> bulk_keychain::OrderItem {
    use bulk_keychain::{Order, OrderItem, OrderType};
    OrderItem::Order(Order {
        symbol: sym.into(),
        is_buy,
        price: 0.0,
        size,
        reduce_only,
        iso: false,
        order_type: OrderType::market(),
        client_id: None,
        commission: None,
    })
}

async fn temizle(api_url: &str, signer: &mut bulk_keychain::Signer, pk: &str) -> eyre::Result<()> {
    use bulk_keychain::{CancelAll, OrderItem};
    println!("=== temizlik ===");
    let _ = gonder_items(
        api_url,
        signer,
        vec![OrderItem::CancelAll(CancelAll::all())],
    )
    .await?;
    let poz = pozisyon(api_url, pk).await?;
    if poz.abs() > 1e-9 {
        let r = gonder_items(
            api_url,
            signer,
            vec![market("BTC-USD", poz < 0.0, poz.abs(), true)],
        )
        .await?;
        println!("pozisyon kapatiliyor ({poz}): {}", &r[..r.len().min(120)]);
    }
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    println!("kalan pozisyon: {}", pozisyon(api_url, pk).await?);
    Ok(())
}

/// S11 — Kademeli çıkış (TP1 %30 / TP2 %70 + SL) kurulabiliyor mu, ve asıl
/// soru: TP1 dolup pozisyon küçülünce boyutu büyük kalan SL ne yapıyor?
///
/// `Stop`/`TakeProfit` struct'larinda `reduce_only` alani YOK, ama borsa acik
/// emirlerde `reduceOnly: true` gosteriyor. Reduce-only'nin iki anlami olabilir
/// ve biri tehlikeli:
///   (a) kirpar   -> min(emir, pozisyon) kapatir, ladder guvenli
///   (b) reddeder -> pozisyondan buyuk emir tumden reddedilir; TP1 dolduktan
///                   sonra SL reddedilir -> KORUMASIZ POZISYON
/// Varsayamayiz; olcuyoruz.
async fn cmd_exit_ladder(api_url: &str) -> eyre::Result<()> {
    use bulk_keychain::{
        Keypair as KcKeypair, OnFill, OrderItem, Signer as KcSigner, Stop, TakeProfit,
    };

    let keys = load_or_create_keys()?;
    let c = client(api_url, &keys.master)?;
    let px = c.get_ticker("BTC-USD").await?.mark_price;
    let kp = KcKeypair::from_base58(&keys.master).map_err(|e| eyre::eyre!("{e:?}"))?;
    let pk = kp.pubkey().to_base58();
    let mut signer = KcSigner::new(kp);
    println!("BTC mark: {px} | hesap: {pk}");

    temizle(api_url, &mut signer, &pk).await?;

    let tp = |size: f64, trig: f64| {
        OrderItem::TakeProfit(TakeProfit {
            symbol: "BTC-USD".into(),
            is_buy: true, // korunan pozisyon long
            size,
            trigger_price: trig,
            limit_price: f64::NAN,
            iso: false,
        })
    };
    let st = |size: f64, trig: f64| {
        OrderItem::Stop(Stop {
            symbol: "BTC-USD".into(),
            is_buy: true,
            size,
            trigger_price: trig,
            limit_price: f64::NAN,
            iso: false,
        })
    };

    // ── 1. Ladder kabul ediliyor mu? ────────────────────────────────────────
    println!("\n=== 1. [m, of p:0 -> tp1 %30 + tp2 %70 + st %100] ===");
    let r = gonder_items(
        api_url,
        &mut signer,
        vec![
            market("BTC-USD", true, 0.004, false),
            OrderItem::OnFill(OnFill {
                p: 0,
                actions: vec![
                    tp(0.0012, px * 1.05),
                    tp(0.0028, px * 1.10),
                    st(0.004, px * 0.90),
                ],
            }),
        ],
    )
    .await?;
    println!("{r}");
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    emirleri_yaz(api_url, &pk, "ladder kuruldu mu").await?;

    // ── 2. TP1'i doldur: pozisyon kuculunce SL'e ne oluyor? ─────────────────
    println!("\n=== 2. TP1 tetikleniyor (pozisyon 0.004 -> 0.0028 olmali) ===");
    let r = gonder_items(api_url, &mut signer, vec![tp(0.0012, px * 0.999)]).await?;
    println!("{r}");
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    emirleri_yaz(api_url, &pk, "TP1 sonrasi - SL hala 0.004 mu").await?;

    // ── 3. `st`in yonu ne? ──────────────────────────────────────────────────
    // 1. adim: st(is_buy=true, trig=px*0.90) "resting" dedi ama openOrders'da
    // YOK. 3. adim (onceki kosu): st(is_buy=true, trig=px*1.001) duruyor.
    // Yani is_buy=true olan bir stop, tetigi fiyatin ALTINDAYSA kayboluyor.
    // Hipotez: `is_buy` = TriggerBasket'teki `d` tuzagi ("esigin ustunde mi?"),
    // korunan pozisyonun yonu DEGIL. Oyleyse long'u korumak icin is_buy=false
    // gerekir. Olcuyoruz: long 0.002 acip iki yonu de deniyoruz.
    println!("\n=== 3. st yonu: long'u korumak icin is_buy ne olmali? ===");
    temizle(api_url, &mut signer, &pk).await?;
    let r = gonder_items(
        api_url,
        &mut signer,
        vec![market("BTC-USD", true, 0.002, false)],
    )
    .await?;
    println!("long 0.002: {}", &r[..r.len().min(90)]);
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let st_dir = |is_buy: bool, size: f64, trig: f64| {
        OrderItem::Stop(Stop {
            symbol: "BTC-USD".into(),
            is_buy,
            size,
            trigger_price: trig,
            limit_price: f64::NAN,
            iso: false,
        })
    };

    println!(
        "\n-- is_buy=true, trig={:.0} (fiyatin ALTINDA) --",
        px * 0.90
    );
    let r = gonder_items(api_url, &mut signer, vec![st_dir(true, 0.002, px * 0.90)]).await?;
    println!("{}", &r[..r.len().min(140)]);
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    emirleri_yaz(api_url, &pk, "is_buy=true sonrasi").await?;

    println!(
        "\n-- is_buy=false, trig={:.0} (fiyatin ALTINDA) --",
        px * 0.90
    );
    let r = gonder_items(api_url, &mut signer, vec![st_dir(false, 0.002, px * 0.90)]).await?;
    println!("{}", &r[..r.len().min(140)]);
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    emirleri_yaz(api_url, &pk, "is_buy=false sonrasi — long'un stopu bu mu?").await?;

    // ── 4. ASIL SORU: pozisyondan BUYUK koruma ne yapiyor? ──────────────────
    // Stop yerine TakeProfit kullaniyoruz: 2. adimda kanitlandi ki tetigi
    // zaten gecilmis bir tp HEMEN atesliyor. Stop'un yonu belirsiz oldugu
    // icin (bkz. 1/3. adim) soruyu tp ile soruyoruz — cevap ayni mekanizmayi
    // (reduce-only koruma emri) sinar.
    println!("\n=== 4. pozisyondan BUYUK koruma: kirpar mi, reddeder mi, ters mi acar? ===");
    temizle(api_url, &mut signer, &pk).await?;

    println!("long 0.002 aciliyor, ardindan 0.006'lik tp (3 KATI) tetikleniyor");
    let r = gonder_items(
        api_url,
        &mut signer,
        vec![market("BTC-USD", true, 0.002, false)],
    )
    .await?;
    println!("giris: {}", &r[..r.len().min(100)]);
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    println!("pozisyon: {}", pozisyon(api_url, &pk).await?);

    // Long tp: fiyat tetigin USTUNDE olunca atesler. px*0.999 zaten gecilmis.
    let r = gonder_items(api_url, &mut signer, vec![tp(0.006, px * 0.999)]).await?;
    println!("tp(0.006): {r}");
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

    let son = pozisyon(api_url, &pk).await?;
    emirleri_yaz(api_url, &pk, "buyuk tp sonrasi").await?;
    println!("\n>>> SONUC: son pozisyon = {son} (giris 0.002, koruma 0.006 idi)");
    if son.abs() < 1e-9 {
        println!("KIRPIYOR - pozisyon kadar kapatti, ters acmadi. Ladder guvenli.");
    } else if (son - 0.002).abs() < 1e-9 {
        println!("REDDETTI - pozisyon duruyor. KORUMASIZ POZISYON riski!");
    } else if son < 0.0 {
        println!("TERS ACTI - {son} short. Boyutlari elle yonetmek zorundayiz.");
    } else {
        println!("beklenmedik: {son}");
    }

    temizle(api_url, &mut signer, &pk).await?;
    Ok(())
}

/// S12 — `trig` icine kademeli cikis (tp+tp+st) sigiyor mu?
///
/// `TriggerBasket` doc'u "Nested actions may be: m, l, mod, cx, cxa, st, tp,
/// rng" diyor — yani sigmali. Ama §8.7'de `of`'un `trig` icine gomulemedigini
/// (`invalid action in trigger order`) ogrendik: doc'a guvenmiyoruz.
///
/// Onemli: OnChain sablonunda market REDDEDILIRSE cikislar yine kuruluyor
/// (§8.7). Kademeli cikista ayni risk var mi, o da goruluyor.
async fn cmd_trig_ladder(api_url: &str) -> eyre::Result<()> {
    use bulk_keychain::{
        Commission, Keypair as KcKeypair, Order, OrderItem, OrderType, Signer as KcSigner, Stop,
        TakeProfit, TriggerBasket,
    };

    let keys = load_or_create_keys()?;
    let c = client(api_url, &keys.master)?;
    let px = c.get_ticker("BTC-USD").await?.mark_price;
    let builder_pk_str = pubkey_of(&keys.builder)?;
    let kp = KcKeypair::from_base58(&keys.master).map_err(|e| eyre::eyre!("{e:?}"))?;
    let pk = kp.pubkey().to_base58();
    let mut signer = KcSigner::new(kp);
    println!("BTC mark: {px} | hesap: {pk}");

    temizle(api_url, &mut signer, &pk).await?;

    // Long girisi + kademeli cikis, hepsi trig icinde.
    // tp: is_buy=true (yukari tetikler) | st: is_buy=false (asagi tetikler)
    let giris = OrderItem::Order(Order {
        symbol: "BTC-USD".into(),
        is_buy: true,
        price: 0.0,
        size: 0.004,
        reduce_only: false,
        iso: false,
        order_type: OrderType::market(),
        client_id: None,
        commission: Some(
            Commission::new(
                bulk_keychain::Pubkey::from_base58(&builder_pk_str)
                    .map_err(|e| eyre::eyre!("{e:?}"))?,
                2,
            )
            .map_err(|e| eyre::eyre!("{e:?}"))?,
        ),
    });
    let tp = |size: f64, trig: f64| {
        OrderItem::TakeProfit(TakeProfit {
            symbol: "BTC-USD".into(),
            is_buy: true,
            size,
            trigger_price: trig,
            limit_price: f64::NAN,
            iso: false,
        })
    };
    let st = |size: f64, trig: f64| {
        OrderItem::Stop(Stop {
            symbol: "BTC-USD".into(),
            is_buy: false,
            size,
            trigger_price: trig,
            limit_price: f64::NAN,
            iso: false,
        })
    };

    println!("\n=== trig {{ actions: [m, tp %30, tp %70, st %50, st %50] }} ===");
    println!("(trig tetigi fiyatin ustunde + 'altina inerse' → hemen ateslemeli)");
    let basket = OrderItem::TriggerBasket(TriggerBasket {
        symbol: "BTC-USD".into(),
        // is_buy = "esigin ustunde mi?" tuzagi: false = altina inince atesle.
        is_buy: false,
        trigger_price: px * 1.05,
        actions: vec![
            giris,
            tp(0.0012, px * 1.05),
            tp(0.0028, px * 1.10),
            st(0.002, px * 0.92),
            st(0.002, px * 0.90),
        ],
        iso: false,
    });

    let r = gonder_items(api_url, &mut signer, vec![basket]).await?;
    println!("{r}");
    if r.contains("invalid action in trigger order") {
        println!("\n❌ trig kademeli cikis KABUL ETMIYOR — OnChain'de ladder yok.");
        temizle(api_url, &mut signer, &pk).await?;
        return Ok(());
    }
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    emirleri_yaz(api_url, &pk, "trig atesledikten sonra").await?;

    let poz = pozisyon(api_url, &pk).await?;
    println!("\n>>> pozisyon = {poz} (0.004 bekleniyor)");
    println!(">>> yukarida 4 koruma emri (tp 0.0012, tp 0.0028, st 0.002, st 0.002) gorunmeli");

    temizle(api_url, &mut signer, &pk).await?;
    Ok(())
}

/// S13 — Ayni imzali blob iki kez gonderilirse ne oluyor?
///
/// Storage tasariminin bagli oldugu soru. Watcher emri gonderip durumu
/// yazamadan cokerse, yeniden kalkinca alarm hala Armed gorunur ve blob
/// TEKRAR gonderilir. Kullanici ayni isleme iki kez girer — urunun en pahali
/// hatasi.
///
/// Nonce imzaya dahil ve sabit. Borsa nonce'u tek kullanimlik sayiyorsa
/// ikinci gonderim reddedilir → blob dogal olarak idempotent → cokme
/// guvenligi bedava gelir. Saymiyorsa storage'da write-ahead + reconcile
/// kurmak zorundayiz (gonderim ONCESI niyet kaydi, acilista borsayla
/// mutabakat).
///
/// Varsayamayiz; olcuyoruz.
async fn cmd_nonce_replay(api_url: &str) -> eyre::Result<()> {
    use bulk_keychain::{
        Commission, Keypair as KcKeypair, Order, OrderItem, OrderType, Signer as KcSigner,
        TimeInForce,
    };

    let keys = load_or_create_keys()?;
    let c = client(api_url, &keys.master)?;
    let px = c.get_ticker("BTC-USD").await?.mark_price;
    let builder_pk_str = pubkey_of(&keys.builder)?;
    let kp = KcKeypair::from_base58(&keys.master).map_err(|e| eyre::eyre!("{e:?}"))?;
    let pk = kp.pubkey().to_base58();
    let mut signer = KcSigner::new(kp);
    println!("BTC mark: {px} | hesap: {pk}");

    temizle(api_url, &mut signer, &pk).await?;

    // Dolmayacak bir limit: mark'in %20 altinda alis. Iki kez gonderilirse
    // defterde iki emir gorunur — sayarak anlariz.
    let limit_px = (px * 0.80 * 100.0).round() / 100.0;
    let order = Order {
        symbol: "BTC-USD".into(),
        is_buy: true,
        price: limit_px,
        size: 0.001,
        reduce_only: false,
        iso: false,
        order_type: OrderType::Limit {
            tif: TimeInForce::Gtc,
        },
        client_id: None,
        commission: Some(
            Commission::new(
                bulk_keychain::Pubkey::from_base58(&builder_pk_str)
                    .map_err(|e| eyre::eyre!("{e:?}"))?,
                2,
            )
            .map_err(|e| eyre::eyre!("{e:?}"))?,
        ),
    };

    // TEK imza, IKI gonderim — watcher'in cokup tekrar denemesinin taklidi.
    let signed = signer
        .sign_group(vec![OrderItem::Order(order)], None)
        .map_err(|e| eyre::eyre!("imza: {e:?}"))?;
    let body = serde_json::json!({
        "actions": signed.actions, "nonce": signed.nonce,
        "account": signed.account, "signer": signed.signer, "signature": signed.signature,
    });

    // HTTP durumunu da yazdiriyoruz: 504 gibi bir gateway hatasi "nonce
    // reddedildi" DEGIL, "istek islenmedi" olabilir — ikisi cok farkli.
    let gonder = || async {
        let r = reqwest::Client::new()
            .post(format!("{api_url}/order"))
            .json(&body)
            .send()
            .await?;
        let st = r.status();
        let txt = r.text().await?;
        Ok::<_, reqwest::Error>((st, txt))
    };

    let (st, txt) = gonder().await?;
    println!("\n=== 1. gonderim (nonce {}) ===", signed.nonce);
    println!("HTTP {st} | {txt}");
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let n1 = limit_sayisi(api_url, &pk).await?;
    println!("defterdeki limit emri: {n1}");

    // Ayni blob'u birkac kez deniyoruz: tek bir 504 tesaduf olabilir.
    println!("\n=== AYNI blob tekrar tekrar gonderiliyor ===");
    for i in 2..=4 {
        let (st, txt) = gonder().await?;
        let kisa: String = txt.chars().take(120).collect();
        println!("{i}. gonderim → HTTP {st} | {kisa}");
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        println!(
            "   defterdeki limit emri: {}",
            limit_sayisi(api_url, &pk).await?
        );
    }

    let n2 = limit_sayisi(api_url, &pk).await?;
    println!("\n>>> tekrar gonderim: {n1} → {n2}");
    if n2 != n1 {
        println!("❌ TEKRAR EMIR OLUSTU — kullanici ayni isleme iki kez girer.");
        println!("   Storage'da write-ahead + reconcile SART.");
        temizle(api_url, &mut signer, &pk).await?;
        return Ok(());
    }
    println!("Tekrar gonderim emir olusturmadi.");

    // ── Dedup neye bakiyor: nonce'a mi, emrin icerigine mi? ─────────────────
    // Ayrimi bilmek zorundayiz. Icerige bakiyorsa, kullanicinin BIRBIRININ
    // AYNI iki alarmi (ayni sembol/boyut/fiyat) carpisir ve ikincisi sessizce
    // hic girmez — bizim urettigimiz bir hata olur.
    // Ayni emri AYRI AYRI imzaliyoruz: farkli nonce, ayni icerik.
    println!("\n=== AYNI icerik, FARKLI nonce (iki ayri imza) ===");
    let ayni_emir = || Order {
        symbol: "BTC-USD".into(),
        is_buy: true,
        price: limit_px,
        size: 0.001,
        reduce_only: false,
        iso: false,
        order_type: OrderType::Limit {
            tif: TimeInForce::Gtc,
        },
        client_id: None,
        commission: None,
    };

    let mut olusan = 0;
    for i in 1..=2 {
        let s = signer
            .sign_group(vec![OrderItem::Order(ayni_emir())], None)
            .map_err(|e| eyre::eyre!("imza: {e:?}"))?;
        let b = serde_json::json!({
            "actions": s.actions, "nonce": s.nonce,
            "account": s.account, "signer": s.signer, "signature": s.signature,
        });
        let r = reqwest::Client::new()
            .post(format!("{api_url}/order"))
            .json(&b)
            .send()
            .await?;
        let st = r.status();
        let txt = r.text().await?;
        let kisa: String = txt.chars().take(110).collect();
        println!("imza #{i} (nonce {}) → HTTP {st} | {kisa}", s.nonce);
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let n = limit_sayisi(api_url, &pk).await?;
        println!("   defterdeki limit emri: {n}");
        olusan = n;
    }

    println!("\n>>> SONUC");
    // n2 = tekrar testinden kalan emir; +2 yeni imza bekliyoruz.
    if olusan == n2 + 2 {
        println!("✅ Dedup NONCE'a bakiyor, icerige degil.");
        println!("   → Ayni blob'u tekrar gondermek zararsiz (cokme guvenligi bedava).");
        println!("   → Birbirinin AYNI iki alarm carpismiyor.");
    } else {
        println!(
            "⚠️ Ayni icerikli iki AYRI imzadan {} emir olustu (2 bekleniyordu).",
            olusan - n2
        );
        println!("   Dedup icerige de bakiyor olabilir — ayni alarmi iki kez kuran");
        println!("   kullanicinin ikinci emri sessizce girmez. Arastirilmali.");
    }

    temizle(api_url, &mut signer, &pk).await?;
    Ok(())
}

/// Defterde duran limit emirlerinin sayisi.
async fn limit_sayisi(api_url: &str, pk: &str) -> eyre::Result<usize> {
    let a = hesap_json(api_url, pk).await?;
    Ok(a["openOrders"].as_array().map_or(0, |os| {
        os.iter()
            .filter(|o| o["orderType"].as_str() == Some("limit"))
            .count()
    }))
}
