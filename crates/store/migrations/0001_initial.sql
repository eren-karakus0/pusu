-- PUSU kalıcı durumu.
--
-- Tasarımı belirleyen ölçüm (§8.11): ön-imzalı blob nonce sayesinde
-- idempotent, yani çift-giriş koruması için write-ahead GEREKMİYOR. Ama çökme
-- sonrası "gönderdim mi?" sorusunu cevaplamak için gönderim niyeti ile
-- entry_oid kalıcı olmalı — mutabakat SORGU ile yapılıyor (openOrders'ı oid
-- için sorgula), körlemesine tekrar gönderimle değil.

-- Alarmın yaşam döngüsü. pusu_core::AlertState ile birebir.
CREATE TYPE alert_state AS ENUM (
    'armed',      -- koşul bekleniyor
    'working',    -- limit giriş deftere kondu, dolum bekleniyor
    'fired',      -- emir gönderildi/doldu
    'cancelled',  -- kullanıcı ya da iptal koşulu düşürdü
    'rejected',   -- borsa reddetti (ör. yetersiz marjin)
    'uncertain',  -- gönderildi ama sonuç bilinmiyor — insan bakmalı
    'missed'      -- kaçırıldı; "hâlâ istiyor musun?" soruldu
);

-- Bir kullanıcı. Master hesap = kimlik; builder onayı burada duruyor.
CREATE TABLE users (
    pubkey      TEXT PRIMARY KEY,           -- master EOA, base58
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Bir alarm. condition/action/invalidate/exits domain nesneleri JSONB olarak
-- taşınıyor: şekilleri Rust'ta (pusu_core) yaşıyor ve hızla evriliyor; SQL
-- şemasına kopyalamak iki yerde bakım demek olurdu. Sorgular yalnızca skaler
-- kolonlara (owner, account, state, deadline) dokunuyor.
CREATE TABLE alerts (
    id                TEXT PRIMARY KEY,
    owner             TEXT NOT NULL REFERENCES users(pubkey),
    account           TEXT NOT NULL,        -- işlemin gireceği sub-account
    state             alert_state NOT NULL DEFAULT 'armed',
    condition         JSONB NOT NULL,
    invalidate        JSONB,                -- opsiyonel iptal koşulu
    action            JSONB NOT NULL,
    armed_at_ms       BIGINT NOT NULL,
    entry_oid         TEXT,                 -- imza anında hesaplanan oid (§8.9)
    fill_deadline_ms  BIGINT,               -- limit girişin dolum sınırı
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Watcher her tur yalnızca canlı alarmlara bakıyor (armed/working). Kısmi
-- indeks: terminal alarmlar biriktikçe tarama maliyeti artmasın.
CREATE INDEX alerts_live ON alerts (state) WHERE state IN ('armed', 'working');

-- Ön-imzalı tx blob'ları. Kullanıcının tarayıcıda imzaladığı, DEĞİŞTİRİLEMEZ
-- paketler. Sunucuda imza yetkisi yok; watcher yalnızca zamanı gelince
-- postalıyor.
--
-- Alarm başına İKİ blob: giriş ve (limit ise) onun iptali. İptal, girişin
-- oid'ine bağlı ayrı bir imza — kullanıcı ikisini birlikte imzalıyor (§8.9).
CREATE TYPE blob_role AS ENUM ('entry', 'cancel');

CREATE TABLE presigned_blobs (
    alert_id    TEXT NOT NULL REFERENCES alerts(id) ON DELETE CASCADE,
    role        blob_role NOT NULL,
    nonce       BIGINT NOT NULL,            -- idempotency anahtarı (§8.11)
    payload     JSONB NOT NULL,             -- {actions, nonce, account, signer, signature}
    -- Gönderim niyeti: gönderMEDEN önce set ediliyor. Çökme sonrası "bu blob'u
    -- postaladım mı?" sorusunun cevabı. NULL = hiç gönderilmedi.
    dispatched_at  TIMESTAMPTZ,
    PRIMARY KEY (alert_id, role)
);

-- Değişmez denetim kaydı. Her durum geçişi ve her borsa yanıtı buraya
-- yazılıyor; "alarmım neden ateşledi/ateşlemedi" sorusunun tek doğru kaynağı.
-- Yalnızca INSERT — asla UPDATE/DELETE.
CREATE TABLE audit_log (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    alert_id    TEXT NOT NULL,             -- FK yok: alarm silinse de iz kalmalı
    at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    kind        TEXT NOT NULL,             -- 'state_change' | 'dispatch' | 'exchange_response' | ...
    detail      JSONB NOT NULL
);

CREATE INDEX audit_log_alert ON audit_log (alert_id, at);
