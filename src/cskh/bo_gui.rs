//! The single background sender thread of the whole app.
//!
//! One thread, so the daily cap, the spacing and the slot reservation in `dot_gui::gui_tiep`
//! are never checked concurrently. It catches every error and panic, writes a Vietnamese message
//! on the Đợt gửi and stops; nothing restarts it silently (only opening the page or pressing
//! "Gửi đợt này" does).

use super::dot_gui::{self, DongHo, GuiTin, LyDoDung, CHO_KIEM_TRA_LAI_GIAY};
use crate::session::Session;
use chrono::{Local, NaiveDateTime};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

type PhienChung = Arc<tokio::sync::Mutex<Session>>;

static PHIEN: OnceLock<PhienChung> = OnceLock::new();
static DANG_CHAY: Mutex<bool> = Mutex::new(false);

/// Hand over the worker's shared session (called once from `spawn_session_worker`).
pub fn dat_phien(phien: PhienChung) {
    let _ = PHIEN.set(phien);
}

pub fn dang_chay() -> bool {
    *DANG_CHAY.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct DongHoThat;

impl DongHo for DongHoThat {
    fn now(&self) -> NaiveDateTime {
        Local::now().naive_local()
    }
    fn ngu(&self, giay: f64) {
        std::thread::sleep(Duration::from_secs_f64(giay.max(0.0)));
    }
}

/// Real Zalo sender: `chat::send_message` on a private runtime, with a timeout.
/// A timeout is an ambiguous outcome; the caller already reserved the slot, so it counts as sent.
struct ZaloThat {
    rt: tokio::runtime::Runtime,
    phien: PhienChung,
    /// The account this database belongs to; the shared session may switch to another one.
    uin: String,
}

const TIMEOUT_GUI: Duration = Duration::from_secs(30);

impl GuiTin for ZaloThat {
    fn gui_tin(&self, zalo_id: &str, noi_dung: &str) -> Result<(), String> {
        self.rt.block_on(async {
            let phien = self.phien.lock().await.clone();
            if phien.login.as_ref().map(|l| l.uin.as_str()) != Some(self.uin.as_str()) {
                return Err(
                    "Đã đổi sang tài khoản Zalo khác, dừng gửi để không gửi nhầm nick".into(),
                );
            }
            let kq = tokio::time::timeout(
                TIMEOUT_GUI,
                crate::chat::send_message(&phien, zalo_id, noi_dung),
            )
            .await
            .map_err(|_| "Zalo không trả lời kịp (tin có thể đã được gửi)".to_string())?
            .map_err(|e| e.to_string())?;
            match kq
                .get("error_code")
                .and_then(serde_json::Value::as_i64)
                .filter(|c| *c != 0)
            {
                Some(code) => Err(format!("Zalo báo lỗi (mã {code})")),
                None => Ok(()),
            }
        })
    }
}

/// Start the sender for every Đợt gửi in state `dang_gui`, unless it is already running.
pub fn khoi_dong(uin: &str) {
    let uin = uin.to_string();
    let db_path = super::db::duong_dan(&uin);
    let Some(phien) = PHIEN.get().cloned() else {
        return;
    };
    let mut chay = DANG_CHAY.lock().unwrap_or_else(|e| e.into_inner());
    if *chay {
        return;
    }
    *chay = true;
    let spawn = std::thread::Builder::new()
        .name("cskh-sender".into())
        .spawn(move || {
            let ket_qua = catch_unwind(AssertUnwindSafe(|| chay_den_het(&db_path, phien, uin)));
            if let Err(e) =
                ket_qua.unwrap_or_else(|p| Err(anyhow::anyhow!("luồng gửi bị sập: {p:?}")))
            {
                if let Ok(conn) = super::db::mo(&db_path) {
                    let _ =
                        dot_gui::dung_het_vi_loi(&conn, &format!("Lỗi nội bộ, đã dừng gửi: {e}"));
                }
                // The normal exit clears the flag itself (under the lock, see `chay_den_het`).
                *DANG_CHAY.lock().unwrap_or_else(|e| e.into_inner()) = false;
            }
        });
    if spawn.is_err() {
        *chay = false;
    }
}

fn chay_den_het(db_path: &std::path::Path, phien: PhienChung, uin: String) -> anyhow::Result<()> {
    let conn = super::db::mo(db_path)?;
    let zalo = ZaloThat {
        rt: tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?,
        phien,
        uin,
    };
    let mut rng = rand::thread_rng();
    loop {
        if !dot_gui::co_dot_dang_gui(&conn)? {
            // Re-check under the lock `khoi_dong` takes, so a Đợt started just now is never left
            // without a sender.
            let mut chay = DANG_CHAY.lock().unwrap_or_else(|e| e.into_inner());
            if !dot_gui::co_dot_dang_gui(&conn)? {
                *chay = false;
                return Ok(());
            }
        }
        let ids: Vec<i64> = dot_gui::danh_sach_dot(&conn)?
            .into_iter()
            .rev()
            .filter(|d| d.trang_thai == dot_gui::DOT_DANG_GUI)
            .map(|d| d.id)
            .collect();
        let mut cho = false;
        for id in ids {
            cho |= matches!(
                dot_gui::gui_tiep(&conn, id, &zalo, &DongHoThat, &mut rng)?,
                LyDoDung::NgoaiGio | LyDoDung::HetGioiHan
            );
        }
        if cho {
            DongHoThat.ngu(CHO_KIEM_TRA_LAI_GIAY);
        }
    }
}
