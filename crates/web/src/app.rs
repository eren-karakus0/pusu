//! Arayüz (Leptos CSR): onboarding sihirbazı + alarm kurma.
//!
//! `App` üst durumu (master/sub/onay) tutuyor ve onay tamamlanınca
//! [`Onboarding`]'den [`AlertBuilder`]'a geçiyor. İş mantığı modüllerde
//! ([`crate::onboarding`], [`crate::alert`]); bu dosya durum + görünüm.

use leptos::prelude::*;
use pusu_core::{
    Alert, AlertAction, AlertId, AlertState, Condition, Cross, Entry, Execution, ExitLeg, Exits,
    Interval, Side, Symbol, TradeSpec,
};
use wasm_bindgen_futures::spawn_local;

use crate::account::AccountPanel;
use crate::alert::{self, Placed};
use crate::chart::{CandleChart, ChartHandle};
use crate::{api, config, onboarding, wallet};

#[derive(Clone)]
struct Notice {
    ok: bool,
    text: String,
}

/// Geçici bildirim: başarı/hata sonucu köşede belirir, ~4.5 sn sonra kaybolur.
/// Senkron doğrulama hataları hâlâ form içinde (inline) kalır; toast "işlem
/// gerçekleşti/olmadı" gibi asenkron sonuçlar için.
#[derive(Clone)]
struct Toast {
    id: u64,
    ok: bool,
    text: String,
}

/// Toast kuyruğu — Dashboard kökünde context olarak sağlanır.
#[derive(Clone, Copy)]
struct ToastHub(RwSignal<Vec<Toast>>);

/// Bağlamdaki toast merkezine bir bildirim it (yoksa sessizce yut). Kendini siler.
fn toast(ok: bool, text: impl Into<String>) {
    let Some(hub) = use_context::<ToastHub>() else {
        return;
    };
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let id = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let text = text.into();
    hub.0.update(|v| v.push(Toast { id, ok, text }));
    set_timeout(
        move || hub.0.update(|v| v.retain(|t| t.id != id)),
        std::time::Duration::from_millis(4500),
    );
}

/// Sağ-alt köşede yığılan toast'lar. Tıklayınca kapanır.
#[component]
fn ToastHost() -> impl IntoView {
    let hub = expect_context::<ToastHub>();
    view! {
        <div class="toast-host">
            <For each=move || hub.0.get() key=|t| t.id let:t>
                {
                    let id = t.id;
                    let cls = if t.ok { "toast ok" } else { "toast err" };
                    view! {
                        <div
                            class=cls
                            on:click=move |_| hub.0.update(|v| v.retain(|x| x.id != id))
                        >
                            <span class="toast-dot"></span>
                            <span class="toast-text">{t.text}</span>
                        </div>
                    }
                }
            </For>
        </div>
    }
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

/// URL'de `?demo` var mı — cüzdansız arayüz önizlemesi (onboarding'i atlar).
fn is_demo() -> bool {
    web_sys::window()
        .and_then(|w| w.location().search().ok())
        .is_some_and(|s| s.contains("demo"))
}

/// Önizleme modunda kullanılan yer-tutucu hesaplar (gerçek imza/işlem yok).
const DEMO_MASTER: &str = "DemoMasterPreviewAcct7h3x9k2mNpQrStUvWx";
const DEMO_SUB: &str = "DemoSubPreviewAcct4a8b2c6d9eFgHjKmNpQr";

/// Önizleme (demo) modunda "My alerts"i canlandıran örnek alarmlar — gerçek
/// imza/işlem yok, yalnız arayüzü göstermek için. BTC örneği seçili pariteyle
/// eşleşir; canlı "eşiğe ne kadar kaldı" mesafesi orada görünür.
fn demo_alerts() -> Vec<Alert> {
    let now = js_sys::Date::now() as u64;
    let mins_ago = |m: u64| now.saturating_sub(m * 60_000);

    let mk = |id: &str,
              condition: Condition,
              action: AlertAction,
              state: AlertState,
              armed_at_ms: u64|
     -> Alert {
        Alert {
            id: AlertId::new(id),
            owner: DEMO_MASTER.to_string(),
            account: DEMO_SUB.to_string(),
            condition,
            invalidate: None,
            action,
            state,
            armed_at_ms,
            entry_oid: None,
            fill_deadline_ms: None,
            cancel_requested: false,
        }
    };

    vec![
        mk(
            "demo-btc",
            Condition::CandleClose {
                symbol: Symbol::new("BTC-USD"),
                interval: Interval::H1,
                cross: Cross::Above,
                price: 65_000.0,
            },
            AlertAction::Trade(TradeSpec {
                symbol: Symbol::new("BTC-USD"),
                side: Side::Buy,
                size: 0.5,
                entry: Entry::Market,
                exits: Some(Exits {
                    take_profits: vec![ExitLeg::new(67_500.0, 100.0)],
                    stops: vec![ExitLeg::new(63_000.0, 100.0)],
                }),
            }),
            AlertState::Armed,
            mins_ago(14),
        ),
        mk(
            "demo-eth",
            Condition::MarkCross {
                symbol: Symbol::new("ETH-USD"),
                cross: Cross::Below,
                price: 3_000.0,
            },
            AlertAction::Trade(TradeSpec {
                symbol: Symbol::new("ETH-USD"),
                side: Side::Sell,
                size: 2.0,
                entry: Entry::Market,
                exits: None,
            }),
            AlertState::Working,
            mins_ago(126),
        ),
        mk(
            "demo-sol",
            Condition::CandleClose {
                symbol: Symbol::new("SOL-USD"),
                interval: Interval::H4,
                cross: Cross::Above,
                price: 200.0,
            },
            AlertAction::Trade(TradeSpec {
                symbol: Symbol::new("SOL-USD"),
                side: Side::Buy,
                size: 10.0,
                entry: Entry::Limit { price: 198.0 },
                exits: Some(Exits {
                    take_profits: vec![ExitLeg::new(210.0, 50.0), ExitLeg::new(225.0, 50.0)],
                    stops: vec![ExitLeg::new(190.0, 100.0)],
                }),
            }),
            AlertState::Fired,
            mins_ago(1500),
        ),
    ]
}

#[component]
pub fn App() -> impl IntoView {
    // `?demo` ile doğrudan Dashboard'a gir (cüzdan gerekmeden UI'ı gör).
    let demo = is_demo();
    let master = RwSignal::new(demo.then(|| DEMO_MASTER.to_string()));
    let sub = RwSignal::new(demo.then(|| DEMO_SUB.to_string()));
    let approved = RwSignal::new(demo);

    view! {
        {move || {
            if approved.get() {
                // Onaylı → tam genişlik terminal (dar shell'in dışında).
                let m = master.get().unwrap_or_default();
                let s = sub.get().unwrap_or_default();
                view! { <Dashboard master=m sub=s /> }.into_any()
            } else {
                // Onboarding → dar, ortalanmış sihirbaz.
                view! {
                    <main class="shell">
                        <header class="brand">
                            <span class="mark">"PUSU"</span>
                            <span class="tag">"the alarm that pulls the trigger"</span>
                        </header>
                        <Onboarding master sub approved />
                    </main>
                }
                    .into_any()
            }
        }}
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
                        text: "Wallet connected.".into(),
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
                text: "Enter a valid amount.".into(),
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
                        text: "Sub-account created.".into(),
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
                        text: "Builder approval granted.".into(),
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

    // Cüzdansız arayüz önizlemesi — Phantom imza sorunu çözülene kadar UI'ı
    // gerçek hesap olmadan gezmek için. Yer-tutucu hesaplar; imza/işlem yok.
    let on_preview = move |_| {
        master.set(Some(DEMO_MASTER.to_string()));
        sub.set(Some(DEMO_SUB.to_string()));
        approved.set(true);
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
            <p class="lead">"Sign with your own wallet; your key never reaches us."</p>
            <button class="cta" on:click=on_connect disabled=busy>"Connect wallet"</button>
            <button class="ghost preview-btn" on:click=on_preview>
                "Preview the interface (no wallet) →"
            </button>
        }
        .into_any(),
        1 => view! {
            <p class="lead">
                "We open a separate sub-account. What you put at risk stays here — "
                "we never touch your master account."
            </p>
            <label class="field">
                <span>"Starting margin (USDC)"</span>
                <input
                    r#type="number"
                    prop:value=move || margin.get()
                    on:input=move |ev| margin.set(event_target_value(&ev))
                />
            </label>
            <button class="cta" on:click=on_create_sub disabled=busy>"Create sub-account & sign"</button>
        }
        .into_any(),
        _ => view! {
            <p class="lead">
                "PUSU charges a " {config::BUILDER_FEE_BPS} " bps builder fee. "
                "The rate you approve is the rate we take — it never rises quietly."
            </p>
            <p class="muted">"You can revoke approval anytime; every pending alert dies with it."</p>
            <button class="cta" on:click=on_approve disabled=busy>
                {move || format!("Approve builder ({} bps)", config::BUILDER_FEE_BPS)}
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
            {step(0, "Connect")}
            {step(1, "Fund")}
            {step(2, "Approve")}
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
fn AlertBuilder(
    /// Market/zaman-dilimi/eşik Dashboard'da yaşıyor — grafik ve form aynı
    /// durumu paylaşsın diye (fiyat yazınca grafikteki çizgi kayar).
    symbol: RwSignal<String>,
    interval: RwSignal<String>,
    price: RwSignal<String>,
    master: String,
    sub: String,
    reload: RwSignal<u32>,
) -> impl IntoView {
    let ctype = RwSignal::new("candle".to_string());
    let dir = RwSignal::new("above".to_string());
    let side = RwSignal::new("long".to_string());
    let size = RwSignal::new(String::new());
    let entry = RwSignal::new("market".to_string());
    let limit_price = RwSignal::new(String::new());
    let tps = RwSignal::new(Vec::<LegRow>::new());
    let sls = RwSignal::new(Vec::<LegRow>::new());
    let leg_id = RwSignal::new(0usize);
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
            take_profits: collect_legs(&tps.get()),
            stops: collect_legs(&sls.get()),
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
            let result = alert::submit(alert, &master, &sub).await;
            let text = match &result {
                Ok(Placed::OnChain) => (true, "Set on-chain — the exchange holds it.".to_string()),
                Ok(Placed::Watched) => (true, "Set — the watcher is watching it.".to_string()),
                Err(e) => (false, e.clone()),
            };
            // Watched başarıyla kaydedildiyse liste yenilensin. OnChain store'a
            // girmediği için listede görünmez — yenilemeye gerek yok.
            if matches!(result, Ok(Placed::Watched)) {
                reload.update(|n| *n += 1);
            }
            toast(text.0, text.1);
            busy.set(false);
        });
    };

    // Rozet Alert::execution()'ı yansıtıyor: iptal koşulu olan alarm borsaya
    // bırakılamaz (trigger kendini iptal edemez), mum kapanışı da zincirde yok.
    let badge = move || {
        if inv_on.get() || ctype.get() == "candle" {
            ("badge watch", "⚡ Watcher runs it")
        } else {
            ("badge chain", "🔒 Exchange runs it")
        }
    };

    view! {
        <section class="ticket">
            <div class="cardhead">
                <h2>"Set alert"</h2>
                <span class=move || badge().0>{move || badge().1}</span>
            </div>

            <div class="row">
                <label class="field">
                    <span>"Trigger"</span>
                    <select
                        prop:value=move || ctype.get()
                        on:change=move |ev| ctype.set(event_target_value(&ev))
                    >
                        <option value="candle">"Candle close"</option>
                        <option value="mark">"Mark price"</option>
                    </select>
                </label>
                {move || {
                    (ctype.get() == "candle").then(|| view! {
                        <label class="field">
                            <span>"Timeframe"</span>
                            <select
                                prop:value=move || interval.get()
                                on:change=move |ev| interval.set(event_target_value(&ev))
                            >
                                <option value="15m">"15 minutes"</option>
                                <option value="1h">"Hourly"</option>
                                <option value="4h">"4-hour"</option>
                                <option value="1d">"Daily"</option>
                            </select>
                        </label>
                    })
                }}
            </div>

            <div class="row">
                <label class="field">
                    <span>"Direction"</span>
                    <select
                        prop:value=move || dir.get()
                        on:change=move |ev| dir.set(event_target_value(&ev))
                    >
                        <option value="above">"Crosses above"</option>
                        <option value="below">"Drops below"</option>
                    </select>
                </label>
                <label class="field">
                    <span>"Trigger price"</span>
                    <input
                        r#type="number"
                        prop:value=move || price.get()
                        on:input=move |ev| price.set(event_target_value(&ev))
                    />
                </label>
            </div>

            <div class="row">
                <label class="field">
                    <span>"Trade"</span>
                    <select
                        prop:value=move || side.get()
                        on:change=move |ev| side.set(event_target_value(&ev))
                    >
                        <option value="long">"Long"</option>
                        <option value="short">"Short"</option>
                    </select>
                </label>
                <label class="field">
                    <span>"Size"</span>
                    <input
                        r#type="number"
                        prop:value=move || size.get()
                        on:input=move |ev| size.set(event_target_value(&ev))
                    />
                </label>
            </div>

            <div class="row">
                <label class="field">
                    <span>"Entry"</span>
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
                            <span>"Limit price"</span>
                            <input
                                r#type="number"
                                prop:value=move || limit_price.get()
                                on:input=move |ev| limit_price.set(event_target_value(&ev))
                            />
                        </label>
                    })
                }}
            </div>

            <LegList
                title="Targets — take profit (optional)"
                hint="No profit target. Add one, or stagger it (50% @ X, 50% @ Y)."
                legs=tps
                next_id=leg_id
            />
            <LegList
                title="Stops — stop loss (optional)"
                hint="No stop. Add one, or stagger it."
                legs=sls
                next_id=leg_id
            />

            <label class="check">
                <input
                    r#type="checkbox"
                    prop:checked=move || inv_on.get()
                    on:change=move |ev| inv_on.set(event_target_checked(&ev))
                />
                <span>"Cancel if the setup breaks"</span>
            </label>

            {move || {
                inv_on.get().then(|| view! {
                    <div class="row">
                        <label class="field">
                            <span>"Cancel direction"</span>
                            <select
                                prop:value=move || inv_dir.get()
                                on:change=move |ev| inv_dir.set(event_target_value(&ev))
                            >
                                <option value="below">"Drops below"</option>
                                <option value="above">"Crosses above"</option>
                            </select>
                        </label>
                        <label class="field">
                            <span>"Cancel price"</span>
                            <input
                                r#type="number"
                                prop:value=move || inv_price.get()
                                on:input=move |ev| inv_price.set(event_target_value(&ev))
                            />
                        </label>
                    </div>
                    <p class="muted">
                        "The cancel condition watches the mark price — the moment your setup dies the alert drops "
                        "and no trade enters. This alert can't live on the exchange; PUSU watches it."
                    </p>
                })
            }}

            <p class="muted">
                {format!("{} bps builder fee on entry. Protective orders (stop/target) are free.", config::BUILDER_FEE_BPS)}
            </p>

            <button class="cta" on:click=submit disabled=busy>"Set alert & sign"</button>

            {move || {
                notice
                    .get()
                    .map(|n| view! { <p class=if n.ok { "notice ok" } else { "notice err" }>{n.text}</p> })
            }}
        </section>
    }
}

/// Formdaki bir çıkış kademesinin ham girdisi: fiyat + yüzde sinyalleri.
///
/// `id` `<For>`'un kararlı anahtarı — kademe silinince doğru satır kalksın.
/// RwSignal Copy olduğu için tüm yapı Copy; kapanışlara serbestçe taşınıyor.
#[derive(Clone, Copy)]
struct LegRow {
    id: usize,
    price: RwSignal<String>,
    pct: RwSignal<String>,
}

impl LegRow {
    fn new(id: usize) -> Self {
        // Yüzde %100 ön-dolu: tek kademeli (basit) hal ekstra tık istemesin.
        Self {
            id,
            price: RwSignal::new(String::new()),
            pct: RwSignal::new("100".to_string()),
        }
    }
}

/// Kademe satırlarını (fiyat, yüzde) çiftlerine çevir; fiyatsız (yarım
/// doldurulmuş) satırları at ki eksik kademe göndermeyi engellemesin.
fn collect_legs(rows: &[LegRow]) -> Vec<(f64, f64)> {
    rows.iter()
        .map(|r| (num(&r.price.get()), num(&r.pct.get())))
        .filter(|&(price, _)| price > 0.0)
        .collect()
}

/// Kademeli çıkış girişi: fiyat + yüzde satırları, ekle/çıkar.
///
/// Tek %100 kademe basit OCO'ya, fazlası kademeli emirlere derleniyor — ayrımı
/// derleyici yapıyor, kullanıcı yalnızca kaç kademe istediğini söylüyor.
#[component]
fn LegList(
    title: &'static str,
    hint: &'static str,
    legs: RwSignal<Vec<LegRow>>,
    next_id: RwSignal<usize>,
) -> impl IntoView {
    let add = move |_| {
        let id = next_id.get();
        next_id.set(id + 1);
        legs.update(|v| v.push(LegRow::new(id)));
    };

    view! {
        <div class="legs">
            <div class="legs-head">
                <span>{title}</span>
                <button class="ghost" on:click=add>"+ Tier"</button>
            </div>
            <For
                each=move || legs.get()
                key=|r| r.id
                children=move |row| {
                    let (price, pct, id) = (row.price, row.pct, row.id);
                    view! {
                        <div class="legrow">
                            <input
                                class="leg-price"
                                r#type="number"
                                placeholder="price"
                                prop:value=move || price.get()
                                on:input=move |ev| price.set(event_target_value(&ev))
                            />
                            <input
                                class="leg-pct"
                                r#type="number"
                                placeholder="%"
                                prop:value=move || pct.get()
                                on:input=move |ev| pct.set(event_target_value(&ev))
                            />
                            <button
                                class="legx"
                                on:click=move |_| legs.update(|v| v.retain(|x| x.id != id))
                            >
                                "×"
                            </button>
                        </div>
                    }
                }
            />
            {move || legs.get().is_empty().then_some(view! { <p class="muted small">{hint}</p> })}
        </div>
    }
}

/// Doğal dil kutusu: bir cümleyi taslağa çevirir, önizler, imzalatır.
///
/// Formu bypass etmez — onunla aynı [`alert::submit`] hattına bağlanır, yalnızca
/// girdisi cümledir. Ürünün sözü gereği ("imzalamadan önce hangisi olduğunu
/// görürsün") canlı önizleme koşulu, işlemi, **yürütme katmanını** ve her sessiz
/// varsayımı gösterir.
#[component]
fn NlCompose(master: String, sub: String, reload: RwSignal<u32>) -> impl IntoView {
    let text = RwSignal::new(String::new());
    let parsed = RwSignal::new(None::<pusu_nl::Parsed>);
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let notice = RwSignal::new(None::<Notice>);

    let on_input = move |ev| {
        let t = event_target_value(&ev);
        text.set(t.clone());
        if t.trim().is_empty() {
            parsed.set(None);
            error.set(None);
            return;
        }
        match pusu_nl::parse(&t) {
            Ok(p) => {
                parsed.set(Some(p));
                error.set(None);
            }
            Err(e) => {
                parsed.set(None);
                error.set(Some(e.to_string()));
            }
        }
    };

    let submit = move |_| {
        let Some(p) = parsed.get() else { return };
        busy.set(true);
        notice.set(None);
        let (m, s) = (master.clone(), sub.clone());
        let alert = alert::from_draft(p.draft, &m, &s);
        spawn_local(async move {
            let result = alert::submit(alert, &m, &s).await;
            let (ok, msg) = match &result {
                Ok(Placed::OnChain) => (true, "Set on-chain — the exchange holds it.".to_string()),
                Ok(Placed::Watched) => (true, "Set — the watcher is watching it.".to_string()),
                Err(e) => (false, e.clone()),
            };
            if matches!(result, Ok(Placed::Watched)) {
                reload.update(|n| *n += 1);
            }
            // Başarı → toast (form temiz kalsın); hata → kutu içinde inline.
            if ok {
                notice.set(None);
                toast(true, msg);
            } else {
                notice.set(Some(Notice { ok, text: msg }));
            }
            busy.set(false);
        });
    };

    view! {
        <section class="card nl-box">
            <div class="cardhead">
                <h2>"Set it with a sentence"</h2>
                <span class="badge muted">"experimental"</span>
            </div>
            <p class="muted small">
                "Type what you want: "
                <em>"“if the 1h candle closes above $90k, long 0.5 BTC, SL $88k”"</em>
            </p>
            <textarea
                class="nl-input"
                rows="2"
                placeholder="if the 1h candle closes above $90k, long 0.5 BTC, SL $88k"
                prop:value=move || text.get()
                on:input=on_input
            ></textarea>

            {move || error.get().map(|e| view! { <p class="notice err">{e}</p> })}

            {move || {
                parsed
                    .get()
                    .map(|p| {
                        let cond = describe_condition(&p.draft.condition);
                        let act = describe_action(&p.draft.action);
                        let exits = describe_exits(&p.draft.action);
                        let inv = p.draft.invalidate.as_ref().map(describe_condition);
                        let (badge_cls, badge_txt) = match p.draft.classify() {
                            Execution::OnChain => ("badge chain", "🔒 Exchange runs it"),
                            Execution::Watched { .. } => ("badge watch", "⚡ Watcher runs it"),
                        };
                        let notes = p
                            .notes
                            .iter()
                            .map(|n| {
                                let cls = format!("nl-note {}", n.kind());
                                view! { <li class=cls>{n.text().to_string()}</li> }
                            })
                            .collect::<Vec<_>>();
                        view! {
                            <div class="nl-preview">
                                <div class="nl-line">
                                    <span class="nl-cond">{cond}</span>
                                    <span class=badge_cls>{badge_txt}</span>
                                </div>
                                <div class="nl-act">
                                    {act}
                                    {exits.map(|e| format!(" · {e}"))}
                                </div>
                                {inv.map(|i| view! { <div class="nl-inv">"cancel · " {i}</div> })}
                                <ul class="nl-notes">{notes}</ul>
                            </div>
                        }
                    })
            }}

            <button
                class="cta"
                on:click=submit
                disabled=move || busy.get() || parsed.get().is_none()
            >
                "Set & sign"
            </button>

            {move || {
                notice
                    .get()
                    .map(|n| view! { <p class=if n.ok { "notice ok" } else { "notice err" }>{n.text}</p> })
            }}
        </section>
    }
}

/// Üst bar: marka + market + canlı fiyat + zaman-dilimi sekmeleri + hesap.
#[component]
fn TopBar(
    symbol: RwSignal<String>,
    interval: RwSignal<String>,
    mark: RwSignal<Option<f64>>,
    markets: RwSignal<Vec<String>>,
    owner: String,
    sub: String,
) -> impl IntoView {
    let ivs = ["15m", "1h", "4h", "1d"];
    view! {
        <header class="topbar">
            <div class="tb-brand">
                <svg class="tb-mark" viewBox="0 0 40 40" aria-hidden="true">
                    <line x1="2.5" y1="20" x2="37.5" y2="20" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>
                    <circle cx="20" cy="20" r="12" fill="none" stroke="currentColor" stroke-width="2"/>
                    <circle cx="20" cy="20" r="3.4" fill="currentColor"/>
                    <line x1="20" y1="3" x2="20" y2="9.5" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>
                    <line x1="20" y1="30.5" x2="20" y2="37" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>
                </svg>
                <span class="tb-word">"PUSU"</span>
            </div>

            <div class="tb-market">
                <select
                    class="tb-symbol"
                    on:change=move |ev| symbol.set(event_target_value(&ev))
                    aria-label="Market"
                >
                    {move || {
                        markets
                            .get()
                            .into_iter()
                            .map(|m| {
                                let label = m.clone();
                                let sel = m.clone();
                                view! {
                                    <option value=m selected=move || symbol.get() == sel>
                                        {label}
                                    </option>
                                }
                            })
                            .collect::<Vec<_>>()
                    }}
                </select>
                <span class="tb-price">
                    {move || mark.get().map_or("—".to_string(), |m| format!("${}", fmt_money(m)))}
                </span>
            </div>

            <div class="tb-ivs">
                {ivs
                    .into_iter()
                    .map(|iv| {
                        let for_class = iv.to_string();
                        let for_click = iv.to_string();
                        view! {
                            <button
                                class=move || if interval.get() == for_class { "tb-iv on" } else { "tb-iv" }
                                on:click=move |_| interval.set(for_click.clone())
                            >
                                {iv}
                            </button>
                        }
                    })
                    .collect::<Vec<_>>()}
            </div>

            <div class="tb-acct">
                <NotificationBell owner=owner />
                <span class="tb-live"><i></i>"live"</span>
                <span class="tb-key">{short(&sub)}</span>
            </div>
        </header>
    }
}

/// Önizleme (demo) modunda zili canlandıran örnek bildirimler.
fn demo_notifications() -> Vec<serde_json::Value> {
    let now = js_sys::Date::now() as u64;
    vec![
        serde_json::json!({
            "id": 2, "read": false, "created_at_ms": now - 4 * 60_000,
            "body": { "symbol": "SOL-USD", "message": "SOL-USD · mark > 200" }
        }),
        serde_json::json!({
            "id": 1, "read": true, "created_at_ms": now - 140 * 60_000,
            "body": { "symbol": "ETH-USD", "message": "ETH-USD · hourly close < 3000" }
        }),
    ]
}

/// Üst bardaki bildirim zili — okunmamış rozeti + açılır liste.
///
/// ~10 sn'de bir `/notifications` yoklar. Açılınca okunmamışları okundu
/// işaretler (optimistik + sunucuya POST). Demo modunda örneklerle çalışır.
#[component]
fn NotificationBell(owner: String) -> impl IntoView {
    let items = RwSignal::new(Vec::<serde_json::Value>::new());
    let unread = RwSignal::new(0u64);
    let open = RwSignal::new(false);

    let fetch = {
        let owner = owner.clone();
        move || {
            if is_demo() {
                items.set(demo_notifications());
                unread.set(1);
                return;
            }
            let owner = owner.clone();
            spawn_local(async move {
                if let Ok((list, u)) = api::list_notifications(&owner).await {
                    items.set(list);
                    unread.set(u);
                }
            });
        }
    };
    fetch();
    let ah = set_interval_with_handle(fetch, std::time::Duration::from_millis(10_000)).ok();
    on_cleanup(move || {
        if let Some(h) = ah {
            h.clear();
        }
    });

    // E-posta opt-in (ek kanal). Ayarlıysa prefill.
    let email = RwSignal::new(String::new());
    let saving = RwSignal::new(false);
    if !is_demo() {
        let owner = owner.clone();
        spawn_local(async move {
            if let Ok(Some(e)) = api::get_contact_email(&owner).await {
                email.set(e);
            }
        });
    }
    let save_email = {
        let owner = owner.clone();
        move |_| {
            let addr = email.get().trim().to_string();
            if !addr.contains('@') {
                toast(false, "Enter a valid email.");
                return;
            }
            if is_demo() {
                toast(true, "Email saved (preview — nothing sent).");
                return;
            }
            saving.set(true);
            let owner = owner.clone();
            spawn_local(async move {
                match api::set_contact_email(&owner, &addr).await {
                    Ok(()) => toast(true, "Email saved — alerts will also reach your inbox."),
                    Err(e) => toast(false, e),
                }
                saving.set(false);
            });
        }
    };

    let toggle = {
        let owner = owner.clone();
        move |_| {
            let now_open = !open.get();
            open.set(now_open);
            // Açılışta okunmamışları okundu say (optimistik; sunucuya da bildir).
            if now_open && unread.get() > 0 {
                unread.set(0);
                if !is_demo() {
                    let owner = owner.clone();
                    spawn_local(async move {
                        let _ = api::mark_notifications_read(&owner).await;
                    });
                }
            }
        }
    };

    view! {
        <div class="notif" class:open=move || open.get()>
            <button class="notif-bell" on:click=toggle aria-label="Notifications">
                <svg viewBox="0 0 24 24" width="18" height="18" fill="none"
                    stroke="currentColor" stroke-width="1.8"
                    stroke-linecap="round" stroke-linejoin="round">
                    <path d="M18 8a6 6 0 0 0-12 0c0 7-3 9-3 9h18s-3-2-3-9" />
                    <path d="M13.7 21a2 2 0 0 1-3.4 0" />
                </svg>
                {move || {
                    (unread.get() > 0)
                        .then(|| view! { <span class="notif-badge">{move || unread.get().min(99)}</span> })
                }}
            </button>
            {move || {
                open.get()
                    .then(|| {
                        let list = items.get();
                        let body = if list.is_empty() {
                            view! { <div class="notif-empty">"No notifications yet."</div> }.into_any()
                        } else {
                            let rows = list
                                .into_iter()
                                .map(|n| {
                                    let msg = n["body"]["message"].as_str().unwrap_or_default().to_string();
                                    let sym = n["body"]["symbol"].as_str().unwrap_or_default().to_string();
                                    let ts = n["created_at_ms"].as_u64().unwrap_or(0);
                                    let read = n["read"].as_bool().unwrap_or(false);
                                    let cls = if read { "notif-row" } else { "notif-row unread" };
                                    view! {
                                        <div class=cls>
                                            <span class="notif-msg">{msg}</span>
                                            <span class="notif-meta">{sym}" · fired "{rel_time(ts)}</span>
                                        </div>
                                    }
                                })
                                .collect::<Vec<_>>();
                            view! { <div class="notif-list">{rows}</div> }.into_any()
                        };
                        view! {
                            <div class="notif-menu">
                                <div class="notif-head">"Notifications"</div>
                                {body}
                                <div class="notif-foot">
                                    <label class="notif-foot-l">"Also get these by email"</label>
                                    <div class="notif-foot-row">
                                        <input
                                            class="notif-email"
                                            type="email"
                                            placeholder="you@email.com"
                                            prop:value=move || email.get()
                                            on:input=move |ev| email.set(event_target_value(&ev))
                                        />
                                        <button
                                            class="notif-save"
                                            on:click=save_email.clone()
                                            disabled=saving
                                        >
                                            "Save"
                                        </button>
                                    </div>
                                </div>
                            </div>
                        }
                    })
            }}
        </div>
    }
}

/// Terminal ekranı: üst bar + [büyük grafik + alarm listesi | emir ticket'ı].
///
/// Market/zaman-dilimi/eşik durumu burada yaşıyor ve hem grafiğe hem forma
/// veriliyor — kullanıcı eşik fiyatı yazarken grafikteki çizgi canlı kayar.
#[component]
fn Dashboard(master: String, sub: String) -> impl IntoView {
    // Toast merkezi: alt bileşenler (AlertBuilder/NlCompose) buraya sonuç iter.
    provide_context(ToastHub(RwSignal::new(Vec::new())));
    if is_demo() {
        toast(true, "Preview mode — no wallet connected, nothing is sent.");
    }

    let reload = RwSignal::new(0u32);
    let symbol = RwSignal::new("BTC-USD".to_string());
    let interval = RwSignal::new("1h".to_string());
    let price = RwSignal::new(String::new());
    let mark = RwSignal::new(None::<f64>);
    // BULK pariteleri (exchangeInfo) — market seçici. Yüklenene dek varsayılan.
    let markets = RwSignal::new(vec!["BTC-USD".to_string()]);
    spawn_local(async move {
        if let Ok(ms) = crate::feed::markets().await {
            if !ms.is_empty() {
                markets.set(ms);
            }
        }
    });

    // Üst bardaki canlı fiyat.
    let mh = set_interval_with_handle(
        move || {
            let s = symbol.get_untracked();
            spawn_local(async move {
                if let Ok(m) = crate::feed::mark(&s).await {
                    mark.set(Some(m));
                }
            });
        },
        std::time::Duration::from_millis(2500),
    )
    .ok();
    on_cleanup(move || {
        if let Some(h) = mh {
            h.clear();
        }
    });

    let owner = master.clone();
    let trigger = Signal::derive(move || num(&price.get()));
    // Grafik motoru handle'ı (setData / eşik çizgisi / indikatör çağrıları).
    let chart_handle = ChartHandle::new();

    view! {
        <div class="terminal">
            <TopBar symbol interval mark markets owner=master.clone() sub=sub.clone() />
            <div class="term-body">
                <main class="term-main">
                    <div class="term-chart">
                        <CandleChart symbol interval trigger=trigger handle=chart_handle.clone() />
                    </div>
                    <div class="term-alerts">
                        <AlertList owner=owner reload=reload mark sel_symbol=symbol />
                    </div>
                </main>
                <aside class="term-panel">
                    <AccountPanel sub=sub.clone() symbol />
                    <NlCompose master=master.clone() sub=sub.clone() reload=reload />
                    <AlertBuilder symbol interval price master=master sub=sub reload=reload />
                </aside>
            </div>
            <ToastHost />
        </div>
    }
}

/// Kullanıcının alarmları + durumları. `reload` artınca yeniden çeker.
///
/// `mark`/`sel_symbol` seçili paritenin canlı fiyatı — eşleşen alarm satırında
/// "eşiğe ne kadar kaldı" mesafesini reaktif göstermek için.
#[component]
fn AlertList(
    owner: String,
    reload: RwSignal<u32>,
    #[prop(into)] mark: Signal<Option<f64>>,
    #[prop(into)] sel_symbol: Signal<String>,
) -> impl IntoView {
    let alerts = RwSignal::new(Vec::<Alert>::new());
    let loading = RwSignal::new(true);
    let error = RwSignal::new(None::<String>);

    let ef_owner = owner.clone();
    Effect::new(move |_| {
        reload.get(); // bağımlılık: sayaç artınca yeniden çek
                      // Önizleme modunda backend yok — örnek alarmlarla arayüzü canlandır.
        if is_demo() {
            alerts.set(demo_alerts());
            loading.set(false);
            return;
        }
        let owner = ef_owner.clone();
        loading.set(true);
        error.set(None);
        spawn_local(async move {
            match api::list_alerts(&owner).await {
                Ok(list) => alerts.set(list),
                Err(e) => error.set(Some(e)),
            }
            loading.set(false);
        });
    });

    let refresh = move |_| reload.update(|n| *n += 1);

    view! {
        <section class="card">
            <div class="cardhead">
                <h2>"My alerts"</h2>
                <button class="ghost" on:click=refresh>"Refresh"</button>
            </div>

            {move || {
                if loading.get() {
                    view! {
                        <div class="alist">
                            <div class="skel-row">
                                <div class="skel w-lg"></div>
                                <div class="skel w-md"></div>
                            </div>
                            <div class="skel-row">
                                <div class="skel w-lg"></div>
                                <div class="skel w-sm"></div>
                            </div>
                        </div>
                    }
                    .into_any()
                } else if let Some(e) = error.get() {
                    view! { <p class="notice err">{e}</p> }.into_any()
                } else if alerts.get().is_empty() {
                    view! {
                        <div class="empty">
                            <span class="empty-title">"No alerts yet"</span>
                            <span class="empty-sub">
                                "Set your first one on the right — the watcher takes it from there."
                            </span>
                        </div>
                    }
                    .into_any()
                } else {
                    let rows = alerts
                        .get()
                        .into_iter()
                        .map(|a| {
                            view! {
                                <AlertRow
                                    alert=a
                                    owner=owner.clone()
                                    reload=reload
                                    mark
                                    sel_symbol
                                />
                            }
                        })
                        .collect::<Vec<_>>();
                    view! { <ul class="alist">{rows}</ul> }.into_any()
                }
            }}

            <p class="muted">
                "🔒 Alerts that live on the exchange aren't listed here — the exchange holds them, "
                "so they run even if our server dies."
            </p>
        </section>
    }
}

/// Tek bir alarm satırı: koşul + işlem + durum rozeti + aksiyon.
///
/// Aksiyon durumdan sabit: beklemedeki alarm iptal edilebilir, sonlanmış alarm
/// listeden kaldırılabilir, defterde bekleyen (working) alarma dokunulmaz —
/// orada borsada canlı bir emir var, iptalini watcher yönetiyor.
#[component]
fn AlertRow(
    alert: Alert,
    owner: String,
    reload: RwSignal<u32>,
    #[prop(into)] mark: Signal<Option<f64>>,
    #[prop(into)] sel_symbol: Signal<String>,
) -> impl IntoView {
    let state = alert.state;
    // Working alarmın iptali watcher'a bırakılıyor; istek gitmişse "ediliyor".
    let cancelling = state == AlertState::Working && alert.cancel_requested;
    let (pill_cls, pill_txt) = if cancelling {
        ("pill warn", "Cancelling…")
    } else {
        state_pill(state)
    };
    let cond = describe_condition(&alert.condition);

    // Alt satır: işlem + çıkış kademeleri + iptal koşulu tek metinde.
    let mut sub = describe_action(&alert.action);
    if let Some(x) = describe_exits(&alert.action) {
        sub = format!("{sub} · {x}");
    }
    if alert.invalidate.is_some() {
        sub = format!("{sub} · cancels if setup breaks");
    }

    let armed = rel_time(alert.armed_at_ms);
    let title = format!("{cond}\n{sub}\narmed {armed}");

    // Eşiğe kalan mesafe — yalnız beklemedeki alarmda ve seçili parite
    // eşleşiyorsa (mark o paritenin fiyatı). Reaktif: fiyat aktıkça güncellenir.
    let pending = matches!(state, AlertState::Armed | AlertState::Working);
    let target = condition_target(&alert.condition);
    let away = move || {
        if !pending {
            return None;
        }
        let (sym, trig) = target.clone()?;
        if sel_symbol.get() != sym {
            return None;
        }
        mark.get().map(|m| fmt_away(m, trig))
    };

    let id = alert.id.as_str().to_string();
    let busy = RwSignal::new(false);
    let err = RwSignal::new(None::<String>);

    // İptal edilebilir: beklemedeki (armed) ya da henüz iptali istenmemiş
    // defterdeki (working) alarm. Armed yerel, working watcher'a istek bırakır.
    let cancelable = state == AlertState::Armed || (state == AlertState::Working && !cancelling);

    let action = if cancelable {
        let (id, owner) = (id.clone(), owner.clone());
        let on_cancel = move |_| {
            let (id, owner) = (id.clone(), owner.clone());
            busy.set(true);
            err.set(None);
            spawn_local(async move {
                match api::cancel_alert(&id, &owner).await {
                    Ok(()) => reload.update(|n| *n += 1),
                    Err(e) => {
                        err.set(Some(e));
                        busy.set(false);
                    }
                }
            });
        };
        Some(
            view! { <button class="ghost" on:click=on_cancel disabled=busy>"Cancel"</button> }
                .into_any(),
        )
    } else if state.is_terminal() {
        let (id, owner) = (id.clone(), owner.clone());
        let on_delete = move |_| {
            let (id, owner) = (id.clone(), owner.clone());
            busy.set(true);
            err.set(None);
            spawn_local(async move {
                match api::delete_alert(&id, &owner).await {
                    Ok(()) => reload.update(|n| *n += 1),
                    Err(e) => {
                        err.set(Some(e));
                        busy.set(false);
                    }
                }
            });
        };
        Some(
            view! { <button class="ghost" on:click=on_delete disabled=busy>"Remove"</button> }
                .into_any(),
        )
    } else {
        None // working: borsada canlı emir, watcher yönetiyor
    };

    view! {
        <li class="arow" title=title>
            <div class="arow-main">
                <span class="arow-cond">{cond}</span>
                <span class="arow-sub">{sub}</span>
                <div class="arow-meta">
                    <span class="arow-time">{"armed "}{armed}</span>
                    {move || away().map(|a| view! { <span class="arow-away">{a}</span> })}
                </div>
                {move || err.get().map(|e| view! { <span class="arow-err">{e}</span> })}
            </div>
            <div class="arow-side">
                <span class=pill_cls>{pill_txt}</span>
                {action}
            </div>
        </li>
    }
}

/// Fiyatı temiz göster: tam sayıysa ondalık basma.
fn fmt_price(p: f64) -> String {
    if p.fract() == 0.0 {
        format!("{p:.0}")
    } else {
        format!("{p}")
    }
}

/// Üst bar fiyatı: binlik ayraçlı, ≥1000 tam sayı, altı 2 ondalık (ör. `64,026`).
fn fmt_money(p: f64) -> String {
    if p < 1000.0 {
        return format!("{p:.2}");
    }
    let digits = (p.round() as i64).to_string();
    let mut out = String::new();
    let len = digits.len();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn cross_sym(c: Cross) -> &'static str {
    match c {
        Cross::Above => ">",
        Cross::Below => "<",
    }
}

fn describe_condition(c: &Condition) -> String {
    match c {
        Condition::MarkCross {
            symbol,
            cross,
            price,
        } => format!(
            "{} · mark {} {}",
            symbol.as_str(),
            cross_sym(*cross),
            fmt_price(*price)
        ),
        Condition::CandleClose {
            symbol,
            interval,
            cross,
            price,
        } => format!(
            "{} · {} close {} {}",
            symbol.as_str(),
            interval.label(),
            cross_sym(*cross),
            fmt_price(*price)
        ),
        Condition::All(_) => "multi-condition (all must hold)".to_string(),
        Condition::Any(_) => "multi-condition (any must hold)".to_string(),
    }
}

fn describe_action(a: &AlertAction) -> String {
    match a {
        AlertAction::Trade(s) => {
            let entry = match s.entry {
                Entry::Market => "market".to_string(),
                Entry::Limit { price } => format!("limit {}", fmt_price(price)),
            };
            format!("{} {} · {} entry", s.side.label(), s.size, entry)
        }
        AlertAction::Notify => "notify".to_string(),
    }
}

/// Çıkışları tek satırlık özete çevir (önizleme için). Kademe %100 ise yüzdeyi
/// gizle; değilse "(30%)" olarak göster.
fn describe_exits(a: &AlertAction) -> Option<String> {
    let AlertAction::Trade(s) = a else {
        return None;
    };
    let e = s.exits.as_ref()?;
    let legs = |ls: &[pusu_core::ExitLeg]| {
        ls.iter()
            .map(|l| {
                if l.pct == 100.0 {
                    fmt_price(l.price)
                } else {
                    format!("{} ({}%)", fmt_price(l.price), l.pct as i64)
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let tp = (!e.take_profits.is_empty()).then(|| format!("TP {}", legs(&e.take_profits)));
    let sl = (!e.stops.is_empty()).then(|| format!("SL {}", legs(&e.stops)));
    match (tp, sl) {
        (Some(t), Some(s)) => Some(format!("{t} · {s}")),
        (Some(t), None) => Some(t),
        (None, Some(s)) => Some(s),
        (None, None) => None,
    }
}

/// Durum → (CSS sınıfı, kullanıcı metni).
fn state_pill(s: AlertState) -> (&'static str, &'static str) {
    match s {
        AlertState::Armed => ("pill live", "Waiting"),
        AlertState::Working => ("pill live", "Resting on book"),
        AlertState::Fired => ("pill ok", "Entered"),
        AlertState::Missed => ("pill warn", "Missed"),
        AlertState::Cancelled => ("pill muted", "Cancelled"),
        AlertState::Rejected => ("pill err", "Rejected"),
        AlertState::Uncertain => ("pill warn", "Uncertain — check"),
    }
}

/// Alarmın tetik hedefi: (sembol, eşik fiyatı). Çoklu koşulda tek hedef yok.
fn condition_target(c: &Condition) -> Option<(String, f64)> {
    match c {
        Condition::MarkCross { symbol, price, .. }
        | Condition::CandleClose { symbol, price, .. } => {
            Some((symbol.as_str().to_string(), *price))
        }
        _ => None,
    }
}

/// Kuruluştan bu yana geçen süre, kaba bağıl ("3m ago"). Saat kayması olursa
/// (armed > now) "just now".
fn rel_time(armed_ms: u64) -> String {
    let now = js_sys::Date::now() as u64;
    let secs = now.saturating_sub(armed_ms) / 1000;
    if secs < 5 {
        "just now".to_string()
    } else if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// Anlık mark ile eşik arası mesafe: "$430 away · 0.7%".
fn fmt_away(mark: f64, trig: f64) -> String {
    let d = (mark - trig).abs();
    let pct = if trig != 0.0 { d / trig * 100.0 } else { 0.0 };
    format!("${} away · {pct:.1}%", fmt_money(d))
}
