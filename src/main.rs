mod deploy;
mod hzpak;
mod validate;

use eframe::egui::{self, Color32, RichText};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use validate::{format_size, ScanReport};

enum WorkResult {
    Scan(ScanReport),
    Deploy(String),
}

type JobCell = Arc<Mutex<Option<Result<WorkResult, String>>>>;

struct ToolkitApp {
    source: String,
    sd_root: String,
    scan: Option<ScanReport>,
    job: Option<JobCell>,
    notice: Option<(bool, String)>,
}

impl Default for ToolkitApp {
    fn default() -> Self {
        Self {
            source: String::new(),
            sd_root: String::new(),
            scan: None,
            job: None,
            notice: None,
        }
    }
}

impl ToolkitApp {
    fn busy(&self) -> bool {
        self.job.is_some()
    }

    fn start_job(
        &mut self,
        name: &'static str,
        task: impl FnOnce() -> Result<WorkResult, String> + Send + 'static,
    ) {
        if self.busy() {
            return;
        }

        let cell: JobCell = Arc::new(Mutex::new(None));
        let worker_cell = Arc::clone(&cell);
        let spawn = thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(task))
                    .unwrap_or_else(|_| Err("Операция аварийно остановилась. Проверь лог и повтори попытку.".into()));
                if let Ok(mut slot) = worker_cell.lock() {
                    *slot = Some(result);
                }
            });

        match spawn {
            Ok(_) => {
                self.notice = None;
                self.job = Some(cell);
            }
            Err(error) => {
                self.notice = Some((false, format!("Не удалось запустить фоновую задачу: {error}")));
            }
        }
    }

    fn check_source(&mut self) {
        let source = PathBuf::from(self.source.trim());
        if self.source.trim().is_empty() {
            self.notice = Some((false, "Сначала выбери папку подготовленных ресурсов.".into()));
            return;
        }

        self.scan = None;
        self.start_job("resource-validation", move || {
            validate::validate_source(&source).map(WorkResult::Scan)
        });
    }

    fn deploy(&mut self) {
        if self.scan.is_none() {
            self.notice = Some((false, "Сначала проверь исходную папку.".into()));
            return;
        }
        let source = PathBuf::from(self.source.trim());
        let sd_root = PathBuf::from(self.sd_root.trim());
        if self.sd_root.trim().is_empty() {
            self.notice = Some((false, "Сначала выбери корень SD-карты.".into()));
            return;
        }

        self.start_job("sd-deployment", move || {
            deploy::deploy_to_sd(&source, &sd_root).map(WorkResult::Deploy)
        });
    }

    fn poll_job(&mut self, ctx: &egui::Context) {
        let completed = self.job.as_ref().and_then(|cell| {
            let mut guard = cell.try_lock().ok()?;
            guard.take()
        });

        if let Some(result) = completed {
            self.job = None;
            match result {
                Ok(WorkResult::Scan(report)) => {
                    self.notice = Some((true, "Проверка завершена. Исходная папка подходит для упаковки.".into()));
                    self.scan = Some(report);
                }
                Ok(WorkResult::Deploy(message)) => {
                    self.notice = Some((true, message));
                    self.scan = None;
                }
                Err(message) => {
                    self.notice = Some((false, message));
                }
            }
        } else if self.busy() {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
    }

    fn path_row(
        ui: &mut egui::Ui,
        label: &str,
        value: &mut String,
        dialog_title: &str,
        enabled: bool,
    ) -> bool {
        let mut changed = false;
        ui.label(RichText::new(label).strong());
        ui.horizontal(|ui| {
            let response = ui.add_enabled(
                enabled,
                egui::TextEdit::singleline(value).desired_width(f32::INFINITY),
            );
            changed |= response.changed();
            if ui
                .add_enabled(enabled, egui::Button::new("Обзор…"))
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title(dialog_title)
                    .pick_folder()
                {
                    *value = path.display().to_string();
                    changed = true;
                }
            }
        });
        changed
    }
}

impl eframe::App for ToolkitApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_job(ctx);
        ctx.set_visuals(egui::Visuals::dark());

        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.heading(RichText::new("NX Resource Toolkit").size(26.0).strong());
                ui.add_space(12.0);
                ui.label(RichText::new("Horizon NX · Resource Manager").color(Color32::from_rgb(135, 169, 220)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new("MVP 0.1.0").weak());
                });
            });
            ui.add_space(8.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(8.0);
            ui.label(
                RichText::new("Проверка, упаковка и установка уже подготовленных ресурсов на SD-карту.")
                    .color(Color32::from_rgb(190, 198, 210)),
            );
            ui.add_space(14.0);

            let path_inputs_enabled = !self.busy();
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.heading("1. Исходные ресурсы");
                ui.add_space(5.0);
                let changed = Self::path_row(
                    ui,
                    "Папка с horizon-resources.json, assets/ и data/",
                    &mut self.source,
                    "Выбери корень подготовленных ресурсов",
                    path_inputs_enabled,
                );
                if changed {
                    self.scan = None;
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!self.busy(), egui::Button::new("Проверить папку"))
                        .clicked()
                    {
                        self.check_source();
                    }
                    if self.busy() {
                        ui.spinner();
                        ui.label("Выполняется проверка или копирование…");
                    }
                });
            });

            ui.add_space(10.0);
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.heading("2. SD-карта Nintendo Switch");
                ui.add_space(5.0);
                Self::path_row(
                    ui,
                    "Корень SD-карты (внутри должна быть папка switch/)",
                    &mut self.sd_root,
                    "Выбери корень SD-карты",
                    path_inputs_enabled,
                );
                ui.add_space(8.0);
                ui.label(RichText::new("Сначала создаётся временная копия и проверяется HZPAK. Предыдущая папка resources/ и resources.hzpak сохраняются в switch/horizon-backups/.").weak());
                ui.add_space(6.0);
                let can_deploy = !self.busy()
                    && self.scan.is_some()
                    && !self.sd_root.trim().is_empty()
                    && !self.source.trim().is_empty();
                if ui
                    .add_enabled(can_deploy, egui::Button::new(RichText::new("Установить ресурсы на SD").strong()))
                    .clicked()
                {
                    self.deploy();
                }
            });

            if let Some(report) = &self.scan {
                ui.add_space(10.0);
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.heading("Результат проверки");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new("ГОТОВО К УПАКОВКЕ").color(Color32::from_rgb(110, 205, 145)).strong());
                        });
                    });
                    ui.add_space(4.0);
                    ui.label(RichText::new(report.root.display().to_string()).weak());
                    egui::Grid::new("report-grid")
                        .num_columns(2)
                        .spacing([24.0, 5.0])
                        .show(ui, |ui| {
                            ui.label("Файлов");
                            ui.label(report.files.to_string());
                            ui.end_row();
                            ui.label("Папок");
                            ui.label(report.directories.to_string());
                            ui.end_row();
                            ui.label("Общий размер");
                            ui.label(format_size(report.total_bytes));
                            ui.end_row();
                            ui.label("В HZPAK");
                            ui.label(format_size(report.packed_bytes));
                            ui.end_row();
                            ui.label("Останутся отдельными");
                            ui.label(format!("{} · {}", report.loose_files, format_size(report.loose_bytes)));
                            ui.end_row();
                        });
                    if !report.manifest_keys.is_empty() {
                        ui.add_space(5.0);
                        ui.label(RichText::new(format!("Ключи manifest: {}", report.manifest_keys.join(", "))).weak().small());
                    }
                    for warning in &report.warnings {
                        ui.label(RichText::new(format!("• {warning}")).color(Color32::from_rgb(232, 190, 105)));
                    }
                });
            }

            if let Some((success, message)) = &self.notice {
                ui.add_space(10.0);
                let color = if *success {
                    Color32::from_rgb(115, 210, 150)
                } else {
                    Color32::from_rgb(245, 135, 125)
                };
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.label(RichText::new(message).color(color));
                });
            }

            ui.add_space(8.0);
            ui.separator();
            ui.label(RichText::new("Инструмент не загружает Minecraft, не выполняет вход в аккаунт и не создаёт отсутствующие игровые ресурсы.").weak().small());
        });
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 760.0])
            .with_min_inner_size([760.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        "NX Resource Toolkit",
        options,
        Box::new(|_creation_context| Ok(Box::new(ToolkitApp::default()))),
    )
}
