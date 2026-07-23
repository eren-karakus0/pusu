//! Uçtan uca: bir cümlenin gerçekten imzalanabilir `OrderItem`'lara indiğini
//! gösterir — `pusu_nl::parse → into_alert → pusu_compile::compile`.

use bulk_keychain::OrderItem;
use pusu_compile::{compile, Compiled};
use pusu_nl::{parse, AlertCtx};

const BUILDER: &str = "AdjWd4DCeKC3P4QjRaP5BmmcPMs1YaQ8kRjPqpnbnqdz";

fn ctx() -> AlertCtx {
    AlertCtx {
        id: "t1".into(),
        owner: "master".into(),
        account: "sub".into(),
        now_ms: 0,
    }
}

#[test]
fn mark_cross_cumlesi_trigger_baskete_iner() {
    let draft = parse("short 2 ETH when ETH breaks below $3,000")
        .unwrap()
        .draft;
    let alert = draft.into_alert(ctx());
    let Compiled::OnChain { items } = compile(&alert, BUILDER).unwrap() else {
        panic!("OnChain bekleniyordu");
    };
    assert_eq!(items.len(), 1);
    assert!(matches!(items[0], OrderItem::TriggerBasket(_)));
}

#[test]
fn mum_kapanisi_cumlesi_watchera_ve_of_ile_cikisa_iner() {
    let draft = parse("if the 1h candle closes above $90k, long 0.5 BTC, SL $88k")
        .unwrap()
        .draft;
    let alert = draft.into_alert(ctx());
    let Compiled::Watched { items } = compile(&alert, BUILDER).unwrap() else {
        panic!("Watched bekleniyordu");
    };
    // market emri + on-fill'e bağlı çıkış.
    assert_eq!(items.len(), 2);
    assert!(matches!(items[0], OrderItem::Order(_)));
    assert!(matches!(items[1], OrderItem::OnFill(_)));
}

#[test]
fn notify_cumlesi_derlenecek_sey_uretmez() {
    let draft = parse("notify me when the 4h candle closes below $2,800 on ETH")
        .unwrap()
        .draft;
    let alert = draft.into_alert(ctx());
    assert_eq!(compile(&alert, BUILDER).unwrap(), Compiled::NotifyOnly);
}

#[test]
fn builder_fee_giriste_ilistiriliyor() {
    let draft = parse("if the 1h candle closes above $90k, long 0.5 BTC")
        .unwrap()
        .draft;
    let alert = draft.into_alert(ctx());
    let Compiled::Watched { items } = compile(&alert, BUILDER).unwrap() else {
        panic!();
    };
    let OrderItem::Order(o) = &items[0] else {
        panic!();
    };
    assert_eq!(
        o.commission.expect("builderCode iliştirilmeli").fee,
        pusu_core::BUILDER_FEE_BPS
    );
}
