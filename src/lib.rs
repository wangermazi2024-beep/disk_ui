//! DiskLens 库入口。
//!
//! 这个文件存在的目的是把"可在 Linux 上单元测试"的模块暴露成一个 lib target，
//! 让 `src/bin/verify_mft.rs` 和单元测试都可以直接 `use disk_ui::...`，
//! 而不需要依赖 eframe（eframe 在无 X11 头的 Linux 上编不过）。
//!
//! GUI 主程序（`src/main.rs`）自己挂 `gui` feature，用 eframe；
//! 非图形工具和单元测试不挂这个 feature，只用本 lib 暴露的纯逻辑模块。

pub mod categorize;
pub mod disk_info;
pub mod format;
pub mod mft_parse;
pub mod model;
#[cfg(windows)]
pub mod mft_scan;
#[cfg(windows)]
pub mod mft_verify;
pub mod scan;
