-- Notify = uygulama-içi bildirim outbox'ı (+ opsiyonel iletişim kanalları).
--
-- Zincir zaten vardı: NL "notify me…" → store → watcher koşulu tutunca
-- `Tick.notified`. Eksik olan tek şey bildirimin KALICILAŞTIRILIP kullanıcıya
-- sunulmasıydı. Watcher bir Notify koşulu tutunca buraya bir satır yazar;
-- in-app anında görünür. E-posta/telegram teslimi (sonraki faz) ilgili
-- `*_sent_at` kolonlarını doldurur.

-- İletişim kanalları — kullanıcı verirse Notify oraya da düşer. Yoksa yalnız
-- in-app. Şimdilik yalnız şema; teslim döngüsü sonraki fazda.
ALTER TABLE users
    ADD COLUMN email            TEXT,
    ADD COLUMN telegram_chat_id TEXT;

CREATE TABLE notifications (
    id                BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    owner             TEXT NOT NULL,       -- FK yok: kullanıcı silinse de iz kalsın (audit gibi)
    alert_id          TEXT NOT NULL,
    kind              TEXT NOT NULL,       -- 'fired' (şimdilik); ileride 'missed' vb.
    body              JSONB NOT NULL,      -- {symbol, message} — in-app render için
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    read_at           TIMESTAMPTZ,         -- in-app "okundu"
    email_sent_at     TIMESTAMPTZ,         -- teslim döngüsü doldurur (sonraki faz)
    telegram_sent_at  TIMESTAMPTZ,
    -- Bir alarm-olayı yalnız bir kez bildirilir; watcher aynı turu çökme sonrası
    -- tekrar işlese de çift satır olmasın (idempotent kayıt).
    UNIQUE (alert_id, kind)
);

-- Kullanıcının bildirimlerini en yeni önce çekmek + okunmamış saymak için.
CREATE INDEX notifications_owner ON notifications (owner, created_at DESC);
