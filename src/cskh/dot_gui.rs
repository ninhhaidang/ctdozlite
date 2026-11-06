//! Đợt gửi: pick Khách hàng, fill Mẫu tin, review, then send at a safe pace (ADR-0001, ADR-0003).
//!
//! Every rule protecting the Zalo account lives in `gui_tiep`: sending hours, spacing, daily cap,
//! stop on the first error, never send twice. Zalo and the clock are behind traits so tests run
//! without a real account or real time.

use super::khach_hang::{tu_dong, KhachHang, COT_SQL};
use super::mau_tin;
use anyhow::Result;
use chrono::{NaiveDate, NaiveDateTime, Timelike};
use rand::seq::SliceRandom;
use rand::Rng;
use rusqlite::{named_params, params, Connection, OptionalExtension};

/// Sending window [8h, 20h), local time. Not configurable.
pub const GIO_BAT_DAU: u32 = 8;
pub const GIO_KET_THUC: u32 = 20;
/// Random gap between two messages, in seconds (3–10 minutes).
pub const GIAN_CACH_GIAY: (f64, f64) = (180.0, 600.0);
pub const TOI_DA_NGAY_MAC_DINH: i64 = 100;
pub const NGAY_KHONG_HOI_LAI: i64 = 30;
/// How long the worker waits before re-checking after "outside hours" / "cap reached".
pub const CHO_KIEM_TRA_LAI_GIAY: f64 = 60.0;

pub const KHOA_TOI_DA_NGAY: &str = "toi_da_ngay";

// Đợt gửi states.
pub const DOT_NHAP: &str = "nhap";
pub const DOT_DANG_GUI: &str = "dang_gui";
pub const DOT_DUNG_VI_LOI: &str = "dung_vi_loi";
pub const DOT_XONG: &str = "xong";
// Message states. TIN_DANG_GUI = slot reserved, outcome unknown: it is NEVER sent again.
pub const TIN_CHO: &str = "cho";
pub const TIN_DANG_GUI: &str = "dang_gui";
pub const TIN_DA_GUI: &str = "da_gui";
pub const TIN_LOI: &str = "loi";
pub const TIN_XU_LY_TAY: &str = "xu_ly_tay";

/// Sends one text message to a Zalo user. Any `Err` means "stop everything".
pub trait GuiTin {
    fn gui_tin(&self, zalo_id: &str, noi_dung: &str) -> Result<(), String>;
}

pub trait DongHo {
    fn now(&self) -> NaiveDateTime;
    fn ngu(&self, giay: f64);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LyDoDung {
    Xong,
    Loi,
    /// Not an error: calling again later continues.
    NgoaiGio,
    /// Not an error: calling again after midnight continues.
    HetGioiHan,
    /// The Đợt gửi was not started, or stopped because of an error.
    KhongChay,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Dot {
    pub id: i64,
    pub nhom_mau_id: Option<i64>,
    pub tao_luc: String,
    pub trang_thai: String,
    pub loi: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tin {
    pub id: i64,
    pub khach_hang_id: i64,
    pub ten_khach: String,
    pub noi_dung: String,
    pub chon: bool,
    pub trang_thai: String,
    pub gui_luc: Option<String>,
    pub loi: Option<String>,
}

fn iso(t: NaiveDateTime) -> String {
    t.format("%Y-%m-%dT%H:%M:%S").to_string()
}

pub fn toi_da_ngay(conn: &Connection) -> Result<i64> {
    Ok(super::db::doc_cai_dat(conn, KHOA_TOI_DA_NGAY)?
        .and_then(|s| s.parse().ok())
        .unwrap_or(TOI_DA_NGAY_MAC_DINH))
}

pub fn dat_toi_da_ngay(conn: &Connection, n: i64) -> Result<()> {
    super::db::luu_cai_dat(conn, KHOA_TOI_DA_NGAY, &n.max(0).to_string())
}

/// Khách hàng linked to Zalo with no Lần hỏi thăm in the last 30 days, filtered by purchase date.
/// A message whose outcome is unknown also counts as a Lần hỏi thăm: it may have gone out.
pub fn ung_vien(
    conn: &Connection,
    now: NaiveDateTime,
    tu_ngay: Option<NaiveDate>,
    den_ngay: Option<NaiveDate>,
) -> Result<Vec<KhachHang>> {
    let moc = iso(now - chrono::Duration::days(NGAY_KHONG_HOI_LAI));
    let ngay = |d: Option<NaiveDate>| d.map(|d| d.format("%Y-%m-%d").to_string());
    let mut st = conn.prepare(&format!(
        "SELECT {COT_SQL} FROM khach_hang k WHERE zalo_id IS NOT NULL
           AND (:tu IS NULL OR ngay_mua_gan_nhat >= :tu) AND (:den IS NULL OR ngay_mua_gan_nhat <= :den)
           AND NOT EXISTS (SELECT 1 FROM tin_gui t WHERE t.khach_hang_id = k.id AND t.gui_luc >= :moc)
         ORDER BY ngay_mua_gan_nhat DESC, id"
    ))?;
    let rows = st.query_map(
        named_params! {":tu": ngay(tu_ngay), ":den": ngay(den_ngay), ":moc": moc},
        tu_dong,
    )?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Create a draft Đợt gửi. Khách hàng outside the candidate list are dropped; a Khách hàng
/// with no fitting Mẫu tin becomes `xu_ly_tay` (handle by hand).
pub fn tao<R: Rng + ?Sized>(
    conn: &Connection,
    nhom_id: i64,
    khach_ids: &[i64],
    now: NaiveDateTime,
    rng: &mut R,
) -> Result<i64> {
    let mau = mau_tin::danh_sach_mau(conn, nhom_id)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO dot_gui (nhom_mau_id, tao_luc) VALUES (?, ?)",
        params![nhom_id, iso(now)],
    )?;
    let dot = tx.last_insert_rowid();
    for k in ung_vien(conn, now, None, None)?
        .iter()
        .filter(|k| khach_ids.contains(&k.id))
    {
        let hop: Vec<_> = mau
            .iter()
            .filter(|m| mau_tin::dung_duoc_cho(&m.noi_dung, k.ten.as_deref()))
            .collect();
        let noi_dung = match hop.choose(rng) {
            Some(m) => mau_tin::dien_mau(&m.noi_dung, &k.xung_ho, k.ten.as_deref())?,
            None => String::new(),
        };
        let trang_thai = if noi_dung.is_empty() {
            TIN_XU_LY_TAY
        } else {
            TIN_CHO
        };
        tx.execute(
            "INSERT INTO tin_gui (dot_gui_id, khach_hang_id, zalo_id, noi_dung, chon, trang_thai)
             VALUES (?, ?, ?, ?, ?, ?)",
            params![
                dot,
                k.id,
                k.zalo_id,
                noi_dung,
                !noi_dung.is_empty(),
                trang_thai
            ],
        )?;
    }
    tx.commit()?;
    Ok(dot)
}

fn dot_tu_dong(r: &rusqlite::Row) -> rusqlite::Result<Dot> {
    Ok(Dot {
        id: r.get(0)?,
        nhom_mau_id: r.get(1)?,
        tao_luc: r.get(2)?,
        trang_thai: r.get(3)?,
        loi: r.get(4)?,
    })
}

const COT_DOT: &str = "id, nhom_mau_id, tao_luc, trang_thai, loi";

pub fn dot(conn: &Connection, id: i64) -> Result<Dot> {
    Ok(conn.query_row(
        &format!("SELECT {COT_DOT} FROM dot_gui WHERE id = ?"),
        [id],
        dot_tu_dong,
    )?)
}

pub fn danh_sach_dot(conn: &Connection) -> Result<Vec<Dot>> {
    let mut st = conn.prepare(&format!("SELECT {COT_DOT} FROM dot_gui ORDER BY id DESC"))?;
    let rows = st.query_map([], dot_tu_dong)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn co_dot_dang_gui(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM dot_gui WHERE trang_thai = ?)",
        [DOT_DANG_GUI],
        |r| r.get(0),
    )?)
}

pub fn tin_cua_dot(conn: &Connection, dot_id: i64) -> Result<Vec<Tin>> {
    let mut st = conn.prepare(
        "SELECT t.id, t.khach_hang_id, COALESCE(k.ten, k.ten_zalo, k.so_dien_thoai, ''), t.noi_dung,
                t.chon, t.trang_thai, t.gui_luc, t.loi
         FROM tin_gui t JOIN khach_hang k ON k.id = t.khach_hang_id
         WHERE t.dot_gui_id = ? ORDER BY t.id",
    )?;
    let rows = st.query_map([dot_id], |r| {
        Ok(Tin {
            id: r.get(0)?,
            khach_hang_id: r.get(1)?,
            ten_khach: r.get(2)?,
            noi_dung: r.get(3)?,
            chon: r.get(4)?,
            trang_thai: r.get(5)?,
            gui_luc: r.get(6)?,
            loi: r.get(7)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Only a message that is still waiting can be ticked or unticked; `xu_ly_tay` never.
pub fn dat_chon(conn: &Connection, tin_id: i64, chon: bool) -> Result<()> {
    conn.execute(
        "UPDATE tin_gui SET chon = ? WHERE id = ? AND trang_thai = ?",
        params![chon, tin_id, TIN_CHO],
    )?;
    Ok(())
}

/// The seller pressed "Gửi đợt này" (or resumed after an error). The sending itself is `gui_tiep`.
pub fn bat_dau(conn: &Connection, dot_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE dot_gui SET trang_thai = ?, loi = NULL WHERE id = ? AND trang_thai IN (?, ?)",
        params![DOT_DANG_GUI, dot_id, DOT_NHAP, DOT_DUNG_VI_LOI],
    )?;
    Ok(())
}

/// The seller pressed "Dừng gửi": back to `nhap`, so ticks are editable again and "Gửi đợt này"
/// resumes it. `gui_tiep` re-checks the state before every message, so nothing more is sent
/// (a message already handed to Zalo cannot be recalled). Not resumed on app start: only `dang_gui` is.
pub fn dung_gui(conn: &Connection, dot_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE dot_gui SET trang_thai = ? WHERE id = ? AND trang_thai = ?",
        params![DOT_NHAP, dot_id, DOT_DANG_GUI],
    )?;
    Ok(())
}

/// Send the remaining messages of one Đợt gửi. Must only run on the single sender thread
/// (`bo_gui`), so the cap, spacing and slot reservation are never checked concurrently.
pub fn gui_tiep<R: Rng + ?Sized>(
    conn: &Connection,
    dot_id: i64,
    zalo: &dyn GuiTin,
    dh: &dyn DongHo,
    rng: &mut R,
) -> Result<LyDoDung> {
    let mut gian_cach: Option<f64> = None;
    loop {
        if dot(conn, dot_id)?.trang_thai != DOT_DANG_GUI {
            return Ok(LyDoDung::KhongChay);
        }
        let t = dh.now();
        if !(GIO_BAT_DAU..GIO_KET_THUC).contains(&t.hour()) {
            return Ok(LyDoDung::NgoaiGio);
        }
        let dau_ngay = iso(t.date().and_hms_opt(0, 0, 0).expect("midnight exists"));
        // Counts every reserved slot (ambiguous ones too) in every Đợt: better to send too few than too many.
        let hom_nay: i64 = conn.query_row(
            "SELECT COUNT(*) FROM tin_gui WHERE gui_luc >= ?",
            [dau_ngay],
            |r| r.get(0),
        )?;
        if hom_nay >= toi_da_ngay(conn)? {
            return Ok(LyDoDung::HetGioiHan);
        }
        let tin: Option<(i64, i64, String, String)> = conn
            .query_row(
                "SELECT id, khach_hang_id, zalo_id, noi_dung FROM tin_gui
                 WHERE dot_gui_id = ? AND trang_thai = ? AND chon = 1 ORDER BY id LIMIT 1",
                params![dot_id, TIN_CHO],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((tin_id, khach_id, zalo_id, noi_dung)) = tin else {
            conn.execute(
                "UPDATE dot_gui SET trang_thai = ? WHERE id = ?",
                params![DOT_XONG, dot_id],
            )?;
            return Ok(LyDoDung::Xong);
        };
        let lan_truoc: Option<String> =
            conn.query_row("SELECT MAX(gui_luc) FROM tin_gui", [], |r| r.get(0))?;
        if let Some(lan_truoc) =
            lan_truoc.and_then(|s| NaiveDateTime::parse_from_str(&s, "%Y-%m-%dT%H:%M:%S").ok())
        {
            let gian = *gian_cach
                .get_or_insert_with(|| rng.gen_range(GIAN_CACH_GIAY.0..=GIAN_CACH_GIAY.1));
            let con_lai = (lan_truoc - t).num_milliseconds() as f64 / 1000.0 + gian;
            if con_lai > 0.0 {
                dh.ngu(con_lai);
                continue; // re-check the hours after waiting
            }
        }
        gian_cach = None;
        let moc = iso(t - chrono::Duration::days(NGAY_KHONG_HOI_LAI));
        let tx = conn.unchecked_transaction()?;
        let da_hoi: bool = tx.query_row(
            // Asked in another Đợt after this one was created.
            "SELECT EXISTS (SELECT 1 FROM tin_gui WHERE khach_hang_id = ? AND id != ? AND gui_luc >= ?)",
            params![khach_id, tin_id, moc],
            |r| r.get(0),
        )?;
        if da_hoi {
            tx.execute(
                "UPDATE tin_gui SET trang_thai = ?, chon = 0, loi = ? WHERE id = ?",
                params![
                    TIN_XU_LY_TAY,
                    "Đã được hỏi thăm trong 30 ngày gần đây",
                    tin_id
                ],
            )?;
            tx.commit()?;
            continue;
        }
        // Reserve the slot BEFORE calling Zalo: if the app dies mid-send the message stays
        // `dang_gui` and is never sent again.
        tx.execute(
            "UPDATE tin_gui SET trang_thai = ?, gui_luc = ? WHERE id = ? AND trang_thai = ?",
            params![TIN_DANG_GUI, iso(t), tin_id, TIN_CHO],
        )?;
        tx.commit()?;
        if let Err(loi) = zalo.gui_tin(&zalo_id, &noi_dung) {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "UPDATE tin_gui SET trang_thai = ?, loi = ? WHERE id = ?",
                params![TIN_LOI, loi, tin_id],
            )?;
            // The first error stops every running Đợt: an odd error may mean the account is about to be limited.
            tx.execute(
                "UPDATE dot_gui SET trang_thai = ?, loi = ? WHERE trang_thai = ?",
                params![
                    DOT_DUNG_VI_LOI,
                    format!("Lỗi khi gửi tin, đã dừng mọi Đợt gửi: {loi}"),
                    DOT_DANG_GUI
                ],
            )?;
            tx.commit()?;
            return Ok(LyDoDung::Loi);
        }
        conn.execute(
            "UPDATE tin_gui SET trang_thai = ? WHERE id = ?",
            params![TIN_DA_GUI, tin_id],
        )?;
    }
}

/// Stop every running Đợt with a message the seller can read (worker crashed, DB error…).
pub fn dung_het_vi_loi(conn: &Connection, loi: &str) -> Result<()> {
    conn.execute(
        "UPDATE dot_gui SET trang_thai = ?, loi = ? WHERE trang_thai = ?",
        params![DOT_DUNG_VI_LOI, loi, DOT_DANG_GUI],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cskh::db;
    use rand::rngs::StdRng;
    use rand::SeedableRng;
    use std::cell::{Cell, RefCell};

    fn gio(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    struct DongHoGia(Cell<NaiveDateTime>, RefCell<Vec<f64>>);
    impl DongHoGia {
        fn luc(s: &str) -> Self {
            Self(Cell::new(gio(s)), RefCell::default())
        }
    }
    impl DongHo for DongHoGia {
        fn now(&self) -> NaiveDateTime {
            self.0.get()
        }
        fn ngu(&self, giay: f64) {
            self.0
                .set(self.0.get() + chrono::Duration::milliseconds((giay * 1000.0).ceil() as i64));
            self.1.borrow_mut().push(giay);
        }
    }

    /// Records sends; fails on the n-th call (1-based) when `loi_o` is set.
    struct ZaloGia<'a> {
        dh: &'a DongHoGia,
        conn: &'a Connection,
        da_gui: RefCell<Vec<(String, NaiveDateTime)>>,
        loi_o: Option<usize>,
    }
    impl GuiTin for ZaloGia<'_> {
        fn gui_tin(&self, zalo_id: &str, _: &str) -> Result<(), String> {
            // The slot must already be reserved in the DB at the moment Zalo is called.
            let (tt, luc): (String, Option<String>) = self
                .conn
                .query_row(
                    "SELECT trang_thai, gui_luc FROM tin_gui WHERE zalo_id = ? AND trang_thai = 'dang_gui'",
                    [zalo_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .expect("slot reserved before calling Zalo");
            assert_eq!((tt.as_str(), luc.is_some()), ("dang_gui", true));
            self.da_gui
                .borrow_mut()
                .push((zalo_id.to_string(), self.dh.now()));
            if self.loi_o == Some(self.da_gui.borrow().len()) {
                return Err("Zalo từ chối".into());
            }
            Ok(())
        }
    }

    fn khach(conn: &Connection, zalo: Option<&str>, ten: Option<&str>, ngay: &str) -> i64 {
        conn.execute(
            "INSERT INTO khach_hang (zalo_id, ten, ngay_mua_gan_nhat) VALUES (?, ?, ?)",
            params![zalo, ten, ngay],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn mau(conn: &Connection, noi_dung: &[&str]) -> i64 {
        mau_tin::them_nhom(conn, "Hỏi thăm").unwrap();
        let nhom = mau_tin::danh_sach_nhom(conn).unwrap()[0].id;
        for n in noi_dung {
            mau_tin::them_mau(conn, nhom, n).unwrap();
        }
        nhom
    }

    /// n linked customers, a draft Đợt for all of them, started.
    fn dot_san(conn: &Connection, n: usize, now: NaiveDateTime) -> i64 {
        let nhom = mau(conn, &["Chào {xưng_hô}"]);
        let ids: Vec<i64> = (0..n)
            .map(|i| khach(conn, Some(&format!("z{i}")), None, "2025-01-01"))
            .collect();
        let dot = tao(conn, nhom, &ids, now, &mut StdRng::seed_from_u64(1)).unwrap();
        bat_dau(conn, dot).unwrap();
        dot
    }

    fn chay(conn: &Connection, dot: i64, dh: &DongHoGia, zalo: &ZaloGia) -> LyDoDung {
        gui_tiep(conn, dot, zalo, dh, &mut StdRng::seed_from_u64(7)).unwrap()
    }

    fn zalo<'a>(dh: &'a DongHoGia, conn: &'a Connection, loi_o: Option<usize>) -> ZaloGia<'a> {
        ZaloGia {
            dh,
            conn,
            da_gui: RefCell::default(),
            loi_o,
        }
    }

    #[test]
    fn candidates_exclude_unlinked_and_recently_asked_customers() {
        let conn = db::mo_bo_nho().unwrap();
        let now = gio("2026-03-01 10:00:00");
        let chua_lien_ket = khach(&conn, None, Some("A"), "2026-01-01");
        let moi_hoi = khach(&conn, Some("z1"), Some("B"), "2026-01-01");
        let hoi_cu = khach(&conn, Some("z2"), Some("C"), "2026-01-02");
        let ngoai_loc = khach(&conn, Some("z3"), Some("D"), "2025-06-01");
        let dot = conn
            .execute("INSERT INTO dot_gui (tao_luc) VALUES ('x')", [])
            .unwrap();
        let _ = dot;
        for (k, luc) in [
            (moi_hoi, "2026-02-10T09:00:00"),
            (hoi_cu, "2026-01-30T09:00:00"),
        ] {
            conn.execute(
                "INSERT INTO tin_gui (dot_gui_id, khach_hang_id, zalo_id, noi_dung, trang_thai, gui_luc)
                 VALUES (1, ?, 'z', 'x', 'dang_gui', ?)",
                params![k, luc],
            )
            .unwrap();
        }
        let ids: Vec<i64> = ung_vien(&conn, now, None, None)
            .unwrap()
            .iter()
            .map(|k| k.id)
            .collect();
        assert_eq!(ids, vec![hoi_cu, ngoai_loc]);
        assert!(!ids.contains(&chua_lien_ket) && !ids.contains(&moi_hoi));
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok();
        let loc = ung_vien(&conn, now, d("2026-01-01"), d("2026-12-31")).unwrap();
        assert_eq!(loc.iter().map(|k| k.id).collect::<Vec<_>>(), vec![hoi_cu]);
    }

    #[test]
    fn creating_a_dot_fills_templates_and_flags_customers_without_a_fitting_one() {
        let conn = db::mo_bo_nho().unwrap();
        let now = gio("2026-03-01 10:00:00");
        let nhom = mau(&conn, &["Chào {xưng_hô} {tên}"]);
        let co_ten = khach(&conn, Some("z1"), Some("Lan"), "2026-01-01");
        let khong_ten = khach(&conn, Some("z2"), None, "2026-01-01");
        let chua_lk = khach(&conn, None, Some("X"), "2026-01-01");
        let dot = tao(
            &conn,
            nhom,
            &[co_ten, khong_ten, chua_lk],
            now,
            &mut StdRng::seed_from_u64(1),
        )
        .unwrap();
        let tin = tin_cua_dot(&conn, dot).unwrap();
        assert_eq!(tin.len(), 2, "the unlinked customer never enters a Đợt");
        assert_eq!(
            (
                tin[0].noi_dung.as_str(),
                tin[0].chon,
                tin[0].trang_thai.as_str()
            ),
            ("Chào anh/chị Lan", true, TIN_CHO)
        );
        assert_eq!(
            (
                tin[1].noi_dung.as_str(),
                tin[1].chon,
                tin[1].trang_thai.as_str()
            ),
            ("", false, TIN_XU_LY_TAY)
        );
        // "xử lý tay" can never be ticked.
        dat_chon(&conn, tin[1].id, true).unwrap();
        assert!(!tin_cua_dot(&conn, dot).unwrap()[1].chon);
    }

    #[test]
    fn each_customer_gets_a_random_template_of_the_group() {
        let conn = db::mo_bo_nho().unwrap();
        let nhom = mau(&conn, &["A", "B"]);
        let ids: Vec<i64> = (0..40)
            .map(|i| khach(&conn, Some(&format!("z{i}")), None, "2026-01-01"))
            .collect();
        let dot = tao(
            &conn,
            nhom,
            &ids,
            gio("2026-03-01 10:00:00"),
            &mut StdRng::seed_from_u64(3),
        )
        .unwrap();
        let tin = tin_cua_dot(&conn, dot).unwrap();
        assert!(tin.iter().any(|t| t.noi_dung == "A") && tin.iter().any(|t| t.noi_dung == "B"));
    }

    #[test]
    fn nothing_is_sent_before_start_and_only_ticked_messages_are_sent() {
        let conn = db::mo_bo_nho().unwrap();
        let dh = DongHoGia::luc("2026-03-01 09:00:00");
        let nhom = mau(&conn, &["Chào"]);
        let ids: Vec<i64> = (0..3)
            .map(|i| khach(&conn, Some(&format!("z{i}")), None, "2026-01-01"))
            .collect();
        let dot = tao(&conn, nhom, &ids, dh.now(), &mut StdRng::seed_from_u64(1)).unwrap();
        let z = zalo(&dh, &conn, None);
        assert_eq!(chay(&conn, dot, &dh, &z), LyDoDung::KhongChay);
        assert!(z.da_gui.borrow().is_empty());
        dat_chon(&conn, tin_cua_dot(&conn, dot).unwrap()[1].id, false).unwrap();
        bat_dau(&conn, dot).unwrap();
        assert_eq!(chay(&conn, dot, &dh, &z), LyDoDung::Xong);
        let gui: Vec<_> = z.da_gui.borrow().iter().map(|(z, _)| z.clone()).collect();
        assert_eq!(gui, vec!["z0", "z2"]);
        let tin = tin_cua_dot(&conn, dot).unwrap();
        assert_eq!(tin[1].trang_thai, TIN_CHO, "unticked message stays unsent");
        assert_eq!(super::dot(&conn, dot).unwrap().trang_thai, DOT_XONG);
    }

    #[test]
    fn stopping_sends_nothing_more_and_can_resume() {
        let conn = db::mo_bo_nho().unwrap();
        let dh = DongHoGia::luc("2026-03-01 09:00:00");
        let dot = dot_san(&conn, 3, dh.now());
        let z = zalo(&dh, &conn, None);
        dung_gui(&conn, dot).unwrap();
        assert_eq!(super::dot(&conn, dot).unwrap().trang_thai, DOT_NHAP);
        assert_eq!(chay(&conn, dot, &dh, &z), LyDoDung::KhongChay);
        assert!(z.da_gui.borrow().is_empty());
        bat_dau(&conn, dot).unwrap();
        assert_eq!(chay(&conn, dot, &dh, &z), LyDoDung::Xong);
        assert_eq!(z.da_gui.borrow().len(), 3);
    }

    #[test]
    fn messages_are_spaced_3_to_10_minutes_apart_and_never_outside_hours() {
        let conn = db::mo_bo_nho().unwrap();
        let dh = DongHoGia::luc("2026-03-01 09:00:00");
        let dot = dot_san(&conn, 6, dh.now());
        let z = zalo(&dh, &conn, None);
        assert_eq!(chay(&conn, dot, &dh, &z), LyDoDung::Xong);
        let gui = z.da_gui.borrow();
        assert_eq!(gui.len(), 6);
        for w in gui.windows(2) {
            let gap = (w[1].1 - w[0].1).num_seconds();
            assert!((180..=601).contains(&gap), "gap {gap}s");
        }
        let gaps: std::collections::HashSet<_> = gui
            .windows(2)
            .map(|w| (w[1].1 - w[0].1).num_seconds())
            .collect();
        assert!(gaps.len() > 1, "gaps are random, not constant");
        assert!(gui
            .iter()
            .all(|(_, t)| (GIO_BAT_DAU..GIO_KET_THUC).contains(&t.hour())));
    }

    #[test]
    fn outside_sending_hours_nothing_is_sent() {
        for luc in [
            "2026-03-01 07:59:59",
            "2026-03-01 20:00:00",
            "2026-03-01 23:30:00",
            "2026-03-01 03:00:00",
        ] {
            let conn = db::mo_bo_nho().unwrap();
            let dh = DongHoGia::luc(luc);
            let dot = dot_san(&conn, 2, dh.now());
            let z = zalo(&dh, &conn, None);
            assert_eq!(chay(&conn, dot, &dh, &z), LyDoDung::NgoaiGio, "{luc}");
            assert!(z.da_gui.borrow().is_empty());
        }
    }

    #[test]
    fn waiting_past_the_end_of_the_window_stops_instead_of_sending() {
        let conn = db::mo_bo_nho().unwrap();
        let dh = DongHoGia::luc("2026-03-01 19:59:00");
        let dot = dot_san(&conn, 3, dh.now());
        let z = zalo(&dh, &conn, None);
        assert_eq!(chay(&conn, dot, &dh, &z), LyDoDung::NgoaiGio);
        assert_eq!(z.da_gui.borrow().len(), 1);
        assert_eq!(
            super::dot(&conn, dot).unwrap().trang_thai,
            DOT_DANG_GUI,
            "continues the next morning"
        );
        // Next morning, spacing from last night's message is long satisfied: it continues.
        dh.0.set(gio("2026-03-02 08:00:00"));
        assert_eq!(chay(&conn, dot, &dh, &z), LyDoDung::Xong);
        assert_eq!(z.da_gui.borrow().len(), 3);
    }

    #[test]
    fn daily_cap_counts_every_dot_and_ambiguous_messages() {
        let conn = db::mo_bo_nho().unwrap();
        let dh = DongHoGia::luc("2026-03-01 09:00:00");
        dat_toi_da_ngay(&conn, 3).unwrap();
        let dot1 = dot_san(&conn, 2, dh.now());
        let z = zalo(&dh, &conn, None);
        assert_eq!(chay(&conn, dot1, &dh, &z), LyDoDung::Xong);
        // Second Đợt: 1 more fits under the cap of 3, the rest waits.
        let nhom = mau_tin::danh_sach_nhom(&conn).unwrap()[0].id;
        let ids: Vec<i64> = (10..13)
            .map(|i| khach(&conn, Some(&format!("y{i}")), None, "2025-01-01"))
            .collect();
        let dot2 = tao(&conn, nhom, &ids, dh.now(), &mut StdRng::seed_from_u64(1)).unwrap();
        bat_dau(&conn, dot2).unwrap();
        assert_eq!(chay(&conn, dot2, &dh, &z), LyDoDung::HetGioiHan);
        assert_eq!(z.da_gui.borrow().len(), 3);
        // Tomorrow the cap resets.
        dh.0.set(gio("2026-03-02 09:00:00"));
        assert_eq!(chay(&conn, dot2, &dh, &z), LyDoDung::Xong);
        assert_eq!(z.da_gui.borrow().len(), 5);
    }

    #[test]
    fn first_error_stops_every_running_dot_and_the_failed_message_is_never_retried() {
        let conn = db::mo_bo_nho().unwrap();
        let dh = DongHoGia::luc("2026-03-01 09:00:00");
        let dot1 = dot_san(&conn, 3, dh.now());
        let nhom = mau_tin::danh_sach_nhom(&conn).unwrap()[0].id;
        let k = khach(&conn, Some("w"), None, "2025-01-01");
        let dot2 = tao(&conn, nhom, &[k], dh.now(), &mut StdRng::seed_from_u64(1)).unwrap();
        bat_dau(&conn, dot2).unwrap();
        let z = zalo(&dh, &conn, Some(2));
        assert_eq!(chay(&conn, dot1, &dh, &z), LyDoDung::Loi);
        assert_eq!(z.da_gui.borrow().len(), 2);
        let tin = tin_cua_dot(&conn, dot1).unwrap();
        assert_eq!(
            tin.iter()
                .map(|t| t.trang_thai.as_str())
                .collect::<Vec<_>>(),
            vec![TIN_DA_GUI, TIN_LOI, TIN_CHO]
        );
        assert_eq!(tin[1].loi.as_deref(), Some("Zalo từ chối"));
        assert!(super::dot(&conn, dot2)
            .unwrap()
            .loi
            .unwrap()
            .contains("Zalo từ chối"));
        assert_eq!(super::dot(&conn, dot1).unwrap().trang_thai, DOT_DUNG_VI_LOI);
        assert_eq!(
            super::dot(&conn, dot2).unwrap().trang_thai,
            DOT_DUNG_VI_LOI,
            "the other Đợt stops too"
        );
        assert_eq!(chay(&conn, dot2, &dh, &z), LyDoDung::KhongChay);
        // The seller resumes: the failed message is not sent again, the waiting one is.
        bat_dau(&conn, dot1).unwrap();
        let z2 = zalo(&dh, &conn, None);
        assert_eq!(chay(&conn, dot1, &dh, &z2), LyDoDung::Xong);
        assert_eq!(
            z2.da_gui
                .borrow()
                .iter()
                .map(|(z, _)| z.as_str())
                .collect::<Vec<_>>(),
            vec!["z2"]
        );
        assert_eq!(tin_cua_dot(&conn, dot1).unwrap()[1].trang_thai, TIN_LOI);
    }

    #[test]
    fn restart_after_a_crash_mid_send_never_resends_the_reserved_message() {
        let dir = std::env::temp_dir().join(format!("cskh-restart-{}", std::process::id()));
        let path = dir.join("1_cskh.db");
        let dh = DongHoGia::luc("2026-03-01 09:00:00");
        let (dot, bi_ket) = {
            let conn = db::mo(&path).unwrap();
            let dot = dot_san(&conn, 3, dh.now());
            // Simulate the app dying between "reserve" and "Zalo answered".
            let tin = tin_cua_dot(&conn, dot).unwrap();
            conn.execute(
                "UPDATE tin_gui SET trang_thai = 'dang_gui', gui_luc = '2026-03-01T09:00:00' WHERE id = ?",
                [tin[0].id],
            )
            .unwrap();
            (dot, tin[0].khach_hang_id)
        };
        let conn = db::mo(&path).unwrap(); // "reopen the app"
        dh.0.set(gio("2026-03-01 09:01:00"));
        assert!(co_dot_dang_gui(&conn).unwrap(), "the Đợt resumes on reopen");
        let z = zalo(&dh, &conn, None);
        assert_eq!(chay(&conn, dot, &dh, &z), LyDoDung::Xong);
        let gui: Vec<_> = z.da_gui.borrow().iter().map(|(z, _)| z.clone()).collect();
        assert_eq!(
            gui,
            vec!["z1", "z2"],
            "z0 was reserved, so it is not sent again"
        );
        // The ambiguous message still spaces the next one (09:00 → at least 3 minutes later),
        // counts toward the cap and keeps blocking the customer for 30 days.
        assert!(z.da_gui.borrow()[0].1 >= gio("2026-03-01 09:03:00"));
        assert!(!ung_vien(&conn, dh.now(), None, None)
            .unwrap()
            .iter()
            .any(|k| k.id == bi_ket));
        assert_eq!(tin_cua_dot(&conn, dot).unwrap()[0].trang_thai, TIN_DANG_GUI);
        drop(conn);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_customer_asked_in_another_dot_meanwhile_is_skipped_not_sent() {
        let conn = db::mo_bo_nho().unwrap();
        let dh = DongHoGia::luc("2026-03-01 09:00:00");
        let nhom = mau(&conn, &["Chào"]);
        let k = khach(&conn, Some("z0"), None, "2026-01-01");
        let dot_a = tao(&conn, nhom, &[k], dh.now(), &mut StdRng::seed_from_u64(1)).unwrap();
        let dot_b = tao(&conn, nhom, &[k], dh.now(), &mut StdRng::seed_from_u64(1)).unwrap();
        bat_dau(&conn, dot_a).unwrap();
        bat_dau(&conn, dot_b).unwrap();
        let z = zalo(&dh, &conn, None);
        assert_eq!(chay(&conn, dot_a, &dh, &z), LyDoDung::Xong);
        assert_eq!(chay(&conn, dot_b, &dh, &z), LyDoDung::Xong);
        assert_eq!(z.da_gui.borrow().len(), 1);
        assert_eq!(
            tin_cua_dot(&conn, dot_b).unwrap()[0].trang_thai,
            TIN_XU_LY_TAY
        );
    }
}
