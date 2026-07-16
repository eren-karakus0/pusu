//! Arayüz (Leptos CSR): onboarding sihirbazı + alarm kurma.
//!
//! `App` üst durumu (master/sub/onay) tutuyor ve onay tamamlanınca
//! [`Onboarding`]'den [`AlertBuilder`]'a geçiyor. İş mantığı modüllerde
//! ([`crate::onboarding`], [`crate::alert`]); bu dosya durum + görünüm.

use leptos::prelude::*;
use pusu_core::{Interval, Side};
use wasm_bindgen_futures::spawn_local;

use crate::alert::{self, Placed};
use crate::{config, onboarding, wallet};

#[derive(Clone)]
struct Notice {
    ok: bool,
    text: String,
}

/// Pubkey'i kısalt: `AbcDef…XyzUvw`.
fn short(pk: &str) -> String {
    if pk.len() <= 14 {
        return pk.to_string();
    }
    format!("{}…{}", &pk[..6], &pk[pk.len() - 6..])
}

fn num(s: &str) -> f64 {
    s.trim().parse::<f64>().unwrap_or(0.0)
}

fn parse_interval(s: &str) -> Interval {
    match s {
        "15m" => Interval::M15,
        "4h" => Interval::H4,
        "1d" => Interval::D1,
        _ => Interval::H1,
    }
}

#[component]
pub fn App() -> impl IntoView {
    let master = RwSignal::new(None::<String>);
    let sub = RwSignal::new(None::<String>);
    let approved = RwSignal::new(false);

    view! {
        <main class="shell">
            <header class="brand">
                <span class="mark">"PUSU"</span>
                <span class="tag">"alarm ki işlemi de yapar"</span>
            </header>

            {move || {
                if approved.get() {
                    let m = master.get().unwrap_or_default();
                    let s = sub.get().unwrap_or_default();
                    view! { <AlertBuilder master=m sub=s /> }.into_any()
                } else {
                    view! { <Onboarding master sub approved /> }.into_any()
                }
            }}

            <footer class="accounts">
                {move || master.get().map(|m| view! { <span>"master " {short(&m)}</span> })}
                {move || sub.get().map(|s| view! { <span>"sub " {short(&s)}</span> })}
            </footer>
        </main>
    }
}

#[component]
fn Onboarding(
    master: RwSignal<Option<String>>,
    sub: RwSignal<Option<String>>,
    approved: RwSignal<bool>,
) -> impl IntoView {
    let margin = RwSignal::new("200".to_string());
    let busy = RwSignal::new(false);
    let notice = RwSignal::new(None::<Notice>);

    let on_connect = move |_| {
        busy.set(true);
        notice.set(None);
        spawn_local(async move {
            match wallet::connect().await {
                Ok(pk) => {
                    notice.set(Some(Notice {
                        ok: true,
                        text: "Cüzdan bağlandı.".into(),
                    }));
                    master.set(Some(pk));
                }
                Err(e) => notice.set(Some(Notice {
                    ok: false,
                    text: e.to_string(),
                })),
            }
            busy.set(false);
        });
    };

    let on_create_sub = move |_| {
        let Some(m) = master.get() else { return };
        let Ok(amount) = margin.get().trim().parse::<f64>() else {
            notice.set(Some(Notice {
                ok: false,
                text: "Geçerli bir miktar gir.".into(),
            }));
            return;
        };
        busy.set(true);
        notice.set(None);
        spawn_local(async move {
            match onboarding::create_subaccount(&m, "pusu-1", amount).await {
                Ok(pk) => {
                    notice.set(Some(Notice {
                        ok: true,
                        text: "Sub-account açıldı.".into(),
                    }));
                    sub.set(Some(pk));
                }
                Err(e) => notice.set(Some(Notice {
                    ok: false,
                    text: e.to_string(),
                })),
            }
            busy.set(false);
        });
    };

    let on_approve = move |_| {
        let Some(m) = master.get() else { return };
        busy.set(true);
        notice.set(None);
        spawn_local(async move {
            match onboarding::approve_builder(&m).await {
                Ok(()) => {
                    notice.set(Some(Notice {
                        ok: true,
                        text: "Builder onayı verildi.".into(),
                    }));
                    approved.set(true);
                }
                Err(e) => notice.set(Some(Notice {
                    ok: false,
                    text: e.to_string(),
                })),
            }
            busy.set(false);
        });
    };

    let active = move || {
        if master.get().is_none() {
            0
        } else if sub.get().is_none() {
            1
        } else {
            2
        }
    };

    let body = move || {
        match active() {
        0 => view! {
            <p class="lead">"Kendi cüzdanınla imzala; anahtarın hiçbir zaman bize gelmez."</p>
            <button class="cta" on:click=on_connect disabled=busy>"Cüzdanı bağla"</button>
        }
        .into_any(),
        1 => view! {
            <p class="lead">
                "Ayrı bir sub-account açıyoruz. Riske atacağın miktar burada kalır — "
                "master hesabına asla dokunmuyoruz."
            </p>
            <label class="field">
                <span>"Başlangıç teminatı (USDC)"</span>
                <input
                    r#type="number"
                    prop:value=move || margin.get()
                    on:input=move |ev| margin.set(event_target_value(&ev))
                />
            </label>
            <button class="cta" on:click=on_create_sub disabled=busy>"Sub-account aç ve imzala"</button>
        }
        .into_any(),
        _ => view! {
            <p class="lead">
                "PUSU " {config::BUILDER_FEE_BPS} " bps builder fee kesiyor. "
                "Onayladığın oran ile kestiğimiz aynıdır — sonradan sessizce artmaz."
            </p>
            <p class="muted">"İstediğin an onayı geri çekebilirsin; tüm bekleyen alarmlar ölür."</p>
            <button class="cta" on:click=on_approve disabled=busy>
                {move || format!("Builder onayı ver ({} bps)", config::BUILDER_FEE_BPS)}
            </button>
        }
        .into_any(),
    }
    };

    let step = move |n: i32, label: &'static str| {
        let cls = move || {
            let a = active();
            if a > n {
                "step done"
            } else if a == n {
                "step now"
            } else {
                "step"
            }
        };
        view! { <li class=cls>{label}</li> }
    };

    view! {
        <ol class="rail">
            {step(0, "Bağlan")}
            {step(1, "Fonla")}
            {step(2, "Onayla")}
        </ol>
        <section class="card">
            {body}
            {move || {
                notice
                    .get()
                    .map(|n| view! { <p class=if n.ok { "notice ok" } else { "notice err" }>{n.text}</p> })
            }}
        </section>
    }
}

#[component]
fn AlertBuilder(master: String, sub: String) -> impl IntoView {
    let symbol = RwSignal::new("BTC-USD".to_string());
    let ctype = RwSignal::new("candle".to_string());
    let interval = RwSignal::new("1h".to_string());
    let dir = RwSignal::new("above".to_string());
    let price = RwSignal::new(String::new());
    let side = RwSignal::new("long".to_string());
    let size = RwSignal::new(String::new());
    let entry = RwSignal::new("market".to_string());
    let limit_price = RwSignal::new(String::new());
    let stop = RwSignal::new(String::new());
    let target = RwSignal::new(String::new());
    let inv_on = RwSignal::new(false);
    let inv_dir = RwSignal::new("below".to_string());
    let inv_price = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let notice = RwSignal::new(None::<Notice>);

    let submit = move |_| {
        let form = alert::Form {
            symbol: symbol.get(),
            use_candle: ctype.get() == "candle",
            interval: parse_interval(&interval.get()),
            above: dir.get() == "above",
            price: num(&price.get()),
            side: if side.get() == "long" {
                Side::Buy
            } else {
                Side::Sell
            },
            size: num(&size.get()),
            limit_entry: entry.get() == "limit",
            limit_price: num(&limit_price.get()),
            stop: num(&stop.get()),
            target: num(&target.get()),
            inv_on: inv_on.get(),
            inv_above: inv_dir.get() == "above",
            inv_price: num(&inv_price.get()),
        };
        let alert = match alert::build_alert(&form, &master, &sub) {
            Ok(a) => a,
            Err(e) => {
                notice.set(Some(Notice { ok: false, text: e }));
                return;
            }
        };
        busy.set(true);
        notice.set(None);
        let master = master.clone();
        let sub = sub.clone();
        spawn_local(async move {
            let text = match alert::submit(alert, &master, &sub).await {
                Ok(Placed::OnChain) => (true, "Borsaya kuruldu — borsa tutuyor.".to_string()),
                Ok(Placed::Watched) => (true, "Kuruldu — watcher izliyor.".to_string()),
                Err(e) => (false, e),
            };
            notice.set(Some(Notice {
                ok: text.0,
                text: text.1,
            }));
            busy.set(false);
        });
    };

    // Rozet Alert::execution()'ı yansıtıyor: iptal koşulu olan alarm borsaya
    // bırakılamaz (trigger kendini iptal edemez), mum kapanışı da zincirde yok.
    let badge = move || {
        if inv_on.get() || ctype.get() == "candle" {
            ("badge watch", "⚡ Watcher yürütür")
        } else {
            ("badge chain", "🔒 Borsa yürütür")
        }
    };

    view! {
        <section class="card">
            <div class="cardhead">
                <h2>"Alarm kur"</h2>
                <span class=move || badge().0>{move || badge().1}</span>
            </div>

            <label class="field">
                <span>"Sembol"</span>
                <input
                    prop:value=move || symbol.get()
                    on:input=move |ev| symbol.set(event_target_value(&ev))
                />
            </label>

            <div class="row">
                <label class="field">
                    <span>"Tetik"</span>
                    <select
                        prop:value=move || ctype.get()
                        on:change=move |ev| ctype.set(event_target_value(&ev))
                    >
                        <option value="candle">"Mum kapanışı"</option>
                        <option value="mark">"Anlık fiyat"</option>
                    </select>
                </label>
                {move || {
                    (ctype.get() == "candle").then(|| view! {
                        <label class="field">
                            <span>"Periyot"</span>
                            <select
                                prop:value=move || interval.get()
                                on:change=move |ev| interval.set(event_target_value(&ev))
                            >
                                <option value="15m">"15 dakika"</option>
                                <option value="1h">"Saatlik"</option>
                                <option value="4h">"4 saatlik"</option>
                                <option value="1d">"Günlük"</option>
                            </select>
                        </label>
                    })
                }}
            </div>

            <div class="row">
                <label class="field">
                    <span>"Yön"</span>
                    <select
                        prop:value=move || dir.get()
                        on:change=move |ev| dir.set(event_target_value(&ev))
                    >
                        <option value="above">"Üstüne çıkarsa"</option>
                        <option value="below">"Altına inerse"</option>
                    </select>
                </label>
                <label class="field">
                    <span>"Eşik fiyatı"</span>
                    <input
                        r#type="number"
                        prop:value=move || price.get()
                        on:input=move |ev| price.set(event_target_value(&ev))
                    />
                </label>
            </div>

            <div class="row">
                <label class="field">
                    <span>"İşlem"</span>
                    <select
                        prop:value=move || side.get()
                        on:change=move |ev| side.set(event_target_value(&ev))
                    >
                        <option value="long">"Long"</option>
                        <option value="short">"Short"</option>
                    </select>
                </label>
                <label class="field">
                    <span>"Miktar"</span>
                    <input
                        r#type="number"
                        prop:value=move || size.get()
                        on:input=move |ev| size.set(event_target_value(&ev))
                    />
                </label>
            </div>

            <div class="row">
                <label class="field">
                    <span>"Giriş"</span>
                    <select
                        prop:value=move || entry.get()
                        on:change=move |ev| entry.set(event_target_value(&ev))
                    >
                        <option value="market">"Market"</option>
                        <option value="limit">"Limit (retest)"</option>
                    </select>
                </label>
                {move || {
                    (entry.get() == "limit").then(|| view! {
                        <label class="field">
                            <span>"Limit fiyatı"</span>
                            <input
                                r#type="number"
                                prop:value=move || limit_price.get()
                                on:input=move |ev| limit_price.set(event_target_value(&ev))
                            />
                        </label>
                    })
                }}
            </div>

            <div class="row">
                <label class="field">
                    <span>"Stop (opsiyonel)"</span>
                    <input
                        r#type="number"
                        prop:value=move || stop.get()
                        on:input=move |ev| stop.set(event_target_value(&ev))
                    />
                </label>
                <label class="field">
                    <span>"Hedef (opsiyonel)"</span>
                    <input
                        r#type="number"
                        prop:value=move || target.get()
                        on:input=move |ev| target.set(event_target_value(&ev))
                    />
                </label>
            </div>

            <label class="check">
                <input
                    r#type="checkbox"
                    prop:checked=move || inv_on.get()
                    on:change=move |ev| inv_on.set(event_target_checked(&ev))
                />
                <span>"Setup bozulursa iptal et"</span>
            </label>

            {move || {
                inv_on.get().then(|| view! {
                    <div class="row">
                        <label class="field">
                            <span>"İptal yönü"</span>
                            <select
                                prop:value=move || inv_dir.get()
                                on:change=move |ev| inv_dir.set(event_target_value(&ev))
                            >
                                <option value="below">"Altına inerse"</option>
                                <option value="above">"Üstüne çıkarsa"</option>
                            </select>
                        </label>
                        <label class="field">
                            <span>"İptal fiyatı"</span>
                            <input
                                r#type="number"
                                prop:value=move || inv_price.get()
                                on:input=move |ev| inv_price.set(event_target_value(&ev))
                            />
                        </label>
                    </div>
                    <p class="muted">
                        "İptal koşulu anlık fiyata bakar — setup öldüğü an alarm düşer, "
                        "işlem hiç girmez. Bu alarm borsaya bırakılamaz; PUSU izler."
                    </p>
                })
            }}

            <p class="muted">
                {format!("Girişte {} bps builder fee. Koruma emirleri (stop/hedef) ücretsiz.", config::BUILDER_FEE_BPS)}
            </p>

            <button class="cta" on:click=submit disabled=busy>"Alarmı kur ve imzala"</button>

            {move || {
                notice
                    .get()
                    .map(|n| view! { <p class=if n.ok { "notice ok" } else { "notice err" }>{n.text}</p> })
            }}
        </section>
    }
}
