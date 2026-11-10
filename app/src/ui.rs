//! Отрисовка окна: слева плата, её Wi-Fi и этот компьютер,
//! справа телефоны, принятые фото и журнал платы.

use std::time::Duration;

use eframe::egui::{self, Color32, RichText};

use crate::app::{App, LinkState, RECEIVER_PORT, RECOGNITION_TIMEOUT, ReceiverState};

const GREEN: Color32 = Color32::from_rgb(46, 160, 67);
const YELLOW: Color32 = Color32::from_rgb(210, 153, 34);
const RED: Color32 = Color32::from_rgb(218, 54, 51);
const GRAY: Color32 = Color32::from_rgb(140, 140, 140);

/// Масштаб окна: шрифт по умолчанию мелковат.
const ZOOM: f32 = 1.15;
/// Как часто перерисовывать окно, даже если ничего не происходит
/// (чтобы обновлялись счётчики вроде «плата не отвечает»).
const IDLE_REPAINT: Duration = Duration::from_secs(1);
const LEFT_COLUMN_WIDTH: f32 = 380.0;
const DOT_SIZE: f32 = 12.0;
const LIST_HEIGHT: f32 = 150.0;

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.process_events();
        ui.ctx().set_zoom_factor(ZOOM);
        ui.ctx().request_repaint_after(IDLE_REPAINT);

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(LEFT_COLUMN_WIDTH);
                        self.board_card(ui);
                        self.wifi_card(ui);
                        self.pc_card(ui);
                    });
                    ui.vertical(|ui| {
                        self.phones_card(ui);
                        self.files_card(ui);
                        self.log_card(ui);
                    });
                });
                self.notice_line(ui);
            });
        });
    }
}

impl App {
    fn board_card(&mut self, ui: &mut egui::Ui) {
        card(ui, "Плата", |ui| {
            match &self.link {
                LinkState::Disconnected => status_line(ui, GRAY, "Не подключена"),
                LinkState::Waiting { port, since } => {
                    if since.elapsed() < RECOGNITION_TIMEOUT {
                        status_line(ui, YELLOW, &format!("{port}: ждём ответа платы…"));
                    } else {
                        status_line(ui, RED, &format!("{port}: устройство не отвечает"));
                        hint(ui, "Возможно, это не та плата или на ней другая прошивка.");
                    }
                }
                LinkState::Ready { port } => {
                    status_line(ui, GREEN, &format!("Подключена по USB ({port})"));
                }
            }

            if self.link != LinkState::Disconnected {
                if ui.button("Отключить").clicked() {
                    self.disconnect();
                }
                return;
            }

            if self.ports.is_empty() {
                hint(ui, "Порты не найдены. Подключите плату USB-кабелем.");
                return;
            }

            ui.horizontal(|ui| {
                let selected = self
                    .selected_port
                    .clone()
                    .unwrap_or_else(|| "выберите порт".into());
                egui::ComboBox::from_id_salt("port")
                    .selected_text(selected)
                    .show_ui(ui, |ui| {
                        for port in &self.ports {
                            let label = if port.description.is_empty() {
                                port.name.clone()
                            } else {
                                format!("{} — {}", port.name, port.description)
                            };
                            ui.selectable_value(
                                &mut self.selected_port,
                                Some(port.name.clone()),
                                label,
                            );
                        }
                    });

                let can_connect = self.selected_port.is_some();
                if ui
                    .add_enabled(can_connect, egui::Button::new("Подключить"))
                    .clicked()
                {
                    self.connect();
                }
            });
        });
    }

    fn wifi_card(&mut self, ui: &mut egui::Ui) {
        card(ui, "Wi-Fi платы", |ui| {
            match &self.status {
                None => status_line(ui, GRAY, "Нет данных — подключите плату"),
                Some(status) if status.wifi_connected => {
                    status_line(ui, GREEN, &format!("Подключена к «{}»", status.ssid));
                    ui.label(format!("IP-адрес платы: {}", status.ip));
                    ui.label(format!(
                        "Сигнал: {} ({} дБм)",
                        signal_quality(status.rssi),
                        status.rssi
                    ));
                }
                Some(status) if status.ssid.is_empty() => {
                    status_line(ui, YELLOW, "Сеть не задана");
                }
                Some(status) => {
                    status_line(ui, YELLOW, &format!("Подключается к «{}»…", status.ssid));
                    hint(
                        ui,
                        "Если так остаётся долго — проверьте название сети и пароль.",
                    );
                }
            }

            if let Some(link) = self.phone_link() {
                ui.add_space(4.0);
                ui.label("Откройте на телефоне:");
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&link).strong().monospace());
                    if ui.button("Копировать").clicked() {
                        ui.ctx().copy_text(link.clone());
                    }
                });
            }

            ui.separator();
            egui::Grid::new("wifi_form").num_columns(2).show(ui, |ui| {
                ui.label("Сеть:");
                ui.text_edit_singleline(&mut self.wifi_ssid);
                ui.end_row();

                ui.label("Пароль:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.wifi_password)
                        .password(!self.show_password),
                );
                ui.end_row();
            });
            ui.checkbox(&mut self.show_password, "Показать пароль");

            let board_ready = matches!(self.link, LinkState::Ready { .. });
            let save = egui::Button::new("Сохранить на плату");
            if ui.add_enabled(board_ready, save).clicked() {
                self.save_settings_to_board();
            }
            hint(
                ui,
                "Плата работает только в сетях 2,4 ГГц. Пароль хранится на плате.",
            );
        });
    }

    fn pc_card(&mut self, ui: &mut egui::Ui) {
        card(ui, "Этот компьютер", |ui| {
            match &self.net.ip {
                Some(ip) => status_line(ui, GREEN, &format!("В сети, IP-адрес: {ip}")),
                None => status_line(ui, RED, "Нет подключения к сети"),
            }
            match &self.net.wifi_name {
                Some(name) => ui.label(format!("Wi-Fi: «{name}»")),
                None => ui.label("Wi-Fi: не используется или не определён"),
            };

            match &self.receiver {
                ReceiverState::Starting => status_line(ui, YELLOW, "Приём фото запускается…"),
                ReceiverState::Listening => {
                    status_line(
                        ui,
                        GREEN,
                        &format!("Приём фото работает (порт {RECEIVER_PORT})"),
                    );
                }
                ReceiverState::Failed(reason) => {
                    status_line(ui, RED, "Приём фото не работает");
                    hint(ui, reason);
                }
            }

            if self.pc_address_mismatch() {
                ui.add_space(4.0);
                ui.colored_label(YELLOW, "Плата настроена слать фото не на этот ПК.");
                if ui.button("Передать плате адрес этого ПК").clicked() {
                    self.send_pc_address();
                }
            }

            if let (Some(pc), Some(board)) = (&self.net.wifi_name, &self.status)
                && board.wifi_connected
                && pc != &board.ssid
            {
                hint(
                    ui,
                    "ПК и плата в сетях с разными названиями — фото могут не дойти.",
                );
            }
        });
    }

    fn phones_card(&mut self, ui: &mut egui::Ui) {
        card(ui, "Телефоны", |ui| {
            if self.phones.is_empty() {
                hint(ui, "Пока никто не открывал страницу платы.");
                return;
            }
            egui::Grid::new("phones")
                .num_columns(4)
                .striped(true)
                .show(ui, |ui| {
                    for title in ["Адрес", "Был на связи", "Открытий", "Фото"]
                    {
                        ui.label(RichText::new(title).strong());
                    }
                    ui.end_row();

                    for phone in &self.phones {
                        ui.label(&phone.ip);
                        ui.label(time_ago(phone.seconds_since_seen));
                        ui.label(phone.page_views.to_string());
                        ui.label(phone.uploads.to_string());
                        ui.end_row();
                    }
                });
        });
    }

    fn files_card(&mut self, ui: &mut egui::Ui) {
        card(ui, "Принятые фото", |ui| {
            if ui.button("Открыть папку").clicked() {
                self.open_output_dir();
            }
            hint(ui, &self.output_dir.display().to_string());

            if self.files.is_empty() {
                hint(ui, "За этот запуск фото ещё не приходили.");
                return;
            }
            egui::ScrollArea::vertical()
                .id_salt("files")
                .max_height(LIST_HEIGHT)
                .show(ui, |ui| {
                    egui::Grid::new("files_grid")
                        .num_columns(3)
                        .striped(true)
                        .show(ui, |ui| {
                            for file in &self.files {
                                ui.label(&file.name)
                                    .on_hover_text(file.path.display().to_string());
                                ui.label(file_size(file.size));
                                ui.label(format!("через плату {}", file.from));
                                ui.end_row();
                            }
                        });
                });
        });
    }

    fn log_card(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Журнал платы").show(ui, |ui| {
            if self.board_log.is_empty() {
                hint(ui, "Плата пока ничего не сообщала.");
                return;
            }
            egui::ScrollArea::vertical()
                .id_salt("log")
                .max_height(LIST_HEIGHT)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &self.board_log {
                        ui.label(RichText::new(line).monospace());
                    }
                });
        });
    }

    fn notice_line(&mut self, ui: &mut egui::Ui) {
        let Some(notice) = self.notice.clone() else {
            return;
        };
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            let color = if notice.is_error { RED } else { GREEN };
            ui.colored_label(color, &notice.text);
            if ui.small_button("Скрыть").clicked() {
                self.notice = None;
            }
        });
    }
}

/// Рамка с заголовком — один раздел окна.
fn card(ui: &mut egui::Ui, title: &str, content: impl FnOnce(&mut egui::Ui)) {
    ui.group(|ui| {
        ui.set_width(ui.available_width());
        ui.heading(title);
        ui.add_space(2.0);
        content(ui);
    });
    ui.add_space(6.0);
}

/// Цветной кружок и подпись: зелёный — всё хорошо, жёлтый — ждём, красный — ошибка.
fn status_line(ui: &mut egui::Ui, color: Color32, text: &str) {
    ui.horizontal(|ui| {
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(DOT_SIZE, DOT_SIZE), egui::Sense::hover());
        ui.painter()
            .circle_filled(rect.center(), DOT_SIZE * 0.4, color);
        ui.label(text);
    });
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).weak().small());
}

/// Словесная оценка уровня сигнала Wi-Fi.
fn signal_quality(rssi: i32) -> &'static str {
    match rssi {
        -60.. => "отличный",
        -70..-60 => "хороший",
        -80..-70 => "слабый",
        _ => "очень слабый",
    }
}

fn time_ago(seconds: u64) -> String {
    match seconds {
        0..5 => "только что".to_string(),
        5..60 => format!("{seconds} с назад"),
        60..3600 => format!("{} мин назад", seconds / 60),
        _ => format!("{} ч назад", seconds / 3600),
    }
}

fn file_size(bytes: u64) -> String {
    const KILOBYTE: u64 = 1024;
    const MEGABYTE: u64 = 1024 * 1024;
    match bytes {
        0..KILOBYTE => format!("{bytes} Б"),
        KILOBYTE..MEGABYTE => format!("{:.1} КБ", bytes as f64 / KILOBYTE as f64),
        _ => format!("{:.1} МБ", bytes as f64 / MEGABYTE as f64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_quality_by_level() {
        assert_eq!(signal_quality(-45), "отличный");
        assert_eq!(signal_quality(-60), "отличный");
        assert_eq!(signal_quality(-61), "хороший");
        assert_eq!(signal_quality(-75), "слабый");
        assert_eq!(signal_quality(-90), "очень слабый");
    }

    #[test]
    fn time_ago_in_words() {
        assert_eq!(time_ago(0), "только что");
        assert_eq!(time_ago(42), "42 с назад");
        assert_eq!(time_ago(125), "2 мин назад");
        assert_eq!(time_ago(7300), "2 ч назад");
    }

    #[test]
    fn file_size_in_units() {
        assert_eq!(file_size(512), "512 Б");
        assert_eq!(file_size(1536), "1.5 КБ");
        assert_eq!(file_size(3 * 1024 * 1024), "3.0 МБ");
    }
}
