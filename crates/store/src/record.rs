//! Satır ↔ domain nesnesi dönüşümü.
//!
//! `AlertState` ile SQL enum'u arasındaki eşleme tek yerde tutuluyor ki
//! ikisinin sapması derleme değil, buradaki `match` kaçağıyla yakalansın.

use crate::StoreError;
use pusu_core::{Alert, AlertAction, AlertId, AlertState, Condition};
use sqlx::postgres::PgRow;
use sqlx::Row;

/// `AlertState` → SQL enum literali.
pub fn state_str(s: AlertState) -> &'static str {
    match s {
        AlertState::Armed => "armed",
        AlertState::Working => "working",
        AlertState::Fired => "fired",
        AlertState::Cancelled => "cancelled",
        AlertState::Rejected => "rejected",
        AlertState::Uncertain => "uncertain",
        AlertState::Missed => "missed",
    }
}

/// SQL enum literali → `AlertState`.
pub fn str_to_state(s: &str) -> Option<AlertState> {
    Some(match s {
        "armed" => AlertState::Armed,
        "working" => AlertState::Working,
        "fired" => AlertState::Fired,
        "cancelled" => AlertState::Cancelled,
        "rejected" => AlertState::Rejected,
        "uncertain" => AlertState::Uncertain,
        "missed" => AlertState::Missed,
        _ => return None,
    })
}

/// Bir satırı `Alert`'e çöz.
pub fn row_to_alert(row: &PgRow) -> Result<Alert, StoreError> {
    let state_s: String = row.get("state");
    let state = str_to_state(&state_s)
        .ok_or_else(|| StoreError::NotFound(format!("bilinmeyen state: {state_s}")))?;

    let condition: Condition = serde_json::from_value(row.get("condition"))?;
    let action: AlertAction = serde_json::from_value(row.get("action"))?;
    let invalidate: Option<Condition> = row
        .get::<Option<serde_json::Value>, _>("invalidate")
        .map(serde_json::from_value)
        .transpose()?;

    let armed_at_ms: i64 = row.get("armed_at_ms");
    let fill_deadline_ms: Option<i64> = row.get("fill_deadline_ms");

    Ok(Alert {
        id: AlertId::new(row.get::<String, _>("id")),
        owner: row.get("owner"),
        account: row.get("account"),
        condition,
        invalidate,
        action,
        state,
        armed_at_ms: armed_at_ms as u64,
        entry_oid: row.get("entry_oid"),
        fill_deadline_ms: fill_deadline_ms.map(|v| v as u64),
    })
}
