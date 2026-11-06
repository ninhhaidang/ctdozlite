//! Nhóm mẫu and Mẫu tin with the variables `{xưng_hô}` and `{tên}`.

use anyhow::{bail, Result};
use rusqlite::{params, Connection};
use unicode_normalization::UnicodeNormalization;

pub const BIEN_XUNG_HO: &str = "{xưng_hô}";
pub const BIEN_TEN: &str = "{tên}";

#[derive(Debug, Clone, PartialEq)]
pub struct Nhom {
    pub id: i64,
    pub ten: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Mau {
    pub id: i64,
    pub noi_dung: String,
}

/// NFC: "Unicode tổ hợp" typing and text pasted from macOS is NFD, which looks identical
/// but would not match `{tên}`.
pub fn nfc(s: &str) -> String {
    s.nfc().collect()
}

/// Unsupported `{…}` variables (usually typos), to be refused before saving.
pub fn bien_khong_hop_le(noi_dung: &str) -> Vec<String> {
    let nd = nfc(noi_dung);
    let mut sai = Vec::new();
    let mut tu = 0;
    while let Some(m) = nd[tu..].find('{') {
        let bat_dau = tu + m;
        let sau = &nd[bat_dau + 1..];
        match sau.find(['{', '}']) {
            Some(k) if sau[k..].starts_with('}') => {
                let bien = &nd[bat_dau..bat_dau + k + 2];
                if bien != BIEN_XUNG_HO && bien != BIEN_TEN && !sai.iter().any(|s| s == bien) {
                    sai.push(bien.to_string());
                }
                tu = bat_dau + k + 2;
            }
            Some(k) => tu = bat_dau + 1 + k, // a new '{' restarts the match
            None => break,
        }
    }
    sai.sort();
    sai
}

/// A Khách hàng without a name only matches Mẫu tin that do not contain `{tên}`.
pub fn dung_duoc_cho(noi_dung: &str, ten: Option<&str>) -> bool {
    ten.is_some_and(|t| !t.is_empty()) || !nfc(noi_dung).contains(BIEN_TEN)
}

/// `{tên}` only ever comes from the real name, never from the Zalo display name.
pub fn dien_mau(noi_dung: &str, xung_ho: &str, ten: Option<&str>) -> Result<String> {
    if !dung_duoc_cho(noi_dung, ten) {
        bail!("Khách hàng chưa có tên, không dùng được Mẫu tin chứa {BIEN_TEN}");
    }
    Ok(nfc(noi_dung)
        .replace(BIEN_XUNG_HO, xung_ho)
        .replace(BIEN_TEN, ten.unwrap_or("")))
}

fn kiem_tra(noi_dung: &str) -> Result<()> {
    if noi_dung.trim().is_empty() {
        bail!("Mẫu tin trống");
    }
    let sai = bien_khong_hop_le(noi_dung);
    if !sai.is_empty() {
        bail!(
            "Biến không hỗ trợ: {}. Chỉ dùng {BIEN_XUNG_HO} và {BIEN_TEN}.",
            sai.join(", ")
        );
    }
    Ok(())
}

pub fn danh_sach_nhom(conn: &Connection) -> Result<Vec<Nhom>> {
    let mut st = conn.prepare("SELECT id, ten FROM nhom_mau ORDER BY ten")?;
    let rows = st.query_map([], |r| {
        Ok(Nhom {
            id: r.get(0)?,
            ten: r.get(1)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn ten_nhom(ten: &str) -> Result<String> {
    let ten = nfc(ten.trim());
    if ten.is_empty() {
        bail!("Tên Nhóm mẫu trống");
    }
    Ok(ten)
}

fn loi_trung_ten(e: rusqlite::Error) -> anyhow::Error {
    match e {
        rusqlite::Error::SqliteFailure(f, _)
            if f.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            anyhow::anyhow!("Đã có Nhóm mẫu tên này")
        }
        e => e.into(),
    }
}

pub fn them_nhom(conn: &Connection, ten: &str) -> Result<()> {
    let ten = ten_nhom(ten)?;
    conn.execute("INSERT INTO nhom_mau (ten) VALUES (?)", [ten])
        .map_err(loi_trung_ten)?;
    Ok(())
}

pub fn doi_ten_nhom(conn: &Connection, id: i64, ten: &str) -> Result<()> {
    let ten = ten_nhom(ten)?;
    conn.execute("UPDATE nhom_mau SET ten = ? WHERE id = ?", params![ten, id])
        .map_err(loi_trung_ten)?;
    Ok(())
}

pub fn xoa_nhom(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM nhom_mau WHERE id = ?", [id])?;
    Ok(())
}

pub fn danh_sach_mau(conn: &Connection, nhom_id: i64) -> Result<Vec<Mau>> {
    let mut st =
        conn.prepare("SELECT id, noi_dung FROM mau_tin WHERE nhom_mau_id = ? ORDER BY id")?;
    let rows = st.query_map([nhom_id], |r| {
        Ok(Mau {
            id: r.get(0)?,
            noi_dung: r.get(1)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn them_mau(conn: &Connection, nhom_id: i64, noi_dung: &str) -> Result<()> {
    kiem_tra(noi_dung)?;
    conn.execute(
        "INSERT INTO mau_tin (nhom_mau_id, noi_dung) VALUES (?, ?)",
        params![nhom_id, nfc(noi_dung)],
    )
    .map_err(|e| match e {
        // The group was deleted meanwhile.
        rusqlite::Error::SqliteFailure(f, _)
            if f.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            anyhow::anyhow!("Nhóm mẫu không còn tồn tại")
        }
        e => e.into(),
    })?;
    Ok(())
}

pub fn sua_mau(conn: &Connection, id: i64, noi_dung: &str) -> Result<()> {
    kiem_tra(noi_dung)?;
    conn.execute(
        "UPDATE mau_tin SET noi_dung = ? WHERE id = ?",
        params![nfc(noi_dung), id],
    )?;
    Ok(())
}

pub fn xoa_mau(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM mau_tin WHERE id = ?", [id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cskh::db;

    #[test]
    fn unknown_variables_are_reported_once_and_known_ones_pass() {
        assert!(bien_khong_hop_le("Chào {xưng_hô} {tên}").is_empty());
        assert_eq!(
            bien_khong_hop_le("{ten} {ten} {xưng_hô} {{tên}"),
            vec!["{ten}"]
        );
        assert_eq!(bien_khong_hop_le("{a} {b} {"), vec!["{a}", "{b}"]);
    }

    #[test]
    fn nfd_input_behaves_like_nfc() {
        let nfd: String = "Chào {xưng_hô} {tên}".nfd().collect();
        assert!(bien_khong_hop_le(&nfd).is_empty());
        assert_eq!(dien_mau(&nfd, "chị", Some("Lan")).unwrap(), "Chào chị Lan");
        assert!(!dung_duoc_cho(&nfd, None));
    }

    #[test]
    fn nameless_customer_only_gets_templates_without_ten() {
        assert!(dung_duoc_cho("Chào {xưng_hô}", None));
        assert!(!dung_duoc_cho("Chào {tên}", None));
        assert!(!dung_duoc_cho("Chào {tên}", Some("")));
        assert!(dien_mau("Chào {tên}", "anh", None).is_err());
        assert_eq!(dien_mau("Chào {xưng_hô}", "anh", None).unwrap(), "Chào anh");
    }

    #[test]
    fn saving_validates_and_groups_are_unique() {
        let conn = db::mo_bo_nho().unwrap();
        them_nhom(&conn, " Hết hàng ").unwrap();
        assert!(them_nhom(&conn, "Hết hàng").is_err());
        let nhom = danh_sach_nhom(&conn).unwrap();
        assert_eq!(nhom[0].ten, "Hết hàng");
        assert!(them_mau(&conn, nhom[0].id, "Chào {ten}").is_err());
        assert!(them_mau(&conn, nhom[0].id, "  ").is_err());
        them_mau(&conn, nhom[0].id, "Chào {xưng_hô} {tên}").unwrap();
        assert!(them_mau(&conn, 999, "Chào").is_err());
        let mau = danh_sach_mau(&conn, nhom[0].id).unwrap();
        assert!(sua_mau(&conn, mau[0].id, "{x}").is_err());
        xoa_nhom(&conn, nhom[0].id).unwrap();
        assert!(danh_sach_mau(&conn, nhom[0].id).unwrap().is_empty());
    }
}
