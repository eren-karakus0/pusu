//! Alarmı BULK payload'ına derler.
//!
//! Girdi: [`pusu_core::Alert`] — kullanıcının ne istediği.
//! Çıktı: [`Compiled`] — imzalanmaya hazır `OrderItem`'lar + kimin yürüteceği.
//!
//! **Bu crate imzalamaz.** İmza kullanıcının tarayıcısında atılır; sunucuda
//! anahtar yok. Burada üretilenler imzasız payload.
//!
//! # İki şablon, tek sebep
//!
//! Şablonlar staging'de tek tek denenerek bulundu; ikisinin farklı olmasının
//! sebebi keyfi değil:
//!
//! | | Şablon | Market reddedilirse |
//! |---|---|---|
//! | 🔒 [`Compiled::OnChain`] | `trig { actions: [m, rng] }` | `rng` yine kurulur ⚠️ |
//! | ⚡ [`Compiled::Watched`] | `[m, of{p:0, actions:[rng]}]` | `rng` hiç kurulmaz ✅ |
//!
//! OnChain'de `of` kullanılamıyor: parent'ı `trig` olamıyor
//! (`on_fill parent not found`) ve `trig`'in içine de gömülemiyor
//! (`invalid action in trigger order`). Watched'da parent gerçek bir emir
//! olduğu için `of` çalışıyor ve daha güvenli.
//!
//! # Çıkışlar: `rng` mi, `tp`/`st` kademeleri mi
//!
//! Tek stop + tek hedef → **`rng`** (OCO): bir bacak tetiklenince diğeri
//! borsada otomatik iptal oluyor. Atomik ve Faz 1'de uçtan uca doğrulandı.
//!
//! Kademeli çıkış → **`[tp…, st…]`**. `rng` tek çift taşıdığı için sığmıyor.
//! OCO kaybı sorun değil, çünkü staging'de ölçüldü (§8.10): fazla boyutlu
//! koruma emri **kırpılıyor** ve pozisyon kapanınca artakalan koruma emirleri
//! temizleniyor. Yani kademeler birbirini bozmuyor.
//!
//! # ⚠️ `is_buy` tuzağı — burada iki kez ısırıyor
//!
//! `Stop`/`TakeProfit`/`TriggerBasket`'te `is_buy`, keychain'in doc'unda
//! *"true = buy/long side"* diye geçiyor ama **yanlış**: alan **tetik yönü**
//! ("fiyat eşiğin üstündeyken ateşle"), korunan pozisyonun yönü değil.
//! Emrin kendi yönünü borsa pozisyondan türetiyor.
//!
//! Sonuç: aynı long'u korurken `tp` ile `st` **ters** değer istiyor.
//!
//! | Long'u korumak için | `is_buy` | çünkü |
//! |---|---|---|
//! | `tp` | `true` | kâr yukarıda, yukarı tetikler |
//! | `st` | `false` | zarar aşağıda, aşağı tetikler |
//!
//! Yanlış değer **hata vermiyor**: borsa `"resting"` + geçerli bir `oid`
//! döndürüyor, emir hiç var olmuyor. Kullanıcı stop'u olduğunu sanır.
//! `rng` bu tuzağın dışında — iki tarafı birden taşıdığı için `is_buy`'ı
//! pozisyon yönü olarak kullanıyor (Faz 1'de doğrulandı).

use bulk_keychain::{
    Commission, OnFill, Order, OrderItem, OrderType, Pubkey, RangeOco, Stop, TakeProfit,
    TimeInForce, TriggerBasket,
};
use pusu_core::{Alert, AlertAction, Condition, Entry, Execution, Exits, Side, TradeSpec};

/// Derlenmiş alarm: imzalanmaya hazır payload + kimin yürüteceği.
#[derive(Debug, Clone, PartialEq)]
pub enum Compiled {
    /// 🔒 Borsada yaşar. Kullanıcı imzalar, **hemen** gönderilir, borsa tutar.
    /// PUSU'nun sunucusu ölse bile tetiklenir.
    OnChain { items: Vec<OrderItem> },

    /// ⚡ Watcher'da. Kullanıcı imzalar, **biz saklarız**, koşul gerçekleşince
    /// göndeririz. Sunucuda imzalama yetkisi yok — yalnızca hazır blob'u taşırız.
    Watched { items: Vec<OrderItem> },

    /// Sadece bildirim. İmzalanacak bir şey yok, fee yok.
    NotifyOnly,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum CompileError {
    /// Stop/hedef girişin yanlış tarafında — emir dolar dolmaz kendini tetiklerdi.
    #[error("çıkışlar tutarsız: {0} işlemde stop'lar hedeflerin yanlış tarafında")]
    IncoherentExits(&'static str),

    /// Yüzdeler geçersiz (bacak 0..=100 dışında ya da toplam %100'ü aşıyor).
    #[error("çıkış yüzdeleri geçersiz: her kademe 0-100 arası olmalı, toplam %100'ü aşamaz")]
    InvalidPercentages,

    /// `trig { actions: [l, rng] }` limit book'ta beklerken rng'yi hemen kurar,
    /// yani var olmayan pozisyonu korur. v1'de trigger içinde yalnızca market.
    #[error("zincire gömülen alarmda limit giriş desteklenmiyor (v1)")]
    LimitEntryOnChain,

    #[error("geçersiz builder fee: {0} bps (1..=15 olmalı)")]
    InvalidFee(u8),

    #[error("geçersiz pubkey: {0}")]
    BadPubkey(String),
}

/// Alarmı derle.
///
/// `builder`: builder fee'nin yazılacağı hesap (PUSU'nun pubkey'i).
pub fn compile(alert: &Alert, builder: &str) -> Result<Compiled, CompileError> {
    let spec = match &alert.action {
        AlertAction::Notify => return Ok(Compiled::NotifyOnly),
        AlertAction::Trade(spec) => spec,
    };

    validate_exits(spec)?;

    match alert.execution() {
        Execution::OnChain => compile_onchain(&alert.condition, spec, builder),
        Execution::Watched { .. } => compile_watched(spec, builder),
    }
}

/// 🔒 `trig { c, d, tr, actions: [m{builderCode}, çıkışlar…] }`
fn compile_onchain(
    condition: &Condition,
    spec: &TradeSpec,
    builder: &str,
) -> Result<Compiled, CompileError> {
    let Condition::MarkCross {
        symbol,
        cross,
        price,
    } = condition
    else {
        // execution() OnChain dediyse koşul MarkCross'tur; buraya düşmek
        // sınıflandırıcı ile derleyicinin ayrıştığı anlamına gelir.
        unreachable!("OnChain yalnızca MarkCross'tan gelir");
    };

    // Zincirde limit giriş yok: `trig` ateşleyip `l`'yi deftere koyar ama
    // çıkışlar AYNI anda kurulur — yani daha var olmayan bir pozisyonu
    // korurlar. Üstelik dolum takibi de yapamayız (borsa emri tutuyor, biz
    // izlemiyoruz), o yüzden "dolmazsa sor" da çalışmaz. Limit giriş
    // watcher'ın işi.
    if spec.entry.is_limit() {
        return Err(CompileError::LimitEntryOnChain);
    }

    let mut actions = vec![entry_order(spec, builder)?];
    if let Some(e) = &spec.exits {
        // `trig` kademeyi kabul ediyor: staging'de [m, tp, tp, st, st] ile
        // doğrulandı — dördü de reduce-only olarak dinlenmeye geçti.
        actions.extend(exit_items(spec, e));
    }

    Ok(Compiled::OnChain {
        items: vec![OrderItem::TriggerBasket(TriggerBasket {
            symbol: symbol.to_string(),
            // ⚠️ İsim tuzağı: keychain buna `is_buy` diyor ama wire'daki karşılığı
            // `d` ve anlamı "eşiğin üstü mü?" — işlem yönüyle ilgisi yok.
            is_buy: cross.is_above(),
            trigger_price: *price,
            actions,
            iso: false,
        })],
    })
}

/// ⚡ `[m{builderCode}, of{p:0, actions:[çıkışlar…]}]`
fn compile_watched(spec: &TradeSpec, builder: &str) -> Result<Compiled, CompileError> {
    let mut items = vec![entry_order(spec, builder)?];
    if let Some(e) = &spec.exits {
        // Parent = index 0 (market emri). Gerçek bir emir olduğu için `of`
        // burada çalışıyor: market reddedilirse çıkışlar hiç kurulmuyor.
        items.push(OrderItem::OnFill(OnFill {
            p: 0,
            actions: exit_items(spec, e),
        }));
    }
    Ok(Compiled::Watched { items })
}

/// Giriş emri. Market ya da limit (retest).
///
/// Builder fee **yalnızca burada** — çıkışlara (`tp`/`st`/`rng`) hiç
/// iliştirilmiyor. Koruma emirleri ücretsiz; bkz. PLAN §4.
fn entry_order(spec: &TradeSpec, builder: &str) -> Result<OrderItem, CompileError> {
    let to = Pubkey::from_base58(builder).map_err(|_| CompileError::BadPubkey(builder.into()))?;
    let commission = Commission::new(to, pusu_core::BUILDER_FEE_BPS)
        .map_err(|_| CompileError::InvalidFee(pusu_core::BUILDER_FEE_BPS))?;

    let (price, order_type) = match spec.entry {
        Entry::Market => (0.0, OrderType::market()),
        // GTC: borsada süre sınırlı emir yok (TimeInForce yalnızca Gtc/Ioc/Alo,
        // GTD yok). Dolmayan emrin süresini watcher yönetiyor — ön-imzalı `cx`
        // ile. Bkz. §8.9.
        Entry::Limit { price } => (price, OrderType::limit(TimeInForce::Gtc)),
    };

    Ok(OrderItem::Order(Order {
        symbol: spec.symbol.to_string(),
        is_buy: spec.side.is_buy(),
        price,
        size: spec.size,
        reduce_only: false,
        iso: false,
        order_type,
        client_id: None,
        commission: Some(commission),
    }))
}

/// Çıkışları borsa aksiyonlarına çevirir.
///
/// Tek stop + tek hedef → `rng` (OCO, atomik, Faz 1'de doğrulanmış).
/// Kademeli → ayrı `tp`/`st` emirleri.
fn exit_items(spec: &TradeSpec, e: &Exits) -> Vec<OrderItem> {
    if e.is_simple() {
        vec![collar(spec, e.stops[0].price, e.take_profits[0].price)]
    } else {
        ladder(spec, e)
    }
}

/// Stop + hedefi tek OCO collar'ına çevirir: bir bacak tetiklenince diğeri iptal.
///
/// `collar_min`/`collar_max` fiyat sırasına göre; hangisinin stop hangisinin
/// hedef olduğu yöne bağlı. Long'da stop altta, short'ta üstte.
///
/// `rng`'de `is_buy` gerçekten pozisyonun yönü — `tp`/`st`'deki tetik yönü
/// tuzağı buraya uygulanmıyor, çünkü `rng` iki tarafı birden taşıyor.
fn collar(spec: &TradeSpec, stop: f64, take_profit: f64) -> OrderItem {
    let (min, max) = match spec.side {
        Side::Buy => (stop, take_profit),
        Side::Sell => (take_profit, stop),
    };
    OrderItem::RangeOco(RangeOco {
        symbol: spec.symbol.to_string(),
        is_buy: spec.side.is_buy(),
        size: spec.size,
        collar_min: min,
        collar_max: max,
        // NaN = market-style tetikleme (limit fiyat yok).
        limit_min: f64::NAN,
        limit_max: f64::NAN,
        iso: false,
    })
}

/// Kademeli çıkış: her hedef ve stop için ayrı emir.
///
/// Boyutlar orijinal pozisyonun yüzdesi olarak sabitleniyor; TP1 dolunca
/// kalanları küçültmeye gerek yok, çünkü borsa fazla boyutlu korumayı
/// kırpıyor (§8.10'da ölçüldü).
///
/// ⚠️ `is_buy` burada **tetik yönü** — modül dokümanındaki tuzağa bak.
/// Long'da kâr yukarıda (`tp` → `true`), zarar aşağıda (`st` → `false`).
/// Ters verirsek borsa `"resting"` deyip emri hiç oluşturmuyor.
fn ladder(spec: &TradeSpec, e: &Exits) -> Vec<OrderItem> {
    let yukari = spec.side.is_buy();

    let tps = e.take_profits.iter().map(|leg| {
        OrderItem::TakeProfit(TakeProfit {
            symbol: spec.symbol.to_string(),
            is_buy: yukari,
            size: leg.size_of(spec.size),
            trigger_price: leg.price,
            limit_price: f64::NAN,
            iso: false,
        })
    });

    let sts = e.stops.iter().map(|leg| {
        OrderItem::Stop(Stop {
            symbol: spec.symbol.to_string(),
            is_buy: !yukari,
            size: leg.size_of(spec.size),
            trigger_price: leg.price,
            limit_price: f64::NAN,
            iso: false,
        })
    });

    tps.chain(sts).collect()
}

fn validate_exits(spec: &TradeSpec) -> Result<(), CompileError> {
    let Some(e) = &spec.exits else {
        return Ok(());
    };
    // Borsa fazlasını kırpıyor ama toplamı %100'ü aşan bir ladder'ı sessizce
    // kabul etmek yanlış: kullanıcı son kademelerin çalışacağını sanır.
    if !e.pcts_ok() {
        return Err(CompileError::InvalidPercentages);
    }
    // Girişi bilmiyoruz (market emri), o yüzden girişe göre değil kendi
    // içinde tutarlılığı kontrol ediyoruz: long'da her stop her hedefin altında.
    if !e.is_coherent(spec.side) {
        return Err(CompileError::IncoherentExits(spec.side.label()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pusu_core::{AlertId, AlertState, Cross, ExitLeg, Interval, Symbol};

    const BUILDER: &str = "AdjWd4DCeKC3P4QjRaP5BmmcPMs1YaQ8kRjPqpnbnqdz";

    fn spec(side: Side, exits: Option<Exits>) -> TradeSpec {
        TradeSpec {
            symbol: Symbol::new("BTC-USD"),
            side,
            size: 0.01,
            entry: Entry::Market,
            exits,
        }
    }

    fn alert(condition: Condition, action: AlertAction) -> Alert {
        Alert {
            id: AlertId::new("a1"),
            owner: "master".into(),
            account: "sub".into(),
            condition,
            invalidate: None,
            action,
            state: AlertState::Armed,
            armed_at_ms: 0,
            entry_oid: None,
            fill_deadline_ms: None,
        }
    }

    fn mark_cross(cross: Cross) -> Condition {
        Condition::MarkCross {
            symbol: Symbol::new("BTC-USD"),
            cross,
            price: 88_400.0,
        }
    }

    fn candle_close() -> Condition {
        Condition::CandleClose {
            symbol: Symbol::new("BTC-USD"),
            interval: Interval::H1,
            cross: Cross::Above,
            price: 90_000.0,
        }
    }

    #[test]
    fn mark_cross_trigger_baskete_derlenir() {
        let a = alert(
            mark_cross(Cross::Below),
            AlertAction::Trade(spec(Side::Buy, None)),
        );
        let Compiled::OnChain { items } = compile(&a, BUILDER).unwrap() else {
            panic!("OnChain bekleniyordu");
        };
        assert_eq!(items.len(), 1);
        let OrderItem::TriggerBasket(t) = &items[0] else {
            panic!("trig bekleniyordu");
        };
        assert_eq!(t.trigger_price, 88_400.0);
        assert_eq!(t.actions.len(), 1); // bracket yok → sadece market
    }

    #[test]
    fn cross_yonu_wire_alanina_dogru_esleniyor() {
        // `d` = "eşiğin üstü mü", işlem yönü DEĞİL. Karıştırılırsa alarm
        // ters yönde ateşler — sessiz ve yıkıcı.
        let asagi = alert(
            mark_cross(Cross::Below),
            AlertAction::Trade(spec(Side::Buy, None)),
        );
        let Compiled::OnChain { items } = compile(&asagi, BUILDER).unwrap() else {
            panic!()
        };
        let OrderItem::TriggerBasket(t) = &items[0] else {
            panic!()
        };
        // Cross::Below → d=false, giriş Buy olmasına RAĞMEN.
        assert!(!t.is_buy, "Cross::Below → d=false olmalı");

        let yukari = alert(
            mark_cross(Cross::Above),
            AlertAction::Trade(spec(Side::Sell, None)),
        );
        let Compiled::OnChain { items } = compile(&yukari, BUILDER).unwrap() else {
            panic!()
        };
        let OrderItem::TriggerBasket(t) = &items[0] else {
            panic!()
        };
        // Cross::Above → d=true, giriş Sell olmasına RAĞMEN.
        assert!(t.is_buy, "Cross::Above → d=true olmalı");
    }

    #[test]
    fn onchain_bracketi_trigger_icine_rng_olarak_koyar() {
        // of kullanılamıyor: parent'ı trig olamıyor, trig'e de gömülemiyor.
        let a = alert(
            mark_cross(Cross::Below),
            AlertAction::Trade(spec(Side::Buy, Some(Exits::simple(87_200.0, 94_000.0)))),
        );
        let Compiled::OnChain { items } = compile(&a, BUILDER).unwrap() else {
            panic!()
        };
        let OrderItem::TriggerBasket(t) = &items[0] else {
            panic!()
        };
        assert_eq!(t.actions.len(), 2, "market + rng");
        assert!(matches!(t.actions[0], OrderItem::Order(_)));
        assert!(matches!(t.actions[1], OrderItem::RangeOco(_)));
        // Basket'te of OLMAMALI — borsa "invalid action in trigger order" der.
        assert!(!t.actions.iter().any(|a| matches!(a, OrderItem::OnFill(_))));
    }

    #[test]
    fn watched_bracketi_onfill_ile_baglar() {
        // Parent gerçek bir emir olduğu için of çalışıyor ve daha güvenli:
        // market reddedilirse rng hiç kurulmaz.
        let a = alert(
            candle_close(),
            AlertAction::Trade(spec(Side::Buy, Some(Exits::simple(87_200.0, 94_000.0)))),
        );
        let Compiled::Watched { items } = compile(&a, BUILDER).unwrap() else {
            panic!("Watched bekleniyordu");
        };
        assert_eq!(items.len(), 2);
        assert!(matches!(items[0], OrderItem::Order(_)));
        let OrderItem::OnFill(of) = &items[1] else {
            panic!("of bekleniyordu");
        };
        assert_eq!(of.p, 0, "parent = market emri");
        assert!(matches!(of.actions[0], OrderItem::RangeOco(_)));
    }

    #[test]
    fn builder_fee_her_giriste_ilistiriliyor() {
        // Fee iliştirilmezse gelir yok — sessizce kaçırılabilecek bir hata.
        for a in [
            alert(
                mark_cross(Cross::Below),
                AlertAction::Trade(spec(Side::Buy, None)),
            ),
            alert(candle_close(), AlertAction::Trade(spec(Side::Buy, None))),
        ] {
            let items = match compile(&a, BUILDER).unwrap() {
                Compiled::OnChain { items } => {
                    let OrderItem::TriggerBasket(t) = &items[0] else {
                        panic!()
                    };
                    t.actions.clone()
                }
                Compiled::Watched { items } => items,
                Compiled::NotifyOnly => panic!(),
            };
            let OrderItem::Order(o) = &items[0] else {
                panic!()
            };
            let c = o.commission.expect("builderCode iliştirilmemiş");
            assert_eq!(c.fee, pusu_core::BUILDER_FEE_BPS);
        }
    }

    #[test]
    fn long_collari_stop_altta_hedef_ustte_kurar() {
        let a = alert(
            candle_close(),
            AlertAction::Trade(spec(Side::Buy, Some(Exits::simple(87_200.0, 94_000.0)))),
        );
        let Compiled::Watched { items } = compile(&a, BUILDER).unwrap() else {
            panic!()
        };
        let OrderItem::OnFill(of) = &items[1] else {
            panic!()
        };
        let OrderItem::RangeOco(r) = &of.actions[0] else {
            panic!()
        };
        assert_eq!(r.collar_min, 87_200.0, "long'da alt bacak = stop");
        assert_eq!(r.collar_max, 94_000.0, "long'da üst bacak = hedef");
    }

    #[test]
    fn short_collari_ters_kurar() {
        // Short'ta stop yukarıda, hedef aşağıda — collar_min/max fiyat sırasına
        // göre olduğu için yer değiştiriyorlar. Karıştırılırsa koruma ters çalışır.
        let a = alert(
            candle_close(),
            AlertAction::Trade(spec(Side::Sell, Some(Exits::simple(94_000.0, 87_200.0)))),
        );
        let Compiled::Watched { items } = compile(&a, BUILDER).unwrap() else {
            panic!()
        };
        let OrderItem::OnFill(of) = &items[1] else {
            panic!()
        };
        let OrderItem::RangeOco(r) = &of.actions[0] else {
            panic!()
        };
        assert_eq!(r.collar_min, 87_200.0, "short'ta alt bacak = hedef");
        assert_eq!(r.collar_max, 94_000.0, "short'ta üst bacak = stop");
        assert!(!r.is_buy);
    }

    #[test]
    fn ters_cikis_reddedilir() {
        // Long'da stop hedefin üstündeyse emir dolar dolmaz kendini tetikler.
        let a = alert(
            candle_close(),
            AlertAction::Trade(spec(Side::Buy, Some(Exits::simple(94_000.0, 87_200.0)))),
        );
        assert!(matches!(
            compile(&a, BUILDER),
            Err(CompileError::IncoherentExits(..))
        ));
    }

    // -- kademeli çıkış -----------------------------------------------------

    /// Kullanıcının kurgusu: TP1 %30, TP2 %70; SL1 %50, SL2 %50.
    fn ladder_exits() -> Exits {
        Exits {
            take_profits: vec![ExitLeg::new(94_000.0, 30.0), ExitLeg::new(98_000.0, 70.0)],
            stops: vec![ExitLeg::new(88_000.0, 50.0), ExitLeg::new(86_000.0, 50.0)],
        }
    }

    #[test]
    fn kademeli_cikis_ayri_tp_ve_st_emirlerine_derlenir() {
        let a = alert(
            candle_close(),
            AlertAction::Trade(spec(Side::Buy, Some(ladder_exits()))),
        );
        let Compiled::Watched { items } = compile(&a, BUILDER).unwrap() else {
            panic!("Watched bekleniyordu");
        };
        let OrderItem::OnFill(of) = &items[1] else {
            panic!("of bekleniyordu");
        };
        assert_eq!(of.actions.len(), 4, "2 tp + 2 st");
        assert!(matches!(of.actions[0], OrderItem::TakeProfit(_)));
        assert!(matches!(of.actions[1], OrderItem::TakeProfit(_)));
        assert!(matches!(of.actions[2], OrderItem::Stop(_)));
        assert!(matches!(of.actions[3], OrderItem::Stop(_)));
    }

    #[test]
    fn kademe_yuzdeleri_mutlak_miktara_cevriliyor() {
        // size 0.01 → TP1 %30 = 0.003, TP2 %70 = 0.007
        let a = alert(
            candle_close(),
            AlertAction::Trade(spec(Side::Buy, Some(ladder_exits()))),
        );
        let Compiled::Watched { items } = compile(&a, BUILDER).unwrap() else {
            panic!()
        };
        let OrderItem::OnFill(of) = &items[1] else {
            panic!()
        };
        let OrderItem::TakeProfit(tp1) = &of.actions[0] else {
            panic!()
        };
        let OrderItem::TakeProfit(tp2) = &of.actions[1] else {
            panic!()
        };
        assert!((tp1.size - 0.003).abs() < 1e-12);
        assert!((tp2.size - 0.007).abs() < 1e-12);
    }

    #[test]
    fn longda_tp_yukari_st_asagi_tetikler() {
        // ⚠️ Ürünün en sinsi tuzağı. `is_buy` = tetik yönü, pozisyon yönü DEĞİL.
        // Ters verirsek borsa "resting" deyip emri hiç oluşturmuyor —
        // kullanıcı stop'u olduğunu sanır, yoktur. Staging'de ölçüldü.
        let a = alert(
            candle_close(),
            AlertAction::Trade(spec(Side::Buy, Some(ladder_exits()))),
        );
        let Compiled::Watched { items } = compile(&a, BUILDER).unwrap() else {
            panic!()
        };
        let OrderItem::OnFill(of) = &items[1] else {
            panic!()
        };
        let OrderItem::TakeProfit(tp) = &of.actions[0] else {
            panic!()
        };
        let OrderItem::Stop(st) = &of.actions[2] else {
            panic!()
        };
        assert!(tp.is_buy, "long'da kâr yukarıda → tp yukarı tetikler");
        assert!(!st.is_buy, "long'da zarar aşağıda → st aşağı tetikler");
    }

    #[test]
    fn shortta_tetik_yonleri_tersine_doner() {
        let e = Exits {
            take_profits: vec![ExitLeg::new(86_000.0, 30.0), ExitLeg::new(82_000.0, 70.0)],
            stops: vec![ExitLeg::new(94_000.0, 50.0), ExitLeg::new(96_000.0, 50.0)],
        };
        let a = alert(
            candle_close(),
            AlertAction::Trade(spec(Side::Sell, Some(e))),
        );
        let Compiled::Watched { items } = compile(&a, BUILDER).unwrap() else {
            panic!()
        };
        let OrderItem::OnFill(of) = &items[1] else {
            panic!()
        };
        let OrderItem::TakeProfit(tp) = &of.actions[0] else {
            panic!()
        };
        let OrderItem::Stop(st) = &of.actions[2] else {
            panic!()
        };
        assert!(!tp.is_buy, "short'ta kâr aşağıda → tp aşağı tetikler");
        assert!(st.is_buy, "short'ta zarar yukarıda → st yukarı tetikler");
    }

    #[test]
    fn tek_stop_tek_hedef_hala_rngye_derlenir() {
        // OCO atomik ve Faz 1'de doğrulanmış; en sık hal için onu koruyoruz.
        let a = alert(
            candle_close(),
            AlertAction::Trade(spec(Side::Buy, Some(Exits::simple(87_200.0, 94_000.0)))),
        );
        let Compiled::Watched { items } = compile(&a, BUILDER).unwrap() else {
            panic!()
        };
        let OrderItem::OnFill(of) = &items[1] else {
            panic!()
        };
        assert_eq!(of.actions.len(), 1);
        assert!(matches!(of.actions[0], OrderItem::RangeOco(_)));
    }

    #[test]
    fn kademeli_cikis_zincire_de_gomulebiliyor() {
        // Staging'de doğrulandı: trig { actions: [m, tp, tp, st, st] } kabul
        // ediliyor, dördü de reduce-only olarak dinlenmeye geçiyor.
        let a = alert(
            mark_cross(Cross::Below),
            AlertAction::Trade(spec(Side::Buy, Some(ladder_exits()))),
        );
        let Compiled::OnChain { items } = compile(&a, BUILDER).unwrap() else {
            panic!("OnChain bekleniyordu");
        };
        let OrderItem::TriggerBasket(b) = &items[0] else {
            panic!()
        };
        assert_eq!(b.actions.len(), 5, "1 market + 2 tp + 2 st");
    }

    #[test]
    fn yuzde_toplami_100u_asan_ladder_reddedilir() {
        let e = Exits {
            take_profits: vec![ExitLeg::new(94_000.0, 70.0), ExitLeg::new(98_000.0, 70.0)],
            stops: vec![],
        };
        let a = alert(candle_close(), AlertAction::Trade(spec(Side::Buy, Some(e))));
        assert_eq!(compile(&a, BUILDER), Err(CompileError::InvalidPercentages));
    }

    #[test]
    fn hedeflerin_arasina_dusen_stop_reddedilir() {
        let e = Exits {
            take_profits: vec![ExitLeg::new(94_000.0, 50.0), ExitLeg::new(98_000.0, 50.0)],
            stops: vec![ExitLeg::new(96_000.0, 100.0)],
        };
        let a = alert(candle_close(), AlertAction::Trade(spec(Side::Buy, Some(e))));
        assert!(matches!(
            compile(&a, BUILDER),
            Err(CompileError::IncoherentExits(..))
        ));
    }

    #[test]
    fn bildirim_derlenecek_bir_sey_uretmez() {
        let a = alert(mark_cross(Cross::Below), AlertAction::Notify);
        assert_eq!(compile(&a, BUILDER).unwrap(), Compiled::NotifyOnly);
    }

    #[test]
    fn bozuk_pubkey_reddedilir() {
        let a = alert(
            mark_cross(Cross::Below),
            AlertAction::Trade(spec(Side::Buy, None)),
        );
        assert!(matches!(
            compile(&a, "bu-bir-pubkey-degil"),
            Err(CompileError::BadPubkey(_))
        ));
    }

    #[test]
    fn giris_market_emri_ve_reduce_only_degil() {
        // reduce_only=true olsaydı pozisyon açamazdık — sessizce hiçbir şey olmazdı.
        let a = alert(candle_close(), AlertAction::Trade(spec(Side::Buy, None)));
        let Compiled::Watched { items } = compile(&a, BUILDER).unwrap() else {
            panic!()
        };
        let OrderItem::Order(o) = &items[0] else {
            panic!()
        };
        assert!(!o.reduce_only);
        assert!(matches!(
            o.order_type,
            OrderType::Trigger {
                is_market: true,
                ..
            }
        ));
    }
}
