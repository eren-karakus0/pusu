//! Tam-özellikli canlı grafik — **KLineChart** (açık kaynak) ile.
//!
//! Motor built-in indikatörler (MA/EMA/BOLL/MACD/RSI/VOL) + çizim araçları
//! (yatay çizgi, trend, fibonacci, dikdörtgen) sunuyor. Biz BULK verisini
//! besliyor, alarm eşiğini kilitli bir yatay çizgi olarak çiziyoruz. Motor
//! `index.html`'deki `window.pusuChart` köprüsüyle sarılı; buradan wasm'dan
//! çağırıyoruz. Araç çubuğu (indikatör/çizim düğmeleri) burada, Leptos'ta.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use js_sys::{Array, Function, Reflect};
use leptos::html::Div;
use leptos::prelude::*;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::spawn_local;

use crate::feed;
use pusu_core::Kline;

type Handle = Rc<RefCell<Option<JsValue>>>;

/// Grafik motoru handle'ı. Dashboard oluşturur, `CandleChart` doldurur; hem o
/// hem sol çizim rayı ([`DrawTools`]) aynı örneğe çağrı yapar (CSR tek-thread,
/// bu yüzden Rc).
#[derive(Clone)]
pub struct ChartHandle(Handle);

impl ChartHandle {
    pub fn new() -> Self {
        Self(Rc::new(RefCell::new(None)))
    }
}

impl Default for ChartHandle {
    fn default() -> Self {
        Self::new()
    }
}

fn draw_glyph(overlay: &str) -> &'static str {
    match overlay {
        "horizontalStraightLine" => "―",
        "segment" => "╱",
        "fibonacciLine" => "☰",
        "rect" => "▭",
        _ => "•",
    }
}

/// Ana panele binen (mumların üstü) vs alt-panel indikatörleri düğme etiketleri.
const INDICATORS: [&str; 6] = ["MA", "EMA", "BOLL", "MACD", "RSI", "VOL"];
/// (etiket, KLineChart overlay adı).
const TOOLS: [(&str, &str); 4] = [
    ("Horizontal", "horizontalStraightLine"),
    ("Trend", "segment"),
    ("Fib", "fibonacciLine"),
    ("Rect", "rect"),
];

fn bridge() -> Option<JsValue> {
    let w = web_sys::window()?;
    let o = Reflect::get(&w, &JsValue::from_str("pusuChart")).ok()?;
    (!o.is_undefined() && !o.is_null()).then_some(o)
}

fn method(obj: &JsValue, name: &str) -> Option<Function> {
    Reflect::get(obj, &JsValue::from_str(name))
        .ok()?
        .dyn_into::<Function>()
        .ok()
}

/// Köprü metodunu çağır: `pusuChart[name](handle, ...extra)`.
fn call(handle: &Handle, name: &str, extra: &[JsValue]) {
    let Some(h) = handle.borrow().clone() else {
        return;
    };
    let (Some(b), Some(f)) = (bridge(), bridge().and_then(|o| method(&o, name))) else {
        return;
    };
    let args = Array::new();
    args.push(&h);
    for a in extra {
        args.push(a);
    }
    let _ = f.apply(&b, &args);
}

#[component]
pub fn CandleChart(
    #[prop(into)] symbol: Signal<String>,
    #[prop(into)] interval: Signal<String>,
    /// Alarm eşiği (0 = çizme). Reaktif — yazdıkça çizgi kayar.
    #[prop(into)]
    trigger: Signal<f64>,
    /// Paylaşılan handle (Dashboard'dan) — sol çizim rayı da aynısını kullanır.
    handle: ChartHandle,
) -> impl IntoView {
    let candles = RwSignal::new(Vec::<Kline>::new());
    let container = NodeRef::<Div>::new();
    let handle: Handle = handle.0;
    let active_inds = RwSignal::new(Vec::<String>::new());

    // div mount olunca motoru bir kez kur + eldeki veriyi bas.
    {
        let handle = handle.clone();
        Effect::new(move |_| {
            if handle.borrow().is_some() {
                return;
            }
            let Some(div) = container.get() else { return };
            if let Some(b) = bridge() {
                if let Some(init) = method(&b, "init") {
                    if let Ok(h) = init.call1(&b, div.as_ref()) {
                        if !h.is_null() && !h.is_undefined() {
                            *handle.borrow_mut() = Some(h);
                            call(
                                &handle,
                                "setData",
                                &[JsValue::from_str(
                                    &serde_json::to_string(&candles.get_untracked())
                                        .unwrap_or_default(),
                                )],
                            );
                            call(
                                &handle,
                                "setThreshold",
                                &[JsValue::from_f64(trigger.get_untracked())],
                            );
                        }
                    }
                }
            }
        });
    }

    // Sembol/interval değişince (+ ilk yükleme) mumları çek.
    Effect::new(move |_| {
        let (s, i) = (symbol.get(), interval.get());
        spawn_local(async move {
            if let Ok(ks) = feed::klines(&s, &i).await {
                candles.set(ks);
            }
        });
    });

    // ~25 sn periyodik tazeleme.
    let kh = set_interval_with_handle(
        move || {
            let (s, i) = (symbol.get_untracked(), interval.get_untracked());
            spawn_local(async move {
                if let Ok(ks) = feed::klines(&s, &i).await {
                    candles.set(ks);
                }
            });
        },
        Duration::from_millis(25_000),
    )
    .ok();
    on_cleanup(move || {
        if let Some(h) = kh {
            h.clear();
        }
    });

    // Mumlar değişince motora bas.
    {
        let handle = handle.clone();
        Effect::new(move |_| {
            let ks = candles.get();
            if let Ok(json) = serde_json::to_string(&ks) {
                call(&handle, "setData", &[JsValue::from_str(&json)]);
            }
        });
    }

    // Eşik değişince çizgiyi güncelle.
    {
        let handle = handle.clone();
        Effect::new(move |_| {
            let t = trigger.get();
            call(&handle, "setThreshold", &[JsValue::from_f64(t)]);
        });
    }

    // -- araç çubuğu düğmeleri --
    let indicator_btns = INDICATORS
        .into_iter()
        .map(|name| {
            let handle = handle.clone();
            let for_click = name.to_string();
            let for_class = name.to_string();
            view! {
                <button
                    class=move || {
                        if active_inds.get().iter().any(|x| x == &for_class) {
                            "ctool on"
                        } else {
                            "ctool"
                        }
                    }
                    on:click=move |_| {
                        call(&handle, "toggleIndicator", &[JsValue::from_str(&for_click)]);
                        active_inds
                            .update(|v| match v.iter().position(|x| x == &for_click) {
                                Some(i) => {
                                    v.remove(i);
                                }
                                None => v.push(for_click.clone()),
                            });
                    }
                >
                    {name}
                </button>
            }
        })
        .collect::<Vec<_>>();

    view! {
        <div class="chart-wrap">
            <div class="chart-tools">
                <span class="ct-label">"Indicators"</span>
                <span class="ct-group">{indicator_btns}</span>
            </div>
            <div node_ref=container class="chart-kline"></div>
        </div>
    }
}

/// Sol kenar dikey çizim rayı (TradingView tarzı) — grafikle aynı handle'ı kullanır.
#[component]
pub fn DrawTools(handle: ChartHandle) -> impl IntoView {
    let handle = handle.0;
    let tools = TOOLS
        .into_iter()
        .map(|(label, overlay)| {
            let handle = handle.clone();
            let ov = overlay.to_string();
            view! {
                <button
                    class="drawtool"
                    title=label
                    on:click=move |_| call(&handle, "draw", &[JsValue::from_str(&ov)])
                >
                    {draw_glyph(overlay)}
                </button>
            }
        })
        .collect::<Vec<_>>();
    let clear_handle = handle.clone();
    view! {
        <aside class="term-rail">
            {tools}
            <div class="rail-sep"></div>
            <button
                class="drawtool"
                title="Clear drawings"
                on:click=move |_| call(&clear_handle, "clearDraw", &[])
            >
                "✕"
            </button>
        </aside>
    }
}
