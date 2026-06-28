use chrono::Utc;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqliteSynchronous};
use sqlx::Row;

use crate::config::Reset;
use crate::error::Result;

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

pub struct UsageRow {
    pub used: i64,
    pub exhausted: bool,
}

/// One recorded router decision (for the dashboard's live route log + history).
pub struct RouteRow {
    pub id: i64,
    pub ts: String,
    pub capability: String,
    pub provider: String,
    pub label: String,
    pub status: i64,
    pub latency_ms: i64,
    pub fail_from: Option<String>,
    pub fail_code: Option<i64>,
}

/// What the router hands to `log_route` after each call (success, with optional failover origin).
pub struct RouteLog<'a> {
    pub capability: &'a str,
    pub provider: &'a str,
    pub label: &'a str,
    pub status: i64,
    pub latency_ms: i64,
    pub fail_from: Option<&'a str>,
    pub fail_code: Option<i64>,
}

impl Store {
    pub async fn open(path: &str) -> Result<Self> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            // WAL so readers don't block the writer when several processes share this file.
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal);
        let pool = SqlitePool::connect_with(opts).await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS usage (
                provider  TEXT    NOT NULL,
                label     TEXT    NOT NULL,
                period    TEXT    NOT NULL,
                used      INTEGER NOT NULL DEFAULT 0,
                exhausted INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (label, period)
            )",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS proxy_assignment (
                label TEXT PRIMARY KEY,
                proxy TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS web_session (
                label    TEXT PRIMARY KEY,
                provider TEXT NOT NULL,
                cookies  TEXT NOT NULL,
                updated  TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS route_log (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                ts         TEXT    NOT NULL,
                capability TEXT    NOT NULL,
                provider   TEXT    NOT NULL,
                label      TEXT    NOT NULL,
                status     INTEGER NOT NULL,
                latency_ms INTEGER NOT NULL,
                fail_from  TEXT,
                fail_code  INTEGER
            )",
        )
        .execute(&pool)
        .await?;
        Ok(Self { pool })
    }

    pub async fn save_session(&self, label: &str, provider: &str, cookies: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO web_session (label, provider, cookies, updated) VALUES (?, ?, ?, ?)
             ON CONFLICT(label) DO UPDATE SET cookies = excluded.cookies, updated = excluded.updated",
        )
        .bind(label)
        .bind(provider)
        .bind(cookies)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn load_session(&self, label: &str) -> Result<Option<String>> {
        let row = sqlx::query("SELECT cookies FROM web_session WHERE label = ?")
            .bind(label)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get("cookies")))
    }

    /// Remove all rows for an account (usage, proxy assignment, web session).
    pub async fn delete_account(&self, label: &str) -> Result<()> {
        sqlx::query("DELETE FROM usage WHERE label = ?")
            .bind(label)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM proxy_assignment WHERE label = ?")
            .bind(label)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM web_session WHERE label = ?")
            .bind(label)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn remaining(&self, label: &str, quota: i64, period: &str) -> Result<i64> {
        let u = self.usage_for(label, period).await?;
        Ok(if u.exhausted {
            0
        } else {
            (quota - u.used).max(0)
        })
    }

    pub async fn usage_for(&self, label: &str, period: &str) -> Result<UsageRow> {
        let row = sqlx::query("SELECT used, exhausted FROM usage WHERE label = ? AND period = ?")
            .bind(label)
            .bind(period)
            .fetch_optional(&self.pool)
            .await?;
        Ok(match row {
            Some(r) => UsageRow {
                used: r.get("used"),
                exhausted: r.get::<i64, _>("exhausted") != 0,
            },
            None => UsageRow {
                used: 0,
                exhausted: false,
            },
        })
    }

    /// Atomically claim `cost` units, or return `false` if granting it would exceed `quota` (or the
    /// account is exhausted) — closes the read-then-write race where concurrent callers all clear
    /// the same `remaining > 0` gate. Caller must `refund` if the reserved attempt then fails.
    pub async fn reserve(
        &self,
        provider: &str,
        label: &str,
        quota: i64,
        period: &str,
        cost: i64,
    ) -> Result<bool> {
        let res = sqlx::query(
            "INSERT INTO usage (provider, label, period, used) VALUES (?, ?, ?, ?)
             ON CONFLICT(label, period) DO UPDATE SET used = used + excluded.used
                WHERE exhausted = 0 AND used + excluded.used <= ?",
        )
        .bind(provider)
        .bind(label)
        .bind(period)
        .bind(cost)
        .bind(quota)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Hand back a reserved `cost` after the attempt it was claimed for failed.
    pub async fn refund(&self, label: &str, period: &str, cost: i64) -> Result<()> {
        sqlx::query("UPDATE usage SET used = max(used - ?, 0) WHERE label = ? AND period = ?")
            .bind(cost)
            .bind(label)
            .bind(period)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn record(&self, provider: &str, label: &str, period: &str, cost: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO usage (provider, label, period, used) VALUES (?, ?, ?, ?)
             ON CONFLICT(label, period) DO UPDATE SET used = used + excluded.used",
        )
        .bind(provider)
        .bind(label)
        .bind(period)
        .bind(cost)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn mark_exhausted(&self, provider: &str, label: &str, period: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO usage (provider, label, period, used, exhausted) VALUES (?, ?, ?, 0, 1)
             ON CONFLICT(label, period) DO UPDATE SET exhausted = 1",
        )
        .bind(provider)
        .bind(label)
        .bind(period)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn assign_proxy(&self, label: &str, proxy: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO proxy_assignment (label, proxy) VALUES (?, ?)
             ON CONFLICT(label) DO NOTHING",
        )
        .bind(label)
        .bind(proxy)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn assignment_count(&self) -> Result<i64> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM proxy_assignment")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get("n"))
    }

    pub async fn proxy_for(&self, label: &str) -> Result<Option<String>> {
        let row = sqlx::query("SELECT proxy FROM proxy_assignment WHERE label = ?")
            .bind(label)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get("proxy")))
    }

    pub async fn log_route(&self, e: &RouteLog<'_>) -> Result<()> {
        let res = sqlx::query(
            "INSERT INTO route_log
                (ts, capability, provider, label, status, latency_ms, fail_from, fail_code)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(e.capability)
        .bind(e.provider)
        .bind(e.label)
        .bind(e.status)
        .bind(e.latency_ms)
        .bind(e.fail_from)
        .bind(e.fail_code)
        .execute(&self.pool)
        .await?;
        // Keep the table bounded (~last 1000 rows), amortized so it isn't run every call.
        if res.last_insert_rowid() % 200 == 0 {
            sqlx::query("DELETE FROM route_log WHERE id <= ?")
                .bind(res.last_insert_rowid() - 1000)
                .execute(&self.pool)
                .await
                .ok();
        }
        Ok(())
    }

    pub async fn recent_routes(&self, limit: i64) -> Result<Vec<RouteRow>> {
        let rows = sqlx::query(
            "SELECT id, ts, capability, provider, label, status, latency_ms, fail_from, fail_code
             FROM route_log ORDER BY id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        let mut out: Vec<RouteRow> = rows.iter().map(route_row).collect();
        out.reverse(); // chronological (oldest first)
        Ok(out)
    }

    pub async fn routes_since(&self, after_id: i64, limit: i64) -> Result<Vec<RouteRow>> {
        let rows = sqlx::query(
            "SELECT id, ts, capability, provider, label, status, latency_ms, fail_from, fail_code
             FROM route_log WHERE id > ? ORDER BY id ASC LIMIT ?",
        )
        .bind(after_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(route_row).collect())
    }

    pub async fn max_route_id(&self) -> Result<i64> {
        let row = sqlx::query("SELECT COALESCE(MAX(id), 0) AS m FROM route_log")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get("m"))
    }
}

fn route_row(r: &sqlx::sqlite::SqliteRow) -> RouteRow {
    RouteRow {
        id: r.get("id"),
        ts: r.get("ts"),
        capability: r.get("capability"),
        provider: r.get("provider"),
        label: r.get("label"),
        status: r.get("status"),
        latency_ms: r.get("latency_ms"),
        fail_from: r.get("fail_from"),
        fail_code: r.get("fail_code"),
    }
}

pub fn period_key(reset: Reset) -> String {
    match reset {
        Reset::Monthly => Utc::now().format("%Y-%m").to_string(),
        Reset::Daily => Utc::now().format("%Y-%m-%d").to_string(),
        Reset::Once => "lifetime".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn route_log_roundtrips() {
        // A temp file, not ":memory:" — each pooled connection gets its own in-memory db.
        let path = std::env::temp_dir().join(format!("fetchira_route_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = Store::open(path.to_str().unwrap()).await.unwrap();
        assert_eq!(store.max_route_id().await.unwrap(), 0);

        store
            .log_route(&RouteLog {
                capability: "search",
                provider: "serper",
                label: "serper-1",
                status: 200,
                latency_ms: 198,
                fail_from: None,
                fail_code: None,
            })
            .await
            .unwrap();
        store
            .log_route(&RouteLog {
                capability: "search",
                provider: "tavily",
                label: "tavily-1",
                status: 200,
                latency_ms: 312,
                fail_from: Some("exa-1"),
                fail_code: Some(429),
            })
            .await
            .unwrap();

        let recent = store.recent_routes(10).await.unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].label, "serper-1"); // chronological: oldest first
        assert_eq!(recent[1].fail_from.as_deref(), Some("exa-1"));

        let since = store.routes_since(recent[0].id, 10).await.unwrap();
        assert_eq!(since.len(), 1);
        assert_eq!(since[0].label, "tavily-1");
        assert_eq!(store.max_route_id().await.unwrap(), recent[1].id);

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn reserve_gates_quota() {
        let path = std::env::temp_dir().join(format!("fetchira_reserve_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = Store::open(path.to_str().unwrap()).await.unwrap();

        // Quota 3: the first three reservations win, the fourth is denied.
        for _ in 0..3 {
            assert!(store
                .reserve("grok_web", "grok-1#dr", 3, "d", 1)
                .await
                .unwrap());
        }
        assert!(!store
            .reserve("grok_web", "grok-1#dr", 3, "d", 1)
            .await
            .unwrap());
        assert_eq!(store.remaining("grok-1#dr", 3, "d").await.unwrap(), 0);

        // A refund frees exactly one slot back.
        store.refund("grok-1#dr", "d", 1).await.unwrap();
        assert!(store
            .reserve("grok_web", "grok-1#dr", 3, "d", 1)
            .await
            .unwrap());

        // Once exhausted, no reservation succeeds even with budget refunded.
        store.refund("grok-1#dr", "d", 1).await.unwrap();
        store
            .mark_exhausted("grok_web", "grok-1#dr", "d")
            .await
            .unwrap();
        assert!(!store
            .reserve("grok_web", "grok-1#dr", 3, "d", 1)
            .await
            .unwrap());

        let _ = std::fs::remove_file(&path);
    }
}
