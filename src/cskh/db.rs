//! Per-account SQLite file `accounts/{uin}_cskh.db`, separate from ctdozlite's history file.

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

/// Forward-only migrations tracked by `PRAGMA user_version` (docs/standards/database-and-migrations.md).
/// Dates are ISO text (`yyyy-mm-dd`, `yyyy-mm-ddThh:mm:ss` local time) so they compare as strings;
/// this deviates from the epoch-ms convention of `store.rs` on purpose, to stay identical to v0.1.0.
const MIGRATIONS: &[&str] = &["
CREATE TABLE khach_hang (
    id INTEGER PRIMARY KEY,
    so_dien_thoai TEXT UNIQUE,      -- normalised 0xxxxxxxxx
    ten TEXT,                       -- real name, used for {tên}
    xung_ho TEXT NOT NULL DEFAULT 'anh/chị',
    ngay_mua_gan_nhat TEXT,
    zalo_id TEXT UNIQUE,            -- NULL = not linked to Zalo yet
    ten_zalo TEXT                   -- Zalo display name: identification only, NEVER used as {tên}
);
CREATE TABLE nhom_mau (id INTEGER PRIMARY KEY, ten TEXT NOT NULL UNIQUE);
CREATE TABLE mau_tin (
    id INTEGER PRIMARY KEY,
    nhom_mau_id INTEGER NOT NULL REFERENCES nhom_mau(id) ON DELETE CASCADE,
    noi_dung TEXT NOT NULL
);
CREATE TABLE dot_gui (
    id INTEGER PRIMARY KEY,
    nhom_mau_id INTEGER REFERENCES nhom_mau(id) ON DELETE SET NULL,
    tao_luc TEXT NOT NULL,
    trang_thai TEXT NOT NULL DEFAULT 'nhap',
    loi TEXT
);
-- One row per message of a Đợt gửi; a row in state 'da_gui' is a Lần hỏi thăm.
CREATE TABLE tin_gui (
    id INTEGER PRIMARY KEY,
    dot_gui_id INTEGER NOT NULL REFERENCES dot_gui(id) ON DELETE CASCADE,
    khach_hang_id INTEGER NOT NULL REFERENCES khach_hang(id),
    zalo_id TEXT NOT NULL,
    noi_dung TEXT NOT NULL,
    chon INTEGER NOT NULL DEFAULT 1,
    trang_thai TEXT NOT NULL DEFAULT 'cho',
    gui_luc TEXT,                   -- set when the slot is reserved, before Zalo is called
    loi TEXT
);
-- Used by the daily cap, the 30-day rule and the spacing check.
CREATE INDEX idx_tin_gui_gui_luc ON tin_gui(gui_luc);
CREATE TABLE cai_dat (khoa TEXT PRIMARY KEY, gia_tri TEXT NOT NULL);
"];

pub fn duong_dan(uin: &str) -> PathBuf {
    let uin: String = uin.chars().filter(char::is_ascii_alphanumeric).collect();
    crate::account_manager::accounts_dir().join(format!("{uin}_cskh.db"))
}

/// Open (creating and migrating) the database file.
pub fn mo(path: &Path) -> Result<Connection> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).context("Không tạo được thư mục accounts")?;
    }
    khoi_tao(Connection::open(path).context("Không mở được dữ liệu chăm sóc khách hàng")?)
}

pub fn mo_bo_nho() -> Result<Connection> {
    khoi_tao(Connection::open_in_memory()?)
}

fn khoi_tao(mut conn: Connection) -> Result<Connection> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;
         PRAGMA busy_timeout = 5000; PRAGMA foreign_keys = ON;",
    )?;
    let version: usize = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version > MIGRATIONS.len() {
        bail!("Dữ liệu chăm sóc khách hàng được tạo bởi bản mới hơn, hãy cập nhật ứng dụng.");
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(version) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
        tx.commit()?;
    }
    Ok(conn)
}

pub fn doc_cai_dat(conn: &Connection, khoa: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT gia_tri FROM cai_dat WHERE khoa = ?", [khoa], |r| {
            r.get(0)
        })
        .optional()?)
}

pub fn luu_cai_dat(conn: &Connection, khoa: &str, gia_tri: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO cai_dat (khoa, gia_tri) VALUES (?1, ?2)
         ON CONFLICT(khoa) DO UPDATE SET gia_tri = excluded.gia_tri",
        params![khoa, gia_tri],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_idempotent_and_newer_versions_are_refused() {
        let dir = std::env::temp_dir().join(format!("cskh-db-{}", std::process::id()));
        let path = dir.join("1_cskh.db");
        drop(mo(&path).unwrap());
        let conn = mo(&path).unwrap();
        luu_cai_dat(&conn, "toi_da_ngay", "50").unwrap();
        assert_eq!(
            doc_cai_dat(&conn, "toi_da_ngay").unwrap().as_deref(),
            Some("50")
        );
        conn.execute_batch("PRAGMA user_version = 99").unwrap();
        drop(conn);
        assert!(mo(&path).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
