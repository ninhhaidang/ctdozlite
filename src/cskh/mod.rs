//! Customer-care tool (ADR-0004): Khách hàng, Mẫu tin, Đợt gửi and the paced sender.
//!
//! Everything here is new code kept out of the upstream files so upstream merges stay easy.
//! The pacing rules (ADR-0001, ADR-0003) live in `dot_gui` behind the `GuiTin` / `DongHo`
//! traits so `cargo test` can check them without a real Zalo account.

pub mod bo_gui;
pub mod db;
pub mod dot_gui;
pub mod giao_dien;
pub mod khach_hang;
pub mod mau_tin;
