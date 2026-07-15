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

use bulk_keychain::{
    Commission, OnFill, Order, OrderItem, OrderType, Pubkey, RangeOco, TriggerBasket,
};
use pusu_core::{Alert, AlertAction, Bracket, Condition, Execution, Side, TradeSpec};

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
    #[error("bracket tutarsız: {0} işlemde stop {1}, hedef {2} olamaz")]
    IncoherentBracket(&'static str, f64, f64),

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

    validate_bracket(spec)?;

    match alert.execution() {
        Execution::OnChain => compile_onchain(&alert.condition, spec, builder),
        Execution::Watched { .. } => compile_watched(spec, builder),
    }
}

/// 🔒 `trig { c, d, tr, actions: [m{builderCode}, rng] }`
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

    let mut actions = vec![market_entry(spec, builder)?];
    if let Some(b) = spec.bracket {
        actions.push(collar(spec, b));
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

/// ⚡ `[m{builderCode}, of{p:0, actions:[rng]}]`
fn compile_watched(spec: &TradeSpec, builder: &str) -> Result<Compiled, CompileError> {
    let mut items = vec![market_entry(spec, builder)?];
    if let Some(b) = spec.bracket {
        // Parent = index 0 (market emri). Gerçek bir emir olduğu için `of`
        // burada çalışıyor: market reddedilirse bracket hiç kurulmuyor.
        items.push(OrderItem::OnFill(OnFill {
            p: 0,
            actions: vec![collar(spec, b)],
        }));
    }
    Ok(Compiled::Watched { items })
}

fn market_entry(spec: &TradeSpec, builder: &str) -> Result<OrderItem, CompileError> {
    let to = Pubkey::from_base58(builder).map_err(|_| CompileError::BadPubkey(builder.into()))?;
    let commission = Commission::new(to, pusu_core::BUILDER_FEE_BPS)
        .map_err(|_| CompileError::InvalidFee(pusu_core::BUILDER_FEE_BPS))?;

    Ok(OrderItem::Order(Order {
        symbol: spec.symbol.to_string(),
        is_buy: spec.side.is_buy(),
        price: 0.0,
        size: spec.size,
        reduce_only: false,
        iso: false,
        order_type: OrderType::market(),
        client_id: None,
        commission: Some(commission),
    }))
}

/// Stop + hedefi tek OCO collar'ına çevirir: bir bacak tetiklenince diğeri iptal.
///
/// `collar_min`/`collar_max` fiyat sırasına göre; hangisinin stop hangisinin
/// hedef olduğu yöne bağlı. Long'da stop altta, short'ta üstte.
fn collar(spec: &TradeSpec, b: Bracket) -> OrderItem {
    let (min, max) = match spec.side {
        Side::Buy => (b.stop, b.take_profit),
        Side::Sell => (b.take_profit, b.stop),
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

fn validate_bracket(spec: &TradeSpec) -> Result<(), CompileError> {
    let Some(b) = spec.bracket else {
        return Ok(());
    };
    // Girişi bilmiyoruz (market emri), o yüzden girişe göre değil kendi
    // içinde tutarlılığı kontrol ediyoruz: long'da stop < hedef, short'ta tersi.
    let ok = match spec.side {
        Side::Buy => b.stop < b.take_profit,
        Side::Sell => b.stop > b.take_profit,
    };
    if ok {
        Ok(())
    } else {
        Err(CompileError::IncoherentBracket(
            spec.side.label(),
            b.stop,
            b.take_profit,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pusu_core::{AlertId, AlertState, Cross, Interval, Symbol};

    const BUILDER: &str = "AdjWd4DCeKC3P4QjRaP5BmmcPMs1YaQ8kRjPqpnbnqdz";

    fn spec(side: Side, bracket: Option<Bracket>) -> TradeSpec {
        TradeSpec {
            symbol: Symbol::new("BTC-USD"),
            side,
            size: 0.01,
            bracket,
        }
    }

    fn alert(condition: Condition, action: AlertAction) -> Alert {
        Alert {
            id: AlertId::new("a1"),
            owner: "master".into(),
            account: "sub".into(),
            condition,
            action,
            state: AlertState::Armed,
            armed_at_ms: 0,
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
            AlertAction::Trade(spec(
                Side::Buy,
                Some(Bracket {
                    stop: 87_200.0,
                    take_profit: 94_000.0,
                }),
            )),
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
            AlertAction::Trade(spec(
                Side::Buy,
                Some(Bracket {
                    stop: 87_200.0,
                    take_profit: 94_000.0,
                }),
            )),
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
            AlertAction::Trade(spec(
                Side::Buy,
                Some(Bracket {
                    stop: 87_200.0,
                    take_profit: 94_000.0,
                }),
            )),
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
            AlertAction::Trade(spec(
                Side::Sell,
                Some(Bracket {
                    stop: 94_000.0,
                    take_profit: 87_200.0,
                }),
            )),
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
    fn ters_bracket_reddedilir() {
        // Long'da stop hedefin üstündeyse emir dolar dolmaz kendini tetikler.
        let a = alert(
            candle_close(),
            AlertAction::Trade(spec(
                Side::Buy,
                Some(Bracket {
                    stop: 94_000.0,
                    take_profit: 87_200.0,
                }),
            )),
        );
        assert!(matches!(
            compile(&a, BUILDER),
            Err(CompileError::IncoherentBracket(..))
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
