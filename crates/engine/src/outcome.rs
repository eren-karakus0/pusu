//! Borsanın cevabını **kesin bir sonuca** çevirir.
//!
//! # Neden ayrı bir modül
//!
//! BULK reddedilen emirde HTTP 200 ve `{"status":"ok"}` dönüyor. Gerçek sonuç
//! `statuses` dizisinin içinde. Bunu okumadan "alarm çalıştı" demek,
//! kullanıcıya yalan söylemek olur.
//!
//! # İndeks hizasına güvenmiyoruz
//!
//! Staging'de gözlemlendi: `[trig, of]` gönderildiğinde dönen statuses
//! `[ack{ok:false, "on_fill parent not found"}, resting]` oldu — yani `of`'un
//! hatası **birinci** sırada, `trig`'in sonucu ikinci. Gönderdiğimiz sırayla
//! eşleşmiyor. `[m, of]` gönderildiğinde ise `[filled, resting]` hizalıydı.
//!
//! Hiza tutarsız olduğu için tüm statuses'ı tarayıp anlam çıkarıyoruz.
//!
//! # En tehlikeli hal: korumasız pozisyon
//!
//! Giriş dolup bracket reddedilirse kullanıcı **korumasız pozisyonda** kalır.
//! Staging'de gerçekleşti (`[trig, of{p:0}]`). Bu, sessizce "başarılı"
//! sayılamayacak bir durum; [`Outcome::FilledUnprotected`] olarak ayrı
//! işaretleniyor ki kullanıcıya haber verilebilsin.

use serde_json::Value;

/// Bir gönderimin kesin sonucu.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Giriş doldu, sorun yok.
    Filled { avg_price: f64, size: f64 },

    /// 🚨 Giriş doldu **ama** bir şey reddedildi — büyük ihtimalle bracket.
    /// Kullanıcının korumasız pozisyonu var; derhal haber verilmeli.
    FilledUnprotected {
        avg_price: f64,
        size: f64,
        reason: String,
    },

    /// Giriş girmedi. Alarm çalıştı ama işlem olmadı.
    Rejected { reason: String },

    /// Emir book'ta bekliyor (limit/conditional). Henüz dolmadı.
    Resting,

    /// Yanıt anlaşılamadı. Ateşlendi mi bilinmiyor → insan bakmalı.
    Unknown { raw: String },
}

impl Outcome {
    /// Kullanıcıya "işlemin girdi" denebilir mi?
    pub const fn entered(&self) -> bool {
        matches!(self, Self::Filled { .. } | Self::FilledUnprotected { .. })
    }

    /// Acil müdahale gerekiyor mu?
    pub const fn needs_attention(&self) -> bool {
        matches!(self, Self::FilledUnprotected { .. } | Self::Unknown { .. })
    }
}

/// Borsanın `/order` yanıtını yorumla.
///
/// `raw`: `POST /order`'ın döndürdüğü tam JSON.
pub fn interpret(raw: &Value) -> Outcome {
    let statuses = raw
        .pointer("/response/data/statuses")
        .and_then(Value::as_array);

    let Some(statuses) = statuses else {
        return Outcome::Unknown {
            raw: raw.to_string(),
        };
    };
    if statuses.is_empty() {
        return Outcome::Unknown {
            raw: raw.to_string(),
        };
    }

    let mut fill: Option<(f64, f64)> = None;
    let mut hata: Option<String> = None;
    let mut resting = false;

    for s in statuses {
        let Some((kind, body)) = s.as_object().and_then(|o| o.iter().next()) else {
            continue;
        };
        match kind.as_str() {
            "filled" | "partiallyFilled" => {
                let px = body["avgPx"].as_f64().unwrap_or(0.0);
                let sz = body["totalSz"].as_f64().unwrap_or(0.0);
                fill = Some((px, sz.abs()));
            }
            "resting" | "working" | "triggered" => resting = true,
            // `ack` hem başarı hem hata taşıyabiliyor: {"ok":true} / {"ok":false,"message":...}
            "ack" => {
                if body["ok"].as_bool() == Some(false) {
                    hata = Some(mesaj(body));
                }
            }
            "rejectedInvalid"
            | "cancelledRiskLimit"
            | "cancelledReduceOnly"
            | "error"
            | "transferFailed"
            | "createSubAccountFailed" => {
                hata = Some(mesaj(body));
            }
            _ => {}
        }
    }

    match (fill, hata) {
        // Giriş doldu ama bir şey de reddedildi → koruma kurulmamış olabilir.
        (Some((px, sz)), Some(reason)) => Outcome::FilledUnprotected {
            avg_price: px,
            size: sz,
            reason,
        },
        (Some((px, sz)), None) => Outcome::Filled {
            avg_price: px,
            size: sz,
        },
        (None, Some(reason)) => Outcome::Rejected { reason },
        (None, None) if resting => Outcome::Resting,
        (None, None) => Outcome::Unknown {
            raw: raw.to_string(),
        },
    }
}

fn mesaj(body: &Value) -> String {
    body["reason"]
        .as_str()
        .or_else(|| body["message"].as_str())
        .unwrap_or("sebep belirtilmemiş")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Staging'den birebir alınmış yanıtlar.
    mod gercek_yanitlar {
        use super::*;

        /// [m, of{p:0,[rng]}] — çalışan Watched şablonu
        pub fn watched_basarili() -> Value {
            json!({"status":"ok","response":{"type":"order","data":{"statuses":[
                {"filled":{"totalSz":0.001,"avgPx":65459.75,"oid":"8ANYiRbHF9kdhnKtgvDBRdmseBZRnw2ki4hZTVD1Gy2W"}},
                {"resting":{"oid":"2R23vdec3pczHhB8au1FVc2ARTsHvme9xRYz5hxGjgRW"}}
            ]}}})
        }

        /// [trig, of{p:0}] — bracket reddedildi ama trigger ateşledi
        pub fn korumasiz() -> Value {
            json!({"status":"ok","response":{"type":"order","data":{"statuses":[
                {"ack":{"ok":false,"message":"on_fill parent not found for seqno=0 nonce=1784129021782"}},
                {"filled":{"totalSz":0.002,"avgPx":65278.25,"oid":"abc"}}
            ]}}})
        }

        /// Onaysız builder code
        pub fn builder_onaysiz() -> Value {
            json!({"status":"ok","response":{"type":"order","data":{"statuses":[
                {"rejectedInvalid":{"oid":"FB9XPjuao5TZXhM5JPqQL8t4umRKCxzczvjM3uVGmNL4",
                 "reason":"builder-code fee 2bps not approved for AdjWd4DCeKC3P4QjRaP5BmmcPMs1YaQ8kRjPqpnbnqdz"}}
            ]}}})
        }

        /// trig içine of gömülmüş
        pub fn gecersiz_trigger() -> Value {
            json!({"status":"ok","response":{"type":"order","data":{"statuses":[
                {"rejectedInvalid":{"oid":"GqTNLAvRyYpnva7JHcTcBmo9VVbVtGBWWd1DAxMWJGKN",
                 "reason":"invalid action in trigger order"}}
            ]}}})
        }

        /// trig kaydedildi, bekliyor
        pub fn trigger_bekliyor() -> Value {
            json!({"status":"ok","response":{"type":"order","data":{"statuses":[
                {"resting":{"oid":"BM77oSQYvd2JcpJZwSGkb3EAkbgKrpHBLphi9egUkwz4"}}
            ]}}})
        }
    }

    #[test]
    fn dolan_emir_dogru_okunuyor() {
        let o = interpret(&gercek_yanitlar::watched_basarili());
        assert_eq!(
            o,
            Outcome::Filled {
                avg_price: 65459.75,
                size: 0.001
            }
        );
        assert!(o.entered());
        assert!(!o.needs_attention());
    }

    #[test]
    fn korumasiz_pozisyon_yakalaniyor() {
        // En tehlikeli hal: pozisyon açıldı, koruma kurulmadı.
        // Üstelik hata BİRİNCİ sırada, dolum ikinci — indekse güvenseydik kaçırırdık.
        let o = interpret(&gercek_yanitlar::korumasiz());
        let Outcome::FilledUnprotected { reason, size, .. } = &o else {
            panic!("FilledUnprotected bekleniyordu, gelen: {o:?}");
        };
        assert_eq!(*size, 0.002);
        assert!(reason.contains("on_fill parent not found"));
        assert!(o.entered(), "pozisyon açıldı");
        assert!(o.needs_attention(), "kullanıcıya haber verilmeli");
    }

    #[test]
    fn onaysiz_builder_code_reddi_okunuyor() {
        let o = interpret(&gercek_yanitlar::builder_onaysiz());
        let Outcome::Rejected { reason } = &o else {
            panic!("Rejected bekleniyordu, gelen: {o:?}");
        };
        assert!(reason.contains("not approved"));
        assert!(!o.entered(), "işlem girmedi");
    }

    #[test]
    fn gecersiz_trigger_reddi_okunuyor() {
        let o = interpret(&gercek_yanitlar::gecersiz_trigger());
        assert_eq!(
            o,
            Outcome::Rejected {
                reason: "invalid action in trigger order".into()
            }
        );
    }

    #[test]
    fn bekleyen_trigger_basari_sayilmiyor() {
        // Trigger kaydedildi ama henüz tetiklenmedi — "işlemin girdi" DEMEK YOK.
        let o = interpret(&gercek_yanitlar::trigger_bekliyor());
        assert_eq!(o, Outcome::Resting);
        assert!(!o.entered());
    }

    #[test]
    fn status_ok_gorunse_bile_ret_yakalaniyor() {
        // Bütün modülün var oluş sebebi: dış zarf "ok" diyor, içerik "reddedildi".
        let v = gercek_yanitlar::builder_onaysiz();
        assert_eq!(v["status"], "ok");
        assert!(!interpret(&v).entered());
    }

    #[test]
    fn ack_ok_true_hata_sayilmaz() {
        // Builder onayı {"ok":true} döndürüyor — bunu hata sanmamalıyız.
        let v = json!({"status":"ok","response":{"data":{"statuses":[{"ack":{"ok":true}}]}}});
        assert!(matches!(interpret(&v), Outcome::Unknown { .. }));
    }

    #[test]
    fn taninmayan_yanit_unknown_olur() {
        // Sessizce "başarılı" saymaktansa insana bırak.
        let o = interpret(&json!({"beklenmedik": "sey"}));
        assert!(matches!(o, Outcome::Unknown { .. }));
        assert!(o.needs_attention());
    }

    #[test]
    fn bos_statuses_unknown_olur() {
        let v = json!({"status":"ok","response":{"data":{"statuses":[]}}});
        assert!(matches!(interpret(&v), Outcome::Unknown { .. }));
    }

    #[test]
    fn sebepsiz_ret_de_okunuyor() {
        let v = json!({"status":"ok","response":{"data":{"statuses":[{"rejectedInvalid":{"oid":"x"}}]}}});
        let Outcome::Rejected { reason } = interpret(&v) else {
            panic!()
        };
        assert_eq!(reason, "sebep belirtilmemiş");
    }

    #[test]
    fn kismi_dolum_da_giris_sayilir() {
        let v = json!({"status":"ok","response":{"data":{"statuses":[
            {"partiallyFilled":{"totalSz":0.0005,"avgPx":65000.0,"oid":"x"}}
        ]}}});
        assert!(interpret(&v).entered());
    }

    #[test]
    fn negatif_size_mutlak_deger_olarak_okunuyor() {
        // Borsa satış tarafını negatif döndürüyor: sell 0.1 → totalSz: -0.1
        let v = json!({"status":"ok","response":{"data":{"statuses":[
            {"filled":{"totalSz":-0.1,"avgPx":65000.0,"oid":"x"}}
        ]}}});
        let Outcome::Filled { size, .. } = interpret(&v) else {
            panic!()
        };
        assert_eq!(size, 0.1);
    }
}
