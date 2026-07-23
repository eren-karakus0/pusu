//! Bir cümleyi uçtan uca gösterir: doğal dil → taslak → notlar → sınıflandırma
//! → imzalanabilir `OrderItem`'lar.
//!
//! Çalıştır:
//! ```text
//! cargo run -p pusu-nl --example compile_sentence -- \
//!   "if the 1h candle closes above $90k, long 0.5 BTC, cancel if price drops below $88k, SL $88k"
//! ```

use pusu_compile::{compile, Compiled};
use pusu_nl::{parse, AlertCtx};

// Demo builder pubkey (örnekteki değer). Gerçek fee bu hesaba yazılır.
const BUILDER: &str = "AdjWd4DCeKC3P4QjRaP5BmmcPMs1YaQ8kRjPqpnbnqdz";

fn main() {
    let input = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let input = if input.trim().is_empty() {
        "if the 1h candle closes above $90k, long 0.5 BTC, cancel if price drops below $88k, SL $88k"
            .to_string()
    } else {
        input
    };

    println!("\n  “{input}”\n");

    let parsed = match parse(&input) {
        Ok(p) => p,
        Err(e) => {
            println!("  ✗ {e}\n");
            std::process::exit(1);
        }
    };

    println!("  ── draft ──────────────────────────────────────────");
    println!("  condition : {:?}", parsed.draft.condition);
    if let Some(inv) = &parsed.draft.invalidate {
        println!("  cancel if : {inv:?}");
    }
    println!("  action    : {:?}", parsed.draft.action);

    println!("\n  ── notes ──────────────────────────────────────────");
    for n in &parsed.notes {
        println!("  • [{}] {}", n.kind(), n.text());
    }

    let alert = parsed.draft.into_alert(AlertCtx {
        id: "demo".into(),
        owner: "master".into(),
        account: "sub".into(),
        now_ms: 0,
    });

    println!("\n  ── compiled ───────────────────────────────────────");
    match compile(&alert, BUILDER) {
        Ok(Compiled::OnChain { items }) => {
            println!("  🔒 on-chain trigger basket, {} item(s)", items.len());
            for it in &items {
                println!("     {it:?}");
            }
        }
        Ok(Compiled::Watched { items }) => {
            println!("  ⚡ watched bundle, {} item(s)", items.len());
            for it in &items {
                println!("     {it:?}");
            }
        }
        Ok(Compiled::NotifyOnly) => println!("  🔔 notify only — nothing to sign"),
        Err(e) => println!("  ✗ compile error: {e}"),
    }
    println!();
}
