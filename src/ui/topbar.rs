//! 顶部菜单栏：文件 / 视图 / 工具 / 帮助。
//! 品牌标题不放在这里——挪到窗口左下角的状态条了（见 app.rs）。

use egui::{Color32, RichText};

pub enum TopbarAction {
    None,
    AddScan,
    ExportCsv,
    ToggleShowAll,
    #[cfg(windows)]
    RestartAsAdmin,
}

pub struct TopbarState<'a> {
    pub scanning: bool,
    pub scanned_count: u64,
    pub scan_error: Option<&'a str>,
    pub has_result: bool,
    pub show_all_details: bool,
    #[cfg(windows)]
    pub is_admin: bool,
}

pub fn show(ui: &mut egui::Ui, state: TopbarState) -> TopbarAction {
    let mut action = TopbarAction::None;
    egui::Panel::top("menu_bar").exact_size(34.0)
        .frame(egui::Frame::default().fill(Color32::from_rgb(0x2E, 0x2E, 0x32))
            .inner_margin(egui::Margin::symmetric(10, 4)))
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.menu_button("文件", |ui| {
                    if ui.button("添加扫描…").clicked() {
                        action = TopbarAction::AddScan;
                        ui.close();
                    }
                    ui.add_enabled_ui(state.has_result, |ui| {
                        if ui.button("导出 CSV…").clicked() {
                            action = TopbarAction::ExportCsv;
                            ui.close();
                        }
                    });
                });

                ui.menu_button("视图", |ui| {
                    let mut show_all = state.show_all_details;
                    if ui.checkbox(&mut show_all, "显示全部信息（含元数据文件）").changed() {
                        action = TopbarAction::ToggleShowAll;
                        ui.close();
                    }
                });

                #[cfg(windows)]
                if !state.is_admin {
                    ui.menu_button("工具", |ui| {
                        if ui.button("⚡ 以管理员身份重启").clicked() {
                            action = TopbarAction::RestartAsAdmin;
                            ui.close();
                        }
                    });
                }

                ui.menu_button("帮助", |ui| {
                    ui.label(RichText::new("DiskForge WMS").strong());
                    ui.label(RichText::new("由 WMS 开发").size(11.0).color(Color32::from_rgb(0xA0, 0xA0, 0xA0)));
                });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if state.scanning {
                        ui.add(egui::Spinner::new());
                        ui.label(RichText::new(format!("正在扫描… 已发现 {} 项", state.scanned_count))
                            .color(Color32::from_rgb(0xA0, 0xA0, 0xA0)));
                    } else if let Some(err) = state.scan_error {
                        ui.label(RichText::new(err).color(Color32::from_rgb(0xE0, 0x55, 0x5B)));
                    }
                });
            });
        });
    action
}
