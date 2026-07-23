//! PUSU'nun kalıcı durumu: alarmlar, ön-imzalı blob'lar, denetim kaydı.
//!
//! # Tasarımı belirleyen ölçüm
//!
//! Ön-imzalı blob nonce sayesinde **idempotent** (§8.11): aynı blob'u iki kez
//! göndermek çift pozisyon açmıyor, borsa ikinciyi nonce'a bakarak eliyor.
//! Sonuç: çift-giriş koruması için write-ahead log GEREKMİYOR.
//!
//! Ama çökme sonrası "bu emri gönderdim mi?" sorusu hâlâ var. Cevabı **sorgu**
//! ile veriyoruz, körlemesine tekrar gönderimle değil: `entry_oid` gönderim
//! öncesi hesaplanabildiği için (§8.9), açılışta `openOrders`'ı o oid için
//! sorgulamak yeterli. Bu yüzden `entry_oid` ve gönderim niyeti
//! ([`BlobRole`] başına `dispatched_at`) kalıcı.
//!
//! # Neden domain nesneleri JSONB
//!
//! `condition`/`action`/`invalidate`/`exits`'in şekli Rust'ta (`pusu_core`)
//! yaşıyor ve hızla evriliyor. SQL şemasına kopyalamak iki yerde bakım
//! demekti; sorgular yalnızca skaler kolonlara (owner, account, state,
//! deadline) dokunuyor, JSONB gövdeler yalnızca taşınıyor.

use pusu_core::{Alert, AlertId};
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Row;

mod crypto;
mod record;

pub use crypto::check_key;
use record::state_str;
pub use record::str_to_state;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("veritabanı: {0}")]
    Db(#[from] sqlx::Error),
    #[error("alarm serileştirme: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("alarm bulunamadı: {0}")]
    NotFound(String),
    #[error("blob şifreleme: {0}")]
    Crypto(String),
}

/// Ön-imzalı bir tx'in rolü.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobRole {
    /// Giriş emri (market ya da limit).
    Entry,
    /// Girişin ön-imzalı iptali — limit girişte var (§8.9).
    Cancel,
}

impl BlobRole {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Entry => "entry",
            Self::Cancel => "cancel",
        }
    }
}

/// Veritabanı havuzu + PUSU'ya özel işlemler.
#[derive(Clone)]
pub struct Store {
    pool: PgPool,
}

impl Store {
    /// Bağlan ve bekleyen migration'ları uygula.
    pub async fn connect(url: &str) -> Result<Self, StoreError> {
        let pool = PgPoolOptions::new().max_connections(8).connect(url).await?;
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(sqlx::Error::from)?;
        Ok(Self { pool })
    }

    /// Hazır havuzu migration çalıştırarak sar (test/gelişmiş kullanım).
    pub async fn from_pool(pool: PgPool) -> Result<Self, StoreError> {
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(sqlx::Error::from)?;
        Ok(Self { pool })
    }

    /// Hazır havuzu **migration çalıştırmadan** sar.
    ///
    /// Şemanın zaten kurulu olduğunu bilen çağıranlar için — testlerde paylaşımlı
    /// havuzu tekrar tekrar sarmak gibi. Migration'ı her seferinde çalıştırmak
    /// yalnızca gereksiz değil, eşzamanlı çağrılırsa `_sqlx_migrations` üzerinde
    /// kilitlenmeye yol açıyor.
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// DB canlı mı? Health-check için hafif `SELECT 1` — statik "ok" yalan
    /// söylemesin (Postgres düşükken sağlıklı görünmek en tehlikeli hata).
    pub async fn ping(&self) -> Result<(), StoreError> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    /// Kullanıcıyı kaydet (idempotent).
    pub async fn upsert_user(&self, pubkey: &str) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO users (pubkey) VALUES ($1) ON CONFLICT (pubkey) DO NOTHING")
            .bind(pubkey)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Yeni bir alarm kaydet. Owner önceden `upsert_user` edilmiş olmalı.
    pub async fn insert_alert(&self, alert: &Alert) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO alerts \
             (id, owner, account, state, condition, invalidate, action, \
              armed_at_ms, entry_oid, fill_deadline_ms) \
             VALUES ($1,$2,$3,$4::alert_state,$5,$6,$7,$8,$9,$10)",
        )
        .bind(alert.id.as_str())
        .bind(&alert.owner)
        .bind(&alert.account)
        .bind(state_str(alert.state))
        .bind(serde_json::to_value(&alert.condition)?)
        .bind(
            alert
                .invalidate
                .as_ref()
                .map(serde_json::to_value)
                .transpose()?,
        )
        .bind(serde_json::to_value(&alert.action)?)
        .bind(i64_of(alert.armed_at_ms))
        .bind(alert.entry_oid.as_deref())
        .bind(alert.fill_deadline_ms.map(i64_of))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Watcher'ın ilgilendiği alarmlar: `armed` + `working`.
    ///
    /// Kısmi indeks (`alerts_live`) bunu terminal alarm sayısından bağımsız
    /// tutuyor.
    pub async fn load_live(&self) -> Result<Vec<Alert>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, owner, account, state::text AS state, condition, invalidate, \
                    action, armed_at_ms, entry_oid, fill_deadline_ms, cancel_requested \
             FROM alerts WHERE state IN ('armed','working')",
        )
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(record::row_to_alert).collect()
    }

    /// Bir kullanıcının **tüm** alarmları, en yeni önce.
    ///
    /// `load_live`'dan farkı: terminal alarmları (fired/missed/cancelled…) da
    /// getirir, çünkü liste ekranı kullanıcının ne kurduğunu ve sonucunu
    /// göstermek için var. Blob'lar dönmüyor — imzalı gövdeleri istemciye geri
    /// vermeye gerek yok, alarmın kendisi yeter.
    pub async fn list_by_owner(&self, owner: &str) -> Result<Vec<Alert>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, owner, account, state::text AS state, condition, invalidate, \
                    action, armed_at_ms, entry_oid, fill_deadline_ms, cancel_requested \
             FROM alerts WHERE owner = $1 ORDER BY armed_at_ms DESC",
        )
        .bind(owner)
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(record::row_to_alert).collect()
    }

    /// Bir alarmın değişen alanlarını yaz. Watcher her tur sonunda çağırıyor.
    ///
    /// Yalnızca watcher'ın değiştirdiği alanlar: state, entry_oid,
    /// fill_deadline_ms. condition/action imzalıdır, değişmez.
    pub async fn update_runtime(&self, alert: &Alert) -> Result<(), StoreError> {
        let n = sqlx::query(
            "UPDATE alerts SET \
                state = $2::alert_state, entry_oid = $3, fill_deadline_ms = $4, \
                updated_at = now() \
             WHERE id = $1",
        )
        .bind(alert.id.as_str())
        .bind(state_str(alert.state))
        .bind(alert.entry_oid.as_deref())
        .bind(alert.fill_deadline_ms.map(i64_of))
        .execute(&self.pool)
        .await?
        .rows_affected();

        if n == 0 {
            return Err(StoreError::NotFound(alert.id.as_str().to_string()));
        }
        Ok(())
    }

    /// Ön-imzalı blob'u sakla. Alarm başına role başına bir tane.
    ///
    /// Payload **at-rest şifreleniyor** (AES-256-GCM): DB sızsa bile emirler
    /// çözülemez, koşulsuz postalanamaz. Bkz. [`crate::crypto`].
    pub async fn put_blob(
        &self,
        alert_id: &AlertId,
        role: BlobRole,
        nonce: u64,
        payload: &serde_json::Value,
    ) -> Result<(), StoreError> {
        let ciphertext = crypto::encrypt(payload)?;
        sqlx::query(
            "INSERT INTO presigned_blobs (alert_id, role, nonce, payload) \
             VALUES ($1, $2::blob_role, $3, $4) \
             ON CONFLICT (alert_id, role) DO UPDATE SET nonce = $3, payload = $4",
        )
        .bind(alert_id.as_str())
        .bind(role.as_str())
        .bind(i64_of(nonce))
        .bind(&ciphertext)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Bir blob'u getir (göndermek için). At-rest şifreli; burada çözülüyor.
    pub async fn get_blob(
        &self,
        alert_id: &AlertId,
        role: BlobRole,
    ) -> Result<Option<serde_json::Value>, StoreError> {
        let row = sqlx::query(
            "SELECT payload FROM presigned_blobs \
             WHERE alert_id = $1 AND role = $2::blob_role",
        )
        .bind(alert_id.as_str())
        .bind(role.as_str())
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(r) => Ok(Some(crypto::decrypt(&r.get::<Vec<u8>, _>("payload"))?)),
            None => Ok(None),
        }
    }

    /// Gönderim niyetini işaretle — blob'u postalaMADAN önce.
    ///
    /// Çökme sonrası "bunu gönderdim mi?" sorusunun cevabı: `dispatched_at`
    /// doluysa gönderilmiş olabilir → `openOrders`'ı oid için sorgulayarak
    /// mutabakat yapılır (§8.11), körlemesine tekrar gönderilmez.
    pub async fn mark_dispatched(
        &self,
        alert_id: &AlertId,
        role: BlobRole,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE presigned_blobs SET dispatched_at = now() \
             WHERE alert_id = $1 AND role = $2::blob_role",
        )
        .bind(alert_id.as_str())
        .bind(role.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Bu blob postalanmış mı? Çökme sonrası mutabakatın giriş sorusu.
    pub async fn was_dispatched(
        &self,
        alert_id: &AlertId,
        role: BlobRole,
    ) -> Result<bool, StoreError> {
        let row = sqlx::query(
            "SELECT dispatched_at IS NOT NULL AS sent FROM presigned_blobs \
             WHERE alert_id = $1 AND role = $2::blob_role",
        )
        .bind(alert_id.as_str())
        .bind(role.as_str())
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| r.get::<bool, _>("sent")).unwrap_or(false))
    }

    /// Alarmı ve (CASCADE ile) blob'larını sil. Denetim kaydı KALIR.
    pub async fn delete_alert(&self, alert_id: &AlertId) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM alerts WHERE id = $1")
            .bind(alert_id.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Kullanıcının **beklemedeki** (armed) alarmını iptal et.
    ///
    /// Owner ve state koşullu: başkasının alarmına dokunmaz, çoktan ateşlenmiş
    /// (fired/working…) bir alarmı iptal etmez. Watched armed alarmın borsada
    /// canlı emri olmadığı için iptal yalnızca yerel: state → cancelled, blob'lar
    /// silinir (bir daha gönderilemesin). İptal edildiyse `true`.
    ///
    /// Working iptali bu yola girmiyor: orada defterde canlı bir limit emri var,
    /// iptali ön-imzalı `cx`'in watcher'ca gönderilmesini gerektiriyor.
    pub async fn cancel_armed(&self, alert_id: &AlertId, owner: &str) -> Result<bool, StoreError> {
        let n = sqlx::query(
            "UPDATE alerts SET state = 'cancelled'::alert_state, updated_at = now() \
             WHERE id = $1 AND owner = $2 AND state = 'armed'",
        )
        .bind(alert_id.as_str())
        .bind(owner)
        .execute(&self.pool)
        .await?
        .rows_affected();

        if n == 0 {
            return Ok(false);
        }
        // Blob'lar artık gereksiz. Alarm satırı kaldığı için CASCADE devreye
        // girmiyor; elle siliyoruz (iptal edilen alarm asla gönderilmemeli).
        sqlx::query("DELETE FROM presigned_blobs WHERE alert_id = $1")
            .bind(alert_id.as_str())
            .execute(&self.pool)
            .await?;
        Ok(true)
    }

    /// Kullanıcının **defterde bekleyen** (working) girişinin iptalini iste.
    ///
    /// api'nin borsaya imza yetkisi yok; bayrağı kaldırıyor, watcher bir sonraki
    /// turda görüp ön-imzalı `cx`'i gönderiyor ([`pusu_core::Alert::cancel_requested`]).
    /// Owner ve state koşullu: yalnızca sahibinin working alarmı. İşaretlendiyse
    /// `true`.
    pub async fn request_cancel(
        &self,
        alert_id: &AlertId,
        owner: &str,
    ) -> Result<bool, StoreError> {
        let n = sqlx::query(
            "UPDATE alerts SET cancel_requested = true, updated_at = now() \
             WHERE id = $1 AND owner = $2 AND state = 'working'",
        )
        .bind(alert_id.as_str())
        .bind(owner)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Kullanıcının **sonlanmış** alarmını listeden kaldır (satır + blob'lar).
    ///
    /// Owner ve state koşullu: aktif alarm (armed/working) silinmiyor — önce
    /// iptal edilmeli. Denetim kaydı FK'siz olduğu için silinmeden kalıyor.
    /// Silindiyse `true`.
    pub async fn delete_owned_terminal(
        &self,
        alert_id: &AlertId,
        owner: &str,
    ) -> Result<bool, StoreError> {
        let n = sqlx::query(
            "DELETE FROM alerts \
             WHERE id = $1 AND owner = $2 AND state NOT IN ('armed', 'working')",
        )
        .bind(alert_id.as_str())
        .bind(owner)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Değişmez denetim kaydına bir satır ekle.
    pub async fn audit(
        &self,
        alert_id: &AlertId,
        kind: &str,
        detail: &serde_json::Value,
    ) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO audit_log (alert_id, kind, detail) VALUES ($1, $2, $3)")
            .bind(alert_id.as_str())
            .bind(kind)
            .bind(detail)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Bir alarmın denetim satırı sayısı (test/gözlem için).
    pub async fn audit_count(&self, alert_id: &AlertId) -> Result<i64, StoreError> {
        let row = sqlx::query("SELECT count(*) AS n FROM audit_log WHERE alert_id = $1")
            .bind(alert_id.as_str())
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get::<i64, _>("n"))
    }

    /// Bir Notify olayını bildirim outbox'ına yaz. `(alert_id, kind)` tekil
    /// olduğu için idempotent: watcher aynı turu çökme sonrası tekrar işlese de
    /// çift bildirim olmaz. Yeni satır girdiyse `true`.
    pub async fn record_notification(
        &self,
        owner: &str,
        alert_id: &AlertId,
        kind: &str,
        body: &serde_json::Value,
    ) -> Result<bool, StoreError> {
        let n = sqlx::query(
            "INSERT INTO notifications (owner, alert_id, kind, body) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (alert_id, kind) DO NOTHING",
        )
        .bind(owner)
        .bind(alert_id.as_str())
        .bind(kind)
        .bind(body)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Kullanıcının bildirimleri, en yeni önce (en çok `limit` satır).
    ///
    /// JSON döndürüyor (domain tipi yok): `{id, alert_id, kind, body,
    /// created_at_ms, read}`. `created_at` istemci için epoch-ms'e çevriliyor.
    pub async fn list_notifications(
        &self,
        owner: &str,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, alert_id, kind, body, \
                    (EXTRACT(EPOCH FROM created_at) * 1000)::BIGINT AS created_at_ms, \
                    read_at IS NOT NULL AS read \
             FROM notifications WHERE owner = $1 \
             ORDER BY created_at DESC LIMIT $2",
        )
        .bind(owner)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.get::<i64, _>("id"),
                    "alert_id": r.get::<String, _>("alert_id"),
                    "kind": r.get::<String, _>("kind"),
                    "body": r.get::<serde_json::Value, _>("body"),
                    "created_at_ms": r.get::<i64, _>("created_at_ms"),
                    "read": r.get::<bool, _>("read"),
                })
            })
            .collect())
    }

    /// Kullanıcının okunmamış bildirimlerini okundu işaretle. İşaretlenen sayı.
    pub async fn mark_notifications_read(&self, owner: &str) -> Result<u64, StoreError> {
        let n = sqlx::query(
            "UPDATE notifications SET read_at = now() \
             WHERE owner = $1 AND read_at IS NULL",
        )
        .bind(owner)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(n)
    }

    /// Kullanıcının okunmamış bildirim sayısı — rozet için hafif sorgu.
    pub async fn unread_count(&self, owner: &str) -> Result<i64, StoreError> {
        let row = sqlx::query(
            "SELECT count(*) AS n FROM notifications WHERE owner = $1 AND read_at IS NULL",
        )
        .bind(owner)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<i64, _>("n"))
    }

    /// Kullanıcının iletişim kanallarını ayarla (e-posta / telegram chat id).
    ///
    /// Kısmi güncelleme: `None` verilen alan **değişmez** (`COALESCE`), böylece
    /// yalnız e-postayı güncellemek telegram'ı silmez. Kullanıcı yoksa oluşturur.
    pub async fn set_contact(
        &self,
        owner: &str,
        email: Option<&str>,
        telegram: Option<&str>,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO users (pubkey, email, telegram_chat_id) VALUES ($1, $2, $3) \
             ON CONFLICT (pubkey) DO UPDATE SET \
                email = COALESCE(EXCLUDED.email, users.email), \
                telegram_chat_id = COALESCE(EXCLUDED.telegram_chat_id, users.telegram_chat_id)",
        )
        .bind(owner)
        .bind(email)
        .bind(telegram)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Kullanıcının iletişim kanalları: `(email, telegram_chat_id)`.
    pub async fn get_contact(
        &self,
        owner: &str,
    ) -> Result<(Option<String>, Option<String>), StoreError> {
        let row = sqlx::query("SELECT email, telegram_chat_id FROM users WHERE pubkey = $1")
            .bind(owner)
            .fetch_optional(&self.pool)
            .await?;
        Ok(match row {
            Some(r) => (
                r.get::<Option<String>, _>("email"),
                r.get::<Option<String>, _>("telegram_chat_id"),
            ),
            None => (None, None),
        })
    }

    /// Belirli bir kanaldan henüz teslim edilmemiş bildirimler (sahibinin o kanalı
    /// dolu). `dest` = e-posta ya da telegram chat id. En eski önce.
    async fn undelivered(
        &self,
        contact_col: &str,
        sent_col: &str,
        limit: i64,
    ) -> Result<Vec<PendingDelivery>, StoreError> {
        // contact_col/sent_col sabit iç değerler (kullanıcı girdisi değil) →
        // SQL enjeksiyonu yok; sqlx bind kolon adına izin vermediği için format.
        let sql = format!(
            "SELECT n.id AS id, u.{contact_col} AS dest, n.body AS body \
             FROM notifications n JOIN users u ON n.owner = u.pubkey \
             WHERE n.{sent_col} IS NULL AND u.{contact_col} IS NOT NULL \
             ORDER BY n.created_at ASC LIMIT $1"
        );
        let rows = sqlx::query(&sql).bind(limit).fetch_all(&self.pool).await?;
        Ok(rows
            .iter()
            .map(|r| PendingDelivery {
                id: r.get::<i64, _>("id"),
                dest: r.get::<String, _>("dest"),
                body: r.get::<serde_json::Value, _>("body"),
            })
            .collect())
    }

    /// E-posta ile teslim bekleyen bildirimler.
    pub async fn undelivered_email(&self, limit: i64) -> Result<Vec<PendingDelivery>, StoreError> {
        self.undelivered("email", "email_sent_at", limit).await
    }

    /// Telegram ile teslim bekleyen bildirimler.
    pub async fn undelivered_telegram(
        &self,
        limit: i64,
    ) -> Result<Vec<PendingDelivery>, StoreError> {
        self.undelivered("telegram_chat_id", "telegram_sent_at", limit)
            .await
    }

    /// Bir bildirimi e-posta teslim edildi işaretle.
    pub async fn mark_email_sent(&self, id: i64) -> Result<(), StoreError> {
        sqlx::query("UPDATE notifications SET email_sent_at = now() WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Bir bildirimi telegram teslim edildi işaretle.
    pub async fn mark_telegram_sent(&self, id: i64) -> Result<(), StoreError> {
        sqlx::query("UPDATE notifications SET telegram_sent_at = now() WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

/// Teslim bekleyen tek bir bildirim: kime (`dest`) ve ne (`body`).
#[derive(Debug, Clone)]
pub struct PendingDelivery {
    pub id: i64,
    /// Hedef adres — e-posta ya da telegram chat id.
    pub dest: String,
    /// Bildirim gövdesi (`{symbol, message}`).
    pub body: serde_json::Value,
}

/// u64 (unix ms) → i64. Postgres BIGINT işaretli; 2262 yılına kadar taşma yok.
fn i64_of(v: u64) -> i64 {
    v as i64
}
