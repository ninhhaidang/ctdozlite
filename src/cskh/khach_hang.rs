//! Khách hàng: phone normalisation, Excel import, adding from 1-1 conversations, editing.

use anyhow::{bail, Result};
use calamine::{open_workbook, Data, Reader, Xlsx};
use chrono::NaiveDate;
use rusqlite::{named_params, params, Connection, Row};
use std::path::Path;
use unicode_normalization::UnicodeNormalization;

pub const XUNG_HO_MAC_DINH: &str = "anh/chị";
pub const COT_EXCEL: [&str; 4] = ["Số điện thoại", "Tên", "Xưng hô", "Ngày mua gần nhất"];

#[derive(Debug, Clone, PartialEq)]
pub struct KhachHang {
    pub id: i64,
    pub so_dien_thoai: Option<String>,
    pub ten: Option<String>,
    pub xung_ho: String,
    pub ngay_mua_gan_nhat: Option<String>,
    pub zalo_id: Option<String>,
    pub ten_zalo: Option<String>,
}

pub const COT_SQL: &str = "id, so_dien_thoai, ten, xung_ho, ngay_mua_gan_nhat, zalo_id, ten_zalo";

pub fn tu_dong(r: &Row) -> rusqlite::Result<KhachHang> {
    Ok(KhachHang {
        id: r.get(0)?,
        so_dien_thoai: r.get(1)?,
        ten: r.get(2)?,
        xung_ho: r.get(3)?,
        ngay_mua_gan_nhat: r.get(4)?,
        zalo_id: r.get(5)?,
        ten_zalo: r.get(6)?,
    })
}

impl KhachHang {
    /// What to show the seller: the real name, else the Zalo display name, else the phone.
    pub fn ten_hien(&self) -> &str {
        [&self.ten, &self.ten_zalo, &self.so_dien_thoai]
            .into_iter()
            .flatten()
            .next()
            .map_or("(chưa có tên)", String::as_str)
    }
}

/// A spreadsheet cell, reduced to what the import needs.
#[derive(Debug, Clone, PartialEq)]
pub enum O {
    Trong,
    Chu(String),
    So(f64),
    Ngay(NaiveDate),
}

impl O {
    fn chu(&self) -> Option<String> {
        let s = match self {
            O::Trong => return None,
            O::Chu(s) => s.clone(),
            O::So(f) if f.fract() == 0.0 => format!("{}", *f as i64),
            O::So(f) => f.to_string(),
            O::Ngay(d) => d.format("%Y-%m-%d").to_string(),
        };
        Some(s.trim().nfc().collect::<String>()).filter(|s| !s.is_empty())
    }
}

/// Normalise to 0xxxxxxxxx (10 digits); `None` when it is not a valid Vietnamese mobile number.
pub fn chuan_hoa_so_dien_thoai(gia_tri: &str) -> Option<String> {
    let mut so: String = gia_tri.chars().filter(char::is_ascii_digit).collect();
    if so.len() == 11 && so.starts_with("84") {
        so = format!("0{}", &so[2..]);
    } else if so.len() == 9 && !so.starts_with('0') {
        so.insert(0, '0'); // Excel stores numbers and drops the leading zero
    }
    (so.len() == 10 && so.starts_with('0')).then_some(so)
}

fn ngay(o: &O) -> Result<Option<String>, String> {
    if let O::Ngay(d) = o {
        return Ok(Some(d.format("%Y-%m-%d").to_string()));
    }
    let Some(s) = o.chu() else { return Ok(None) };
    if !matches!(o, O::So(_)) {
        for dinh_dang in ["%d/%m/%Y", "%Y-%m-%d", "%d-%m-%Y"] {
            if let Ok(d) = NaiveDate::parse_from_str(&s, dinh_dang) {
                return Ok(Some(d.format("%Y-%m-%d").to_string()));
            }
        }
    }
    Err(format!("ngày mua không hiểu được: {s:?}"))
}

#[derive(Debug, Default, PartialEq)]
pub struct KetQuaNhap {
    pub so_dong: usize,
    pub loi: Vec<String>,
}

fn loi_file() -> KetQuaNhap {
    KetQuaNhap {
        so_dong: 0,
        loi: vec!["Không đọc được file Excel. Hãy lưu lại dưới dạng .xlsx rồi thử lại.".into()],
    }
}

fn o_tu_calamine(d: &Data) -> O {
    match d {
        Data::Empty => O::Trong,
        Data::Int(i) => O::So(*i as f64),
        Data::Float(f) => O::So(*f),
        Data::DateTime(dt) => {
            // Excel serial day count; the 1900 leap-year quirk is irrelevant after March 1900.
            let goc = NaiveDate::from_ymd_opt(1899, 12, 30).expect("valid date");
            match goc.checked_add_signed(chrono::Duration::days(dt.as_f64().floor() as i64)) {
                Some(d) => O::Ngay(d),
                None => O::Chu(format!("{}", dt.as_f64())),
            }
        }
        Data::DateTimeIso(s) => O::Chu(s.split('T').next().unwrap_or(s).to_string()),
        other => O::Chu(other.to_string()),
    }
}

/// Import or update Khách hàng from the first sheet of an .xlsx file.
pub fn nhap_excel(conn: &Connection, file: &Path) -> KetQuaNhap {
    let Ok(mut wb) = open_workbook::<Xlsx<_>, _>(file) else {
        return loi_file();
    };
    let Some(Ok(range)) = wb.worksheet_range_at(0) else {
        return loi_file();
    };
    let dong: Vec<Vec<O>> = range
        .rows()
        .map(|r| r.iter().map(o_tu_calamine).collect())
        .collect();
    nhap_cac_dong(conn, &dong)
}

/// Re-import rules: a blank cell keeps the stored value, the purchase date only moves forward.
pub fn nhap_cac_dong(conn: &Connection, dong: &[Vec<O>]) -> KetQuaNhap {
    let tieu_de: Vec<String> = dong
        .first()
        .map(|r| r.iter().map(|c| c.chu().unwrap_or_default()).collect())
        .unwrap_or_default();
    let thieu: Vec<&str> = COT_EXCEL
        .into_iter()
        .filter(|c| !tieu_de.iter().any(|t| t == c))
        .collect();
    if !thieu.is_empty() {
        return KetQuaNhap {
            so_dong: 0,
            loi: vec![format!("Thiếu cột: {}", thieu.join(", "))],
        };
    }
    let vi_tri = COT_EXCEL.map(|c| tieu_de.iter().position(|t| t == c).expect("checked above"));
    let mut kq = KetQuaNhap::default();
    let ghi = (|| -> Result<()> {
        let tx = conn.unchecked_transaction()?;
        for (i, row) in dong.iter().enumerate().skip(1) {
            let o = |k: usize| row.get(vi_tri[k]).unwrap_or(&O::Trong);
            let (sdt_goc, ten, xung_ho, ngay_mua) = (o(0), o(1), o(2), o(3));
            if [sdt_goc, ten, xung_ho, ngay_mua]
                .iter()
                .all(|c| c.chu().is_none())
            {
                continue;
            }
            let so_thu_tu = i + 1;
            let Some(sdt) = sdt_goc.chu().and_then(|s| chuan_hoa_so_dien_thoai(&s)) else {
                kq.loi.push(format!(
                    "Dòng {so_thu_tu}: số điện thoại không hợp lệ ({:?})",
                    sdt_goc.chu().unwrap_or_default()
                ));
                continue;
            };
            let ngay_iso = match ngay(ngay_mua) {
                Ok(n) => n,
                Err(e) => {
                    kq.loi.push(format!("Dòng {so_thu_tu}: {e}"));
                    continue;
                }
            };
            tx.execute(
                "INSERT INTO khach_hang (so_dien_thoai, ten, xung_ho, ngay_mua_gan_nhat)
                 VALUES (:sdt, :ten, COALESCE(:xung_ho, :mac_dinh), :ngay)
                 ON CONFLICT(so_dien_thoai) DO UPDATE SET
                   ten = COALESCE(:ten, ten), xung_ho = COALESCE(:xung_ho, xung_ho),
                   ngay_mua_gan_nhat = CASE WHEN ngay_mua_gan_nhat IS NULL OR :ngay > ngay_mua_gan_nhat
                     THEN COALESCE(:ngay, ngay_mua_gan_nhat) ELSE ngay_mua_gan_nhat END",
                named_params! {
                    ":sdt": sdt, ":ten": ten.chu(), ":xung_ho": xung_ho.chu(),
                    ":mac_dinh": XUNG_HO_MAC_DINH, ":ngay": ngay_iso,
                },
            )?;
            kq.so_dong += 1;
        }
        Ok(tx.commit()?)
    })();
    if let Err(e) = ghi {
        return KetQuaNhap {
            so_dong: 0,
            loi: vec![format!("Không ghi được dữ liệu: {e}")],
        };
    }
    kq
}

/// A conversation as the GUI knows it.
pub struct HoiThoai<'a> {
    pub id: &'a str,
    pub ten: &'a str,
    pub la_nhom: bool,
}

/// Add Khách hàng from 1-1 conversations (they already carry a Zalo ID).
/// The Zalo display name goes to `ten_zalo` only. Returns (new, already present).
pub fn them_tu_hoi_thoai(conn: &Connection, hoi_thoai: &[HoiThoai]) -> Result<(usize, usize)> {
    let (mut moi, mut da_co) = (0, 0);
    let tx = conn.unchecked_transaction()?;
    for ht in hoi_thoai.iter().filter(|h| !h.la_nhom && !h.id.is_empty()) {
        let ten_zalo = Some(ht.ten.trim().nfc().collect::<String>()).filter(|s| !s.is_empty());
        let co: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM khach_hang WHERE zalo_id = ?)",
            [ht.id],
            |r| r.get(0),
        )?;
        if co {
            da_co += 1
        } else {
            moi += 1
        }
        tx.execute(
            "INSERT INTO khach_hang (zalo_id, ten_zalo) VALUES (?1, ?2)
             ON CONFLICT(zalo_id) DO UPDATE SET ten_zalo = COALESCE(excluded.ten_zalo, ten_zalo)",
            params![ht.id, ten_zalo],
        )?;
    }
    tx.commit()?;
    Ok((moi, da_co))
}

pub fn danh_sach(conn: &Connection) -> Result<Vec<KhachHang>> {
    let mut st = conn.prepare(&format!("SELECT {COT_SQL} FROM khach_hang ORDER BY id"))?;
    let rows = st.query_map([], tu_dong)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Save the three seller-editable fields of one Khách hàng. Blank name/date → cleared,
/// blank xưng hô → default. An unreadable date is refused.
pub fn cap_nhat(
    conn: &Connection,
    id: i64,
    ten: &str,
    xung_ho: &str,
    ngay_mua: &str,
) -> Result<()> {
    let chuan = |s: &str| O::Chu(s.to_string()).chu();
    let ngay_iso = match ngay(&O::Chu(ngay_mua.to_string())) {
        Ok(n) => n,
        Err(e) => bail!("Không lưu được: {e}. Nhập dạng ngày/tháng/năm, ví dụ 25/12/2025."),
    };
    conn.execute(
        "UPDATE khach_hang SET ten = ?, xung_ho = ?, ngay_mua_gan_nhat = ? WHERE id = ?",
        params![
            chuan(ten),
            chuan(xung_ho).unwrap_or_else(|| XUNG_HO_MAC_DINH.into()),
            ngay_iso,
            id
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cskh::db;

    fn chu(s: &str) -> O {
        O::Chu(s.into())
    }
    fn tieu_de() -> Vec<O> {
        COT_EXCEL.iter().map(|c| chu(c)).collect()
    }

    #[test]
    fn phone_numbers_normalise_to_ten_digits() {
        for (vao, ra) in [
            ("0912 345 678", Some("0912345678")),
            ("+84 912 345 678", Some("0912345678")),
            ("912345678", Some("0912345678")),
            ("84912345678", Some("0912345678")),
            ("12345", None),
            ("", None),
            ("1912345678", None),
        ] {
            assert_eq!(chuan_hoa_so_dien_thoai(vao).as_deref(), ra, "{vao}");
        }
    }

    #[test]
    fn reimport_updates_by_phone_keeps_blanks_and_only_moves_date_forward() {
        let conn = db::mo_bo_nho().unwrap();
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        let kq = nhap_cac_dong(
            &conn,
            &[
                tieu_de(),
                vec![
                    O::So(912345678.0),
                    chu("Lan"),
                    chu("chị"),
                    O::Ngay(d("2025-05-01")),
                ],
                vec![chu("0987654321"), O::Trong, O::Trong, chu("02/03/2025")],
            ],
        );
        assert_eq!((kq.so_dong, kq.loi.len()), (2, 0));
        // Same phone in another format: blanks keep old values, an older date is ignored.
        let kq = nhap_cac_dong(
            &conn,
            &[
                tieu_de(),
                vec![
                    chu("+84912345678"),
                    O::Trong,
                    O::Trong,
                    O::Ngay(d("2025-01-01")),
                ],
            ],
        );
        assert_eq!(kq.so_dong, 1);
        let ds = danh_sach(&conn).unwrap();
        assert_eq!(ds.len(), 2, "no duplicate");
        assert_eq!(
            (
                ds[0].ten.as_deref(),
                ds[0].xung_ho.as_str(),
                ds[0].ngay_mua_gan_nhat.as_deref()
            ),
            (Some("Lan"), "chị", Some("2025-05-01"))
        );
        assert_eq!(
            (ds[1].xung_ho.as_str(), ds[1].ngay_mua_gan_nhat.as_deref()),
            ("anh/chị", Some("2025-03-02"))
        );
        // A newer date moves forward and a new name replaces the old one.
        nhap_cac_dong(
            &conn,
            &[
                tieu_de(),
                vec![
                    chu("0912345678"),
                    chu("Lan Anh"),
                    O::Trong,
                    O::Ngay(d("2025-06-01")),
                ],
            ],
        );
        let lan = &danh_sach(&conn).unwrap()[0];
        assert_eq!(
            (lan.ten.as_deref(), lan.ngay_mua_gan_nhat.as_deref()),
            (Some("Lan Anh"), Some("2025-06-01"))
        );
    }

    #[test]
    fn bad_rows_are_reported_with_their_line_and_good_rows_still_import() {
        let conn = db::mo_bo_nho().unwrap();
        let kq = nhap_cac_dong(
            &conn,
            &[
                tieu_de(),
                vec![chu("123"), chu("A"), O::Trong, O::Trong],
                vec![
                    chu("0912345678"),
                    chu("B"),
                    O::Trong,
                    chu("không phải ngày"),
                ],
                vec![O::Trong, O::Trong, O::Trong, O::Trong],
                vec![chu("0912345678"), chu("B"), O::Trong, O::So(45000.0)],
                vec![chu("0912345678"), chu("C"), O::Trong, O::Trong],
            ],
        );
        assert_eq!(kq.so_dong, 1);
        assert_eq!(kq.loi.len(), 3);
        assert!(
            kq.loi[0].starts_with("Dòng 2:")
                && kq.loi[1].starts_with("Dòng 3:")
                && kq.loi[2].starts_with("Dòng 5:")
        );
        let missing = nhap_cac_dong(&conn, &[vec![chu("Tên")]]);
        assert_eq!(
            missing.loi,
            vec!["Thiếu cột: Số điện thoại, Xưng hô, Ngày mua gần nhất"]
        );
    }

    #[test]
    fn nfd_names_are_stored_as_nfc() {
        let conn = db::mo_bo_nho().unwrap();
        let nfd: String = "Nguyễn".nfd().collect();
        nhap_cac_dong(
            &conn,
            &[
                tieu_de(),
                vec![chu("0912345678"), chu(&nfd), O::Trong, O::Trong],
            ],
        );
        assert_eq!(danh_sach(&conn).unwrap()[0].ten.as_deref(), Some("Nguyễn"));
    }

    #[test]
    fn xlsx_file_is_read_including_numeric_phone_and_date_cells() {
        let conn = db::mo_bo_nho().unwrap();
        let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/khach_hang.xlsx");
        let kq = nhap_excel(&conn, &file);
        assert_eq!(kq.loi, Vec::<String>::new());
        let ds = danh_sach(&conn).unwrap();
        assert_eq!(ds.len(), 2);
        assert_eq!(ds[0].so_dien_thoai.as_deref(), Some("0912345678"));
        assert_eq!(ds[0].ngay_mua_gan_nhat.as_deref(), Some("2025-12-25"));
        assert_eq!(ds[0].ten.as_deref(), Some("Nguyễn Lan"));
        assert_eq!(ds[1].xung_ho, "anh/chị");
        let hong = nhap_excel(&conn, &file.with_file_name("khong_co.xlsx"));
        assert!(hong.loi[0].starts_with("Không đọc được file Excel"));
    }

    #[test]
    fn zalo_display_name_never_becomes_the_real_name() {
        let conn = db::mo_bo_nho().unwrap();
        let ht = [
            HoiThoai {
                id: "u1",
                ten: "  Lan Zalo ",
                la_nhom: false,
            },
            HoiThoai {
                id: "g1",
                ten: "Nhóm",
                la_nhom: true,
            },
        ];
        assert_eq!(them_tu_hoi_thoai(&conn, &ht).unwrap(), (1, 0));
        assert_eq!(them_tu_hoi_thoai(&conn, &ht).unwrap(), (0, 1));
        let k = &danh_sach(&conn).unwrap()[0];
        assert_eq!(
            (k.ten.as_deref(), k.ten_zalo.as_deref()),
            (None, Some("Lan Zalo"))
        );
        assert_eq!(k.ten_hien(), "Lan Zalo");
    }

    #[test]
    fn editing_saves_the_three_fields_and_refuses_bad_dates() {
        let conn = db::mo_bo_nho().unwrap();
        them_tu_hoi_thoai(
            &conn,
            &[HoiThoai {
                id: "u1",
                ten: "Z",
                la_nhom: false,
            }],
        )
        .unwrap();
        let id = danh_sach(&conn).unwrap()[0].id;
        cap_nhat(&conn, id, " Lan ", "chị", "25/12/2025").unwrap();
        let k = danh_sach(&conn).unwrap().remove(0);
        assert_eq!(
            (
                k.ten.as_deref(),
                k.xung_ho.as_str(),
                k.ngay_mua_gan_nhat.as_deref()
            ),
            (Some("Lan"), "chị", Some("2025-12-25"))
        );
        assert!(cap_nhat(&conn, id, "Lan", "chị", "hôm qua").is_err());
        cap_nhat(&conn, id, "", "", "").unwrap();
        let k = danh_sach(&conn).unwrap().remove(0);
        assert_eq!(
            (k.ten, k.xung_ho, k.ngay_mua_gan_nhat),
            (None, "anh/chị".to_string(), None)
        );
    }
}
