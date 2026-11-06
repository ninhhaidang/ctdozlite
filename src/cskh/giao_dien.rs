//! The full-screen customer-care page (ADR-0004): Khách hàng, Mẫu tin, Đợt gửi.
//!
//! Quick local SQLite reads/writes run on the UI thread (a few rows, microseconds);
//! the Excel import (file dialog + parsing) and all sending run on their own threads.
// shortcut: the UI thread owns a small SQLite connection (INV-3 prefers worker-owned), fine for
// this volume; move to a worker command if lists grow into the tens of thousands.

use super::dot_gui::{self, Dot, Tin};
use super::khach_hang::{self, HoiThoai, KhachHang};
use super::mau_tin::{self, Mau, Nhom};
use super::{bo_gui, db};
use crate::chat::Thread;
use chrono::{Local, NaiveDate};
use eframe::egui::{self, RichText};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

const LAM_MOI_KHI_DANG_GUI: Duration = Duration::from_secs(2);
const NGAY_MAU: &str = "%d/%m/%Y";

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    KhachHang,
    MauTin,
    DotGui,
}

#[derive(Default)]
pub struct Trang {
    pub hoi_thoat: bool,
    uin: String,
    conn: Option<Rc<Connection>>,
    tab: Tab,
    thong_bao: String,
    // Khách hàng
    khach: Vec<KhachHang>,
    sua: HashMap<i64, [String; 3]>,
    nhap_rx: Option<Receiver<String>>,
    // Mẫu tin
    nhom: Vec<Nhom>,
    nhom_chon: Option<i64>,
    mau: Vec<Mau>,
    nhom_moi: String,
    doi_ten: String,
    mau_moi: String,
    sua_mau: HashMap<i64, String>,
    // Đợt gửi
    tu_ngay: String,
    den_ngay: String,
    ung_vien: Vec<KhachHang>,
    chon: HashSet<i64>,
    nhom_dot: Option<i64>,
    dot_ds: Vec<Dot>,
    dot_chon: Option<i64>,
    tin: Vec<Tin>,
    tin_xem: Option<i64>,
    toi_da: i64,
    lam_moi_luc: Option<Instant>,
}

fn nhan_dot(d: &Dot) -> &'static str {
    match d.trang_thai.as_str() {
        dot_gui::DOT_NHAP => "Nháp, chưa gửi",
        dot_gui::DOT_DANG_GUI => "Đang gửi",
        dot_gui::DOT_DUNG_VI_LOI => "Đã dừng vì lỗi",
        dot_gui::DOT_XONG => "Đã xong",
        _ => "?",
    }
}

fn nhan_tin(t: &str) -> &'static str {
    match t {
        dot_gui::TIN_CHO => "Chưa gửi",
        dot_gui::TIN_DANG_GUI => "Đã giữ chỗ, chưa rõ kết quả (không gửi lại)",
        dot_gui::TIN_DA_GUI => "Đã gửi",
        dot_gui::TIN_LOI => "Lỗi (không tự gửi lại)",
        dot_gui::TIN_XU_LY_TAY => "Xử lý tay",
        _ => "?",
    }
}

fn parse_ngay(s: &str) -> Result<Option<NaiveDate>, ()> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    NaiveDate::parse_from_str(s, NGAY_MAU)
        .map(Some)
        .map_err(|_| ())
}

fn hien_ngay(iso: &Option<String>) -> String {
    iso.as_deref()
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .map(|d| d.format(NGAY_MAU).to_string())
        .unwrap_or_default()
}

/// Native "choose an Excel file" dialog; blocks, so only call it off the UI thread.
fn chon_file_excel() -> Option<std::path::PathBuf> {
    use std::process::Command;
    let out = if cfg!(target_os = "macos") {
        Command::new("osascript")
            .args(["-e", "POSIX path of (choose file with prompt \"Chọn file Excel khách hàng\" of type {\"org.openxmlformats.spreadsheetml.sheet\"})"])
            .output()
    } else if cfg!(target_os = "windows") {
        Command::new("powershell")
            .args(["-NoProfile", "-Command", "Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.OpenFileDialog; $d.Filter = 'Excel (*.xlsx)|*.xlsx'; if ($d.ShowDialog() -eq 'OK') { $d.FileName }"])
            .output()
    } else {
        Command::new("zenity")
            .args(["--file-selection", "--title=Chọn file Excel khách hàng", "--file-filter=*.xlsx"])
            .output()
    }
    .ok()?;
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !path.is_empty()).then(|| path.into())
}

impl Trang {
    /// Open (or re-open after an account switch) the database; resumes a Đợt gửi left running.
    fn mo(&mut self, uin: &str) {
        *self = Self {
            hoi_thoat: self.hoi_thoat,
            ..Self::default()
        };
        self.uin = uin.to_string();
        if uin.is_empty() {
            return;
        }
        match db::mo(&db::duong_dan(uin)) {
            Ok(c) => self.conn = Some(Rc::new(c)),
            Err(e) => {
                self.thong_bao = format!("Không mở được dữ liệu chăm sóc khách hàng: {e}");
                return;
            }
        }
        self.tai_lai();
        if self.lam(dot_gui::co_dot_dang_gui).unwrap_or(false) {
            bo_gui::khoi_dong(uin);
        }
    }

    fn lam<T>(&mut self, f: impl FnOnce(&Connection) -> anyhow::Result<T>) -> Option<T> {
        let c = self.conn.clone()?;
        match f(&c) {
            Ok(v) => Some(v),
            Err(e) => {
                self.thong_bao = e.to_string();
                None
            }
        }
    }

    fn tai_lai(&mut self) {
        if let Some(v) = self.lam(khach_hang::danh_sach) {
            self.khach = v;
            self.sua.clear();
        }
        if let Some(v) = self.lam(mau_tin::danh_sach_nhom) {
            if !v.iter().any(|n| Some(n.id) == self.nhom_chon) {
                self.nhom_chon = v.first().map(|n| n.id);
                self.doi_ten = v.first().map(|n| n.ten.clone()).unwrap_or_default();
            }
            if !v.iter().any(|n| Some(n.id) == self.nhom_dot) {
                self.nhom_dot = v.first().map(|n| n.id);
            }
            self.nhom = v;
        }
        self.tai_lai_mau();
        self.tai_lai_dot();
        if let Some(n) = self.lam(dot_gui::toi_da_ngay) {
            self.toi_da = n;
        }
        self.loc_ung_vien();
    }

    fn tai_lai_mau(&mut self) {
        self.sua_mau.clear();
        self.mau = match self.nhom_chon {
            Some(id) => self
                .lam(|c| mau_tin::danh_sach_mau(c, id))
                .unwrap_or_default(),
            None => Vec::new(),
        };
    }

    fn tai_lai_dot(&mut self) {
        if let Some(v) = self.lam(dot_gui::danh_sach_dot) {
            if !v.iter().any(|d| Some(d.id) == self.dot_chon) {
                self.dot_chon = v.first().map(|d| d.id);
            }
            self.dot_ds = v;
        }
        self.tin = match self.dot_chon {
            Some(id) => self
                .lam(|c| dot_gui::tin_cua_dot(c, id))
                .unwrap_or_default(),
            None => Vec::new(),
        };
        self.lam_moi_luc = Some(Instant::now());
    }

    fn loc_ung_vien(&mut self) {
        let (Ok(tu), Ok(den)) = (parse_ngay(&self.tu_ngay), parse_ngay(&self.den_ngay)) else {
            self.thong_bao = "Ngày lọc phải có dạng ngày/tháng/năm, ví dụ 25/12/2025".into();
            return;
        };
        let now = Local::now().naive_local();
        if let Some(v) = self.lam(|c| dot_gui::ung_vien(c, now, tu, den)) {
            self.chon.retain(|id| v.iter().any(|k| k.id == *id));
            self.ung_vien = v;
        }
    }

    /// Draw the page. Returns true when the seller leaves it.
    pub fn ve(&mut self, ctx: &egui::Context, uin: &str, threads: &[Thread]) -> bool {
        if self.uin != uin || (self.conn.is_none() && !uin.is_empty() && self.thong_bao.is_empty())
        {
            self.mo(uin);
        }
        if let Some(rx) = &self.nhap_rx {
            if let Ok(tom_tat) = rx.try_recv() {
                self.thong_bao = tom_tat;
                self.nhap_rx = None;
                self.tai_lai();
            }
        }
        let dang_gui = bo_gui::dang_chay();
        if self.tab == Tab::DotGui
            && (dang_gui
                || self
                    .dot_ds
                    .iter()
                    .any(|d| d.trang_thai == dot_gui::DOT_DANG_GUI))
            && self
                .lam_moi_luc
                .is_none_or(|t| t.elapsed() >= LAM_MOI_KHI_DANG_GUI)
        {
            self.tai_lai_dot();
        }
        if self.nhap_rx.is_some() || dang_gui {
            ctx.request_repaint_after(LAM_MOI_KHI_DANG_GUI);
        }
        let mut thoat = false;
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("← Quay lại tin nhắn").clicked() {
                    thoat = true;
                }
                ui.heading("Chăm sóc khách hàng");
                ui.add_space(12.0);
                ui.selectable_value(&mut self.tab, Tab::KhachHang, "Khách hàng");
                ui.selectable_value(&mut self.tab, Tab::MauTin, "Mẫu tin");
                if ui
                    .selectable_value(&mut self.tab, Tab::DotGui, "Đợt gửi")
                    .clicked()
                {
                    self.tai_lai_dot();
                    self.loc_ung_vien();
                }
            });
            if !self.thong_bao.is_empty() {
                ui.horizontal(|ui| {
                    ui.colored_label(
                        egui::Color32::from_rgb(220, 140, 20),
                        self.thong_bao.clone(),
                    );
                    if ui.small_button("Ẩn").clicked() {
                        self.thong_bao.clear();
                    }
                });
            }
            ui.separator();
            if self.conn.is_none() {
                ui.label("Chưa đăng nhập tài khoản Zalo.");
                return;
            }
            match self.tab {
                Tab::KhachHang => self.ve_khach_hang(ui, threads),
                Tab::MauTin => self.ve_mau_tin(ui),
                Tab::DotGui => self.ve_dot_gui(ui),
            }
        });
        thoat
    }

    fn ve_khach_hang(&mut self, ui: &mut egui::Ui, threads: &[Thread]) {
        ui.horizontal(|ui| {
            if ui.button("Thêm từ hội thoại 1-1 trong Zalo").clicked() {
                let ht: Vec<HoiThoai> = threads
                    .iter()
                    .filter(|t| !t.is_group && !t.name.contains("Cloud của tôi"))
                    .map(|t| HoiThoai {
                        id: &t.id,
                        ten: &t.name,
                        la_nhom: false,
                    })
                    .collect();
                if let Some((moi, da_co)) = self.lam(|c| khach_hang::them_tu_hoi_thoai(c, &ht)) {
                    self.thong_bao =
                        format!("Đã thêm {moi} khách hàng mới, {da_co} đã có từ trước.");
                    self.tai_lai();
                }
            }
            let dang_nhap = self.nhap_rx.is_some();
            if ui
                .add_enabled(!dang_nhap, egui::Button::new("Nhập từ Excel…"))
                .clicked()
            {
                let (tx, rx) = channel();
                self.nhap_rx = Some(rx);
                let duong_dan = db::duong_dan(&self.uin);
                std::thread::spawn(move || {
                    let tom_tat = match chon_file_excel() {
                        None => "Chưa chọn file Excel.".to_string(),
                        Some(file) => match db::mo(&duong_dan) {
                            Err(e) => format!("Không mở được dữ liệu: {e}"),
                            Ok(conn) => {
                                let kq = khach_hang::nhap_excel(&conn, &file);
                                let mut s = format!("Đã nhập {} dòng.", kq.so_dong);
                                for l in kq.loi.iter().take(8) {
                                    s += &format!("\n{l}");
                                }
                                if kq.loi.len() > 8 {
                                    s += &format!("\n… và {} lỗi khác", kq.loi.len() - 8);
                                }
                                s
                            }
                        },
                    };
                    let _ = tx.send(tom_tat);
                });
            }
            ui.label("File cần 4 cột: Số điện thoại, Tên, Xưng hô, Ngày mua gần nhất.");
        });
        ui.label(format!("{} khách hàng", self.khach.len()));
        ui.add_space(4.0);
        let mut luu: Option<(i64, [String; 3])> = None;
        egui::ScrollArea::vertical().auto_shrink(false).show_rows(
            ui,
            28.0,
            self.khach.len(),
            |ui, range| {
                for k in &self.khach[range] {
                    let goc = [
                        k.ten.clone().unwrap_or_default(),
                        k.xung_ho.clone(),
                        hien_ngay(&k.ngay_mua_gan_nhat),
                    ];
                    let o = self.sua.entry(k.id).or_insert_with(|| goc.clone());
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [120.0, 22.0],
                            egui::Label::new(k.so_dien_thoai.as_deref().unwrap_or("—")),
                        );
                        ui.add_sized(
                            [180.0, 22.0],
                            egui::TextEdit::singleline(&mut o[0]).hint_text("Tên"),
                        );
                        ui.add_sized(
                            [90.0, 22.0],
                            egui::TextEdit::singleline(&mut o[1]).hint_text("Xưng hô"),
                        );
                        ui.add_sized(
                            [110.0, 22.0],
                            egui::TextEdit::singleline(&mut o[2]).hint_text("dd/mm/yyyy"),
                        );
                        let zalo = match (&k.zalo_id, &k.ten_zalo) {
                            (Some(_), Some(t)) => format!("Zalo: {t}"),
                            (Some(_), None) => "Đã liên kết Zalo".into(),
                            _ => "Chưa liên kết Zalo".into(),
                        };
                        ui.label(zalo);
                        if *o != goc && ui.button("Lưu").clicked() {
                            luu = Some((k.id, o.clone()));
                        }
                    });
                }
            },
        );
        if let Some((id, o)) = luu {
            if self
                .lam(|c| khach_hang::cap_nhat(c, id, &o[0], &o[1], &o[2]))
                .is_some()
            {
                self.tai_lai();
            }
        }
    }

    fn ve_mau_tin(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(240.0);
                ui.strong("Nhóm mẫu");
                let mut doi: Option<Nhom> = None;
                for n in &self.nhom {
                    if ui.selectable_label(self.nhom_chon == Some(n.id), &n.ten).clicked() {
                        doi = Some(n.clone());
                    }
                }
                if let Some(n) = doi {
                    self.nhom_chon = Some(n.id);
                    self.doi_ten = n.ten;
                    self.tai_lai_mau();
                }
                ui.add_space(6.0);
                ui.add(egui::TextEdit::singleline(&mut self.nhom_moi).hint_text("Tên nhóm mới"));
                if ui.button("Thêm nhóm").clicked() {
                    let ten = self.nhom_moi.clone();
                    if self.lam(|c| mau_tin::them_nhom(c, &ten)).is_some() {
                        self.nhom_moi.clear();
                        self.nhom_chon = None;
                        self.tai_lai();
                        self.nhom_chon = self.nhom.iter().find(|n| n.ten == mau_tin::nfc(ten.trim())).map(|n| n.id);
                        self.doi_ten = ten.trim().to_string();
                        self.tai_lai_mau();
                    }
                }
            });
            ui.separator();
            ui.vertical(|ui| {
                let Some(nhom_id) = self.nhom_chon else {
                    ui.label("Chưa có Nhóm mẫu. Hãy thêm một nhóm ở bên trái.");
                    return;
                };
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.doi_ten).desired_width(220.0));
                    if ui.button("Đổi tên").clicked() {
                        let ten = self.doi_ten.clone();
                        if self.lam(|c| mau_tin::doi_ten_nhom(c, nhom_id, &ten)).is_some() {
                            self.tai_lai();
                        }
                    }
                    if ui.button("Xóa nhóm").clicked()
                        && self.lam(|c| mau_tin::xoa_nhom(c, nhom_id)).is_some()
                    {
                        self.nhom_chon = None;
                        self.tai_lai();
                    }
                });
                ui.label(
                    "Biến dùng được: {xưng_hô}, {tên}. Mỗi khách hàng nhận ngẫu nhiên một mẫu trong nhóm; \
                     khách chưa có tên chỉ nhận mẫu không chứa {tên}.",
                );
                ui.separator();
                let mut luu: Option<(i64, String)> = None;
                let mut xoa: Option<i64> = None;
                for m in &self.mau {
                    let o = self.sua_mau.entry(m.id).or_insert_with(|| m.noi_dung.clone());
                    ui.add(egui::TextEdit::multiline(o).desired_rows(2).desired_width(f32::INFINITY));
                    xem_truoc(ui, o);
                    ui.horizontal(|ui| {
                        if *o != m.noi_dung && ui.button("Lưu").clicked() {
                            luu = Some((m.id, o.clone()));
                        }
                        if ui.button("Xóa mẫu").clicked() {
                            xoa = Some(m.id);
                        }
                    });
                    ui.separator();
                }
                if let Some((id, nd)) = luu {
                    if self.lam(|c| mau_tin::sua_mau(c, id, &nd)).is_some() {
                        self.tai_lai_mau();
                    }
                }
                if let Some(id) = xoa {
                    if self.lam(|c| mau_tin::xoa_mau(c, id)).is_some() {
                        self.tai_lai_mau();
                    }
                }
                ui.strong("Thêm mẫu mới");
                ui.add(egui::TextEdit::multiline(&mut self.mau_moi).desired_rows(2).desired_width(f32::INFINITY));
                xem_truoc(ui, &self.mau_moi);
                if ui.button("Thêm mẫu").clicked() {
                    let nd = self.mau_moi.clone();
                    if self.lam(|c| mau_tin::them_mau(c, nhom_id, &nd)).is_some() {
                        self.mau_moi.clear();
                        self.tai_lai_mau();
                    }
                }
            });
        });
    }

    fn ve_dot_gui(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                self.ve_tao_dot(ui);
                ui.separator();
                self.ve_cac_dot(ui);
            });
    }

    fn ve_tao_dot(&mut self, ui: &mut egui::Ui) {
        ui.strong("1. Chọn khách hàng cho Đợt gửi mới");
        ui.label(format!(
            "Chỉ hiện khách đã liên kết Zalo và chưa được hỏi thăm trong {} ngày qua.",
            dot_gui::NGAY_KHONG_HOI_LAI
        ));
        ui.horizontal(|ui| {
            ui.label("Ngày mua từ");
            ui.add(
                egui::TextEdit::singleline(&mut self.tu_ngay)
                    .hint_text("dd/mm/yyyy")
                    .desired_width(90.0),
            );
            ui.label("đến");
            ui.add(
                egui::TextEdit::singleline(&mut self.den_ngay)
                    .hint_text("dd/mm/yyyy")
                    .desired_width(90.0),
            );
            if ui.button("Lọc").clicked() {
                self.loc_ung_vien();
            }
            if ui.button("Chọn tất cả").clicked() {
                self.chon = self.ung_vien.iter().map(|k| k.id).collect();
            }
            if ui.button("Bỏ chọn").clicked() {
                self.chon.clear();
            }
            ui.label(format!(
                "Đã chọn {}/{}",
                self.chon.len(),
                self.ung_vien.len()
            ));
        });
        egui::ScrollArea::vertical()
            .id_salt("ung_vien")
            .max_height(180.0)
            .show_rows(ui, 24.0, self.ung_vien.len(), |ui, range| {
                for k in &self.ung_vien[range] {
                    let mut c = self.chon.contains(&k.id);
                    let ten = format!(
                        "{} · {} · mua {}",
                        k.ten_hien(),
                        k.xung_ho,
                        hien_ngay(&k.ngay_mua_gan_nhat)
                    );
                    if ui.checkbox(&mut c, ten).changed() {
                        if c {
                            self.chon.insert(k.id)
                        } else {
                            self.chon.remove(&k.id)
                        };
                    }
                }
            });
        ui.horizontal(|ui| {
            ui.label("Nhóm mẫu:");
            let ten_nhom = self
                .nhom
                .iter()
                .find(|n| Some(n.id) == self.nhom_dot)
                .map_or("(chưa có)".to_string(), |n| n.ten.clone());
            egui::ComboBox::from_id_salt("nhom_dot")
                .selected_text(ten_nhom)
                .show_ui(ui, |ui| {
                    for n in &self.nhom {
                        ui.selectable_value(&mut self.nhom_dot, Some(n.id), &n.ten);
                    }
                });
            let san_sang = self.nhom_dot.is_some() && !self.chon.is_empty();
            if ui
                .add_enabled(san_sang, egui::Button::new("Tạo Đợt gửi (chưa gửi)"))
                .clicked()
            {
                let (nhom, ids): (i64, Vec<i64>) = (
                    self.nhom_dot.unwrap_or_default(),
                    self.chon.iter().copied().collect(),
                );
                let now = Local::now().naive_local();
                if let Some(id) = self.lam(|c| {
                    if mau_tin::danh_sach_mau(c, nhom)?.is_empty() {
                        anyhow::bail!("Nhóm mẫu này chưa có Mẫu tin nào.");
                    }
                    dot_gui::tao(c, nhom, &ids, now, &mut rand::thread_rng())
                }) {
                    self.dot_chon = Some(id);
                    self.chon.clear();
                    self.tai_lai_dot();
                    self.loc_ung_vien();
                }
            }
        });
    }

    fn ve_cac_dot(&mut self, ui: &mut egui::Ui) {
        ui.strong("2. Duyệt và gửi");
        ui.horizontal(|ui| {
            ui.label(format!(
                "Chỉ gửi trong khung {}h–{}h, cách nhau ngẫu nhiên {}–{} phút. Tối đa",
                dot_gui::GIO_BAT_DAU,
                dot_gui::GIO_KET_THUC,
                dot_gui::GIAN_CACH_GIAY.0 as u32 / 60,
                dot_gui::GIAN_CACH_GIAY.1 as u32 / 60
            ));
            ui.add(egui::DragValue::new(&mut self.toi_da).range(0..=1000));
            ui.label("tin/ngày (chỉ tính tin của Đợt gửi).");
            if ui.button("Lưu").clicked() {
                let n = self.toi_da;
                self.lam(|c| dot_gui::dat_toi_da_ngay(c, n));
            }
        });
        if self.dot_ds.is_empty() {
            ui.label("Chưa có Đợt gửi nào.");
            return;
        }
        ui.horizontal_wrapped(|ui| {
            let mut doi = None;
            for d in &self.dot_ds {
                let nhan = format!(
                    "Đợt #{} · {} · {}",
                    d.id,
                    d.tao_luc.replace('T', " "),
                    nhan_dot(d)
                );
                if ui
                    .selectable_label(self.dot_chon == Some(d.id), nhan)
                    .clicked()
                {
                    doi = Some(d.id);
                }
            }
            if doi.is_some() {
                self.dot_chon = doi;
                self.tin_xem = None;
                self.tai_lai_dot();
            }
        });
        let Some(dot) = self
            .dot_ds
            .iter()
            .find(|d| Some(d.id) == self.dot_chon)
            .cloned()
        else {
            return;
        };
        if let Some(loi) = &dot.loi {
            ui.colored_label(egui::Color32::from_rgb(200, 50, 50), format!("Lỗi: {loi}"));
        }
        if let Some(loi) = self
            .tin
            .iter()
            .find(|t| t.trang_thai == dot_gui::TIN_LOI)
            .and_then(|t| t.loi.clone())
        {
            ui.colored_label(
                egui::Color32::from_rgb(200, 50, 50),
                format!("Đã dừng mọi Đợt gửi vì lỗi đầu tiên: {loi}. Tin lỗi sẽ không tự gửi lại."),
            );
        }
        let sua_duoc = dot.trang_thai != dot_gui::DOT_XONG;
        let (so_cho, so_chon) = (
            self.tin
                .iter()
                .filter(|t| t.trang_thai == dot_gui::TIN_CHO)
                .count(),
            self.tin
                .iter()
                .filter(|t| t.trang_thai == dot_gui::TIN_CHO && t.chon)
                .count(),
        );
        ui.horizontal(|ui| {
            let nut = match dot.trang_thai.as_str() {
                dot_gui::DOT_NHAP => Some("Gửi đợt này"),
                dot_gui::DOT_DUNG_VI_LOI => Some("Tiếp tục gửi (bỏ qua tin lỗi)"),
                _ => None,
            };
            if let Some(nut) = nut {
                if ui
                    .add_enabled(so_chon > 0, egui::Button::new(RichText::new(nut).strong()))
                    .clicked()
                {
                    let id = dot.id;
                    if self.lam(|c| dot_gui::bat_dau(c, id)).is_some() {
                        bo_gui::khoi_dong(&self.uin);
                        self.tai_lai_dot();
                    }
                }
            }
            ui.label(format!(
                "{so_chon} tin được chọn gửi / {so_cho} tin chưa gửi."
            ));
            if dot.trang_thai == dot_gui::DOT_DANG_GUI {
                ui.label("Đang gửi theo nhịp; đóng ứng dụng là tạm dừng, mở lại sẽ gửi tiếp.");
            }
        });
        let mut doi_chon: Option<(i64, bool)> = None;
        egui::ScrollArea::vertical()
            .id_salt("tin")
            .max_height(320.0)
            .show_rows(ui, 24.0, self.tin.len(), |ui, range| {
                for t in &self.tin[range] {
                    ui.horizontal(|ui| {
                        let mut c = t.chon;
                        let duoc_tick = sua_duoc && t.trang_thai == dot_gui::TIN_CHO;
                        if ui
                            .add_enabled(duoc_tick, egui::Checkbox::without_text(&mut c))
                            .changed()
                        {
                            doi_chon = Some((t.id, c));
                        }
                        ui.add_sized([160.0, 20.0], egui::Label::new(&t.ten_khach).truncate());
                        ui.add_sized(
                            [100.0, 20.0],
                            egui::Label::new(nhan_tin(&t.trang_thai)).truncate(),
                        );
                        if ui
                            .add(
                                egui::Label::new(&t.noi_dung)
                                    .truncate()
                                    .sense(egui::Sense::click()),
                            )
                            .on_hover_text("Bấm để xem đầy đủ")
                            .clicked()
                        {
                            self.tin_xem = Some(t.id);
                        }
                    });
                }
            });
        if let Some(t) = self.tin.iter().find(|t| Some(t.id) == self.tin_xem) {
            ui.group(|ui| {
                ui.strong(format!("Tin gửi cho {}", t.ten_khach));
                ui.label(if t.noi_dung.is_empty() {
                    "(không có Mẫu tin phù hợp, hãy nhắn tay)"
                } else {
                    &t.noi_dung
                });
                if let Some(l) = &t.loi {
                    ui.colored_label(egui::Color32::from_rgb(200, 50, 50), l);
                }
            });
        }
        if let Some((id, c)) = doi_chon {
            if self.lam(|conn| dot_gui::dat_chon(conn, id, c)).is_some() {
                self.tai_lai_dot();
            }
        }
    }

    /// Confirmation box shown when the seller closes the window while the sender runs.
    /// Returns true when they chose to quit anyway.
    pub fn ve_hoi_thoat(&mut self, ctx: &egui::Context) -> bool {
        if !self.hoi_thoat {
            return false;
        }
        let mut thoat = false;
        egui::Window::new("Đang gửi tin cho khách hàng")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(
                    "Đóng ứng dụng bây giờ sẽ tạm dừng việc gửi. Mở lại ứng dụng và vào trang \
                     Chăm sóc khách hàng để gửi tiếp. Tin đã gửi hoặc đã giữ chỗ sẽ không bị gửi lại.",
                );
                ui.horizontal(|ui| {
                    if ui.button("Tiếp tục gửi").clicked() {
                        self.hoi_thoat = false;
                    }
                    if ui.button("Vẫn đóng ứng dụng").clicked() {
                        thoat = true;
                    }
                });
            });
        thoat
    }
}

/// Preview a Mẫu tin with a sample customer with a name and one without.
fn xem_truoc(ui: &mut egui::Ui, noi_dung: &str) {
    if noi_dung.trim().is_empty() {
        return;
    }
    let sai = mau_tin::bien_khong_hop_le(noi_dung);
    if !sai.is_empty() {
        ui.colored_label(
            egui::Color32::from_rgb(200, 50, 50),
            format!("Biến không hỗ trợ: {}", sai.join(", ")),
        );
        return;
    }
    if let Ok(s) = mau_tin::dien_mau(noi_dung, "chị", Some("Lan")) {
        ui.weak(format!("Xem trước (chị Lan): {s}"));
    }
    match mau_tin::dien_mau(noi_dung, khach_hang::XUNG_HO_MAC_DINH, None) {
        Ok(s) => ui.weak(format!("Xem trước (khách chưa có tên): {s}")),
        Err(_) => ui.weak("Khách chưa có tên sẽ không nhận mẫu này (chứa {tên})."),
    };
}
