//! Hesap paneli — bakiye, açık pozisyon, açık emir sayısı.
//!
//! `POST /account` (fullAccount) doğrudan BULK'tan okunuyor (public-by-pubkey);
//! PUSU sunucusu araya girmiyor, non-custodial okuma. Sub-account'un durumunu
//! ~8 sn'de bir tazeliyor. Veri yoksa (fonsuz/demo) tüm alanlar "—".
//!
//! ⚠️ Marjin/bakiye ve pozisyon P&L için ham JSON anahtarları fonlu bir hesapla
//! netleşecek; şimdilik yaygın adayları deneyip bulamayınca "—" gösteriyoruz.

use leptos::prelude::*;
use serde_json::Value;
use std::time::Duration;
use wasm_bindgen_futures::spawn_local;

use crate::feed;

/// Bir kapsamda aday anahtarlardan ilk sayıyı bul (sayı ya da sayı-string).
fn num_key(v: &Value, keys: &[&str]) -> Option<f64> {
    for k in keys {
        if let Some(n) = v.get(k).and_then(Value::as_f64) {
            return Some(n);
        }
        if let Some(n) = v
            .get(k)
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<f64>().ok())
        {
            return Some(n);
        }
    }
    None
}

fn balance_of(acct: &Value) -> Option<f64> {
    let cands = [
        "total_balance",
        "accountValue",
        "totalBalance",
        "equity",
        "marginBalance",
        "balance",
        "totalRawUsd",
    ];
    for scope in [acct, &acct["margin"], &acct["marginSummary"]] {
        if let Some(n) = num_key(scope, &cands) {
            return Some(n);
        }
    }
    None
}

/// Verilen sembol için (imzalı boyut, gerçekleşmemiş P&L). Boyut 0 → pozisyon yok.
fn position_of(acct: &Value, symbol: &str) -> Option<(f64, Option<f64>)> {
    let ps = acct["positions"].as_array()?;
    let p = ps.iter().find(|p| p["symbol"] == symbol)?;
    let size = p["size"].as_f64().unwrap_or(0.0);
    let pnl = num_key(p, &["unrealizedPnl", "unrealisedPnl", "uPnl", "pnl"]);
    Some((size, pnl))
}

fn orders_count(acct: &Value) -> usize {
    acct["openOrders"].as_array().map_or(0, Vec::len)
}

fn money(n: f64) -> String {
    format!("{n:.2}")
}

#[component]
pub fn AccountPanel(sub: String, #[prop(into)] symbol: Signal<String>) -> impl IntoView {
    let acct = RwSignal::new(None::<Value>);

    // Mount + ~8 sn tazeleme. Hata → None (alanlar "—").
    let fetch = {
        let sub = sub.clone();
        move || {
            let sub = sub.clone();
            spawn_local(async move {
                match feed::account(&sub).await {
                    Ok(v) if !v.is_null() => acct.set(Some(v)),
                    _ => acct.set(None),
                }
            });
        }
    };
    fetch();
    let ah = set_interval_with_handle(fetch, Duration::from_millis(8000)).ok();
    on_cleanup(move || {
        if let Some(h) = ah {
            h.clear();
        }
    });

    let balance = move || {
        acct.get()
            .as_ref()
            .and_then(balance_of)
            .map_or("—".to_string(), |b| format!("${}", money(b)))
    };
    let orders = move || {
        acct.get()
            .as_ref()
            .map_or("—".to_string(), |a| orders_count(a).to_string())
    };

    view! {
        <section class="acct-panel">
            <div class="acct-tile">
                <span class="acct-k">"Balance"</span>
                <span class="acct-v">{balance}</span>
            </div>
            <div class="acct-tile">
                <span class="acct-k">{move || format!("{} pos", symbol.get())}</span>
                {move || {
                    let a = acct.get();
                    let sym = symbol.get();
                    match a.as_ref().and_then(|a| position_of(a, &sym)) {
                        Some((size, pnl)) if size != 0.0 => {
                            let cls = if size >= 0.0 { "acct-v up" } else { "acct-v down" };
                            let pnl_txt = pnl
                                .map(|p| {
                                    let sign = if p >= 0.0 { "+" } else { "" };
                                    format!(" · {sign}{}", money(p))
                                })
                                .unwrap_or_default();
                            view! { <span class=cls>{format!("{size}{pnl_txt}")}</span> }.into_any()
                        }
                        _ => view! { <span class="acct-v muted">"flat"</span> }.into_any(),
                    }
                }}
            </div>
            <div class="acct-tile">
                <span class="acct-k">"Open orders"</span>
                <span class="acct-v">{orders}</span>
            </div>
        </section>
    }
}
