//! Feed'i gerçek BULK API'sine karşı doğrular.
//!
//! Birim testler sahte veriyle çalışıyor; bu örnek asıl soruyu cevaplıyor:
//! canlı `/klines` yanıtını doğru parse edip kapanmış mumu doğru seçiyor muyuz?
//!
//! ```bash
//! cargo run -p pusu-feed --example live_check
//! cargo run -p pusu-feed --example live_check -- https://exchange-api.bulk.trade/api/v1
//! ```

use pusu_core::{Interval, Symbol};
use pusu_feed::{last_closed, HttpKlineSource, HttpMarkSource, KlineSource, MarkSource};
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://staging-api.bulk.trade/api/v1".into());
    println!("API: {base}\n");

    let src = HttpKlineSource::new(&base);
    let btc: Symbol = "BTC-USD".into();

    // 1. Ham yanıt doğru parse ediliyor mu?
    let ks = src.klines(&btc, Interval::H1, None).await?;
    let now = now_ms();
    println!("1h  → {} mum döndü", ks.len());

    let acik: Vec<_> = ks.iter().filter(|k| !k.is_closed_at(now)).collect();
    println!("   devam eden (T > now): {} adet", acik.len());
    for k in &acik {
        println!(
            "      T={} ({:+.1} sn) close={:.2}",
            k.close_time,
            (k.close_time as f64 - now as f64) / 1000.0,
            k.close
        );
    }

    match last_closed(&ks, now) {
        Some(k) => println!(
            "   son KAPANMIŞ: T={} ({:.1} sn önce) close={:.2} v={:.4}",
            k.close_time,
            (now - k.close_time) as f64 / 1000.0,
            k.close,
            k.volume
        ),
        None => println!("   ⚠️ kapanmış mum yok"),
    }

    // 2. startTime filtresi maliyeti gerçekten düşürüyor mu?
    let iki_saat_once = now - 2 * 3600 * 1000;
    let dar = src.klines(&btc, Interval::H1, Some(iki_saat_once)).await?;
    println!(
        "\n2. startTime filtresi: {} mum (filtresiz {} idi)",
        dar.len(),
        ks.len()
    );

    // 3. Mark price okunabiliyor mu? Bileşik koşulların bacağı buna bağlı.
    let mark = HttpMarkSource::new(&base).mark(&btc).await?;
    println!("\n3. BTC-USD mark: {mark:.2}");

    // 4. 10s mumuyla gerçek bir kapanış yakala — feed'in canlı kanıtı.
    //    Ateşleme kararı burada değil, pusu-engine'de; burada yalnızca
    //    "yeni kapanmış mum gerçekten geliyor mu" sorusu cevaplanıyor.
    println!("\n4. 10s mumuyla canlı kapanış bekleniyor (~36 sn)...");
    let mut son: Option<u64> = None;
    let mut yakalandi = false;
    for i in 0..12 {
        let now = now_ms();
        let ks = src.fresh_klines(&btc, Interval::S10).await?;
        if let Some(k) = last_closed(&ks, now) {
            if son.is_some_and(|prev| k.close_time > prev) {
                println!(
                    "   ✅ yeni kapanış: T={} close={:.2} ({:.1} sn önce, poll #{i})",
                    k.close_time,
                    k.close,
                    (now - k.close_time) as f64 / 1000.0,
                );
                yakalandi = true;
                break;
            }
            son = Some(k.close_time);
        }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
    if !yakalandi {
        println!("   ⚠️ 36 sn içinde kapanış yakalanamadı");
    }

    println!("\nBitti.");
    Ok(())
}
