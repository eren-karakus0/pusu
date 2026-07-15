//! Watcher döngüsünü **gerçek BULK API'sine** karşı çalıştırır.
//!
//! Birim testler sahte mumlarla çalışıyor. Bu örnek asıl soruyu cevaplıyor:
//! canlı veriyle döngü doğru anda ateşliyor, yanlış anda susuyor mu?
//!
//! Emir gönderimi **sahte** — dispatch ağa çıkmıyor, gerçek para riski yok.
//! Sınanan şey karar mantığı: hangi alarm ne zaman ateşliyor.
//!
//! 10s mumu kullanılıyor ki kanıt yarım dakikada çıksın; mantık 1h'te aynı.
//!
//! ```bash
//! cargo run -p pusu-engine --example live_watch
//! ```

use pusu_core::{
    Alert, AlertAction, AlertId, AlertState, Condition, Cross, Interval, Side, Symbol, TradeSpec,
};
use pusu_engine::{Dispatch, DispatchError, Watcher};
use pusu_feed::{HttpKlineSource, HttpMarkSource, MarkSource};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// Ağa çıkmayan sahte borsa: her zaman "doldu" der.
struct SahteBorsa;
impl Dispatch for SahteBorsa {
    async fn submit(&self, alert: &Alert) -> Result<serde_json::Value, DispatchError> {
        println!("      → [sahte gönderim] alarm {}", alert.id.as_str());
        Ok(
            serde_json::json!({"status":"ok","response":{"data":{"statuses":[
                {"filled":{"totalSz":0.001,"avgPx":0.0,"oid":"sahte"}}
            ]}}}),
        )
    }
}

fn alarm(id: &str, price: f64, cross: Cross, armed_at_ms: u64) -> Alert {
    Alert {
        id: AlertId::new(id),
        owner: String::new(),
        account: String::new(),
        condition: Condition::CandleClose {
            symbol: Symbol::new("BTC-USD"),
            interval: Interval::S10,
            cross,
            price,
        },
        invalidate: None,
        action: AlertAction::Trade(TradeSpec {
            symbol: Symbol::new("BTC-USD"),
            side: Side::Buy,
            size: 0.001,
            bracket: None,
        }),
        state: AlertState::Armed,
        armed_at_ms,
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://staging-api.bulk.trade/api/v1".into());
    println!("API: {base}\n");

    let mark = HttpMarkSource::new(&base)
        .mark(&Symbol::new("BTC-USD"))
        .await?;
    println!("BTC-USD mark: {mark:.2}\n");

    let armed = now_ms();

    // Üç alarm, üç ayrı davranış bekleniyor.
    let mut alerts = vec![
        // 1. Eşik mark'ın çok altında → ilk kapanışta ateşlemeli.
        alarm("ateslemeli", mark * 0.9, Cross::Above, armed),
        // 2. Eşik mark'ın çok üstünde → hiç ateşlememeli.
        alarm("susmali", mark * 1.1, Cross::Above, armed),
        // 3. Eşik düşük AMA gelecekte kurulmuş gibi → tazelik kapısı tutmalı,
        //    yani bu turlarda kapanan hiçbir mum onu ateşleyemez.
        alarm(
            "gelecekte-kurulmus",
            mark * 0.9,
            Cross::Above,
            armed + 600_000,
        ),
    ];

    let mut w = Watcher::new(
        HttpKlineSource::new(&base),
        HttpMarkSource::new(&base),
        SahteBorsa,
    );

    println!("10s mumları izleniyor (~36 sn)...\n");
    for i in 0..12 {
        let t = w.tick(&mut alerts, now_ms()).await;

        if !t.feed_errors.is_empty() {
            println!("  #{i}: ⚠️ feed hatası: {:?}", t.feed_errors);
        }
        for r in &t.fired {
            println!("  #{i}: 🔫 {} → {:?}", r.id.as_str(), r.outcome);
        }
        if t.fired.is_empty() && t.feed_errors.is_empty() {
            println!("  #{i}: sessiz");
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    println!("\nSonuç:");
    for a in &alerts {
        println!("  {:<20} {:?}", a.id.as_str(), a.state);
    }

    // Kanıt: doğru alarm ateşledi, yanlış olanlar susmuş olmalı.
    let durum = |id: &str| alerts.iter().find(|a| a.id.as_str() == id).unwrap().state;

    assert_eq!(
        durum("susmali"),
        AlertState::Armed,
        "eşiğin çok üstündeki alarm ateşlememeliydi"
    );
    assert_eq!(
        durum("gelecekte-kurulmus"),
        AlertState::Armed,
        "tazelik kapısı tutmadı: kurulmadan önce kapanan mumla ateşlendi"
    );
    if durum("ateslemeli") == AlertState::Fired {
        println!("\n✅ Döngü canlı veriyle doğru çalıştı.");
    } else {
        println!("\n⚠️ 36 sn içinde 10s kapanışı yakalanamadı — tekrar dene.");
    }

    Ok(())
}
