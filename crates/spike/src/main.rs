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
    }
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
