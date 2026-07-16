//! Onboarding sihirbazı (Leptos CSR).
//!
//! Üç adım: cüzdan bağla → sub-account aç (fonla) → builder onayı. Her adım
//! [`crate::onboarding`]'deki akışı çağırıyor; bu dosya yalnızca durumu ve
//! görünümü tutuyor. Fee her yerde açık gösteriliyor — güven hikâyesi (§8).

use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;

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

#[component]
pub fn App() -> impl IntoView {
    let master = RwSignal::new(None::<String>);
    let sub = RwSignal::new(None::<String>);
    let approved = RwSignal::new(false);
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
                        text: "Builder onayı verildi. Hazırsın.".into(),
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

    // Aktif adım: 0 bağla · 1 fonla · 2 onayla · 3 bitti.
    let active = move || {
        if master.get().is_none() {
            0
        } else if sub.get().is_none() {
            1
        } else if !approved.get() {
            2
        } else {
            3
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
        2 => view! {
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
        _ => view! {
            <p class="lead ok">"Hazırsın. Artık alarm kurabilirsin."</p>
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
        <main class="shell">
            <header class="brand">
                <span class="mark">"PUSU"</span>
                <span class="tag">"alarm ki işlemi de yapar"</span>
            </header>

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

            <footer class="accounts">
                {move || master.get().map(|m| view! { <span>"master " {short(&m)}</span> })}
                {move || sub.get().map(|s| view! { <span>"sub " {short(&s)}</span> })}
            </footer>
        </main>
    }
}
