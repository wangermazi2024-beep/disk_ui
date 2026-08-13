//! DiskLens 库入口（v2 — WinDirStat 风格重构）。
//!
//! 纯逻辑模块作为 lib target 暴露，让 verify_mft 和单元测试能在无 eframe 环境下编译。

pub mod applog;
pub mod categorize;
pub mod dir_enum;
pub mod disk_info;
pub mod export;
pub mod file_ops;
pub mod format;
pub mod model;
#[cfg(windows)]
pub mod mft_scan;
pub mod scan;
