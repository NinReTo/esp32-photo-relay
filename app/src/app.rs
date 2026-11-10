//! Состояние приложения и обработка событий от фоновых потоков.
//! Отрисовка окна — в модуле `ui`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui;

use crate::board::{self, BoardCommand, BoardEvent, PortChoice};
use crate::netinfo::{self, NetInfo};
use crate::protocol::{self, BoardStatus, Phone};
use crate::receiver::{self, ReceiverEvent, SavedFile};

/// Порт, на котором приложение принимает фото от платы.
pub const RECEIVER_PORT: u16 = 5050;
/// Папка для принятых фото (рядом с программой).
const OUTPUT_DIR_NAME: &str = "received";
/// Сколько строк журнала платы хранить.
const MAX_LOG_LINES: usize = 200;
/// Сколько принятых файлов показывать в списке.
const MAX_FILES_SHOWN: usize = 100;
/// Сколько ждать ответа платы, прежде чем заподозрить чужую прошивку.
pub const RECOGNITION_TIMEOUT: Duration = Duration::from_secs(6);

/// Соединение с платой по USB.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkState {
    Disconnected,
    /// Порт открыт, плата пока не ответила.
    Waiting {
        port: String,
        since: Instant,
    },
    /// Плата ответила, прошивка наша.
    Ready {
        port: String,
    },
}

/// Работает ли приём фото на этом ПК.
#[derive(Debug, Clone, PartialEq)]
pub enum ReceiverState {
    Starting,
    Listening,
    Failed(String),
}

/// Сообщение для пользователя внизу окна.
#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub text: String,
    pub is_error: bool,
}

pub struct App {
    board_commands: Sender<BoardCommand>,
    board_events: Receiver<BoardEvent>,
    receiver_events: Receiver<ReceiverEvent>,
    net_events: Receiver<NetInfo>,

    pub ports: Vec<PortChoice>,
    pub selected_port: Option<String>,
    pub link: LinkState,
    pub status: Option<BoardStatus>,
    pub phones: Vec<Phone>,
    pub board_log: VecDeque<String>,

    /// Поля формы «Wi-Fi для платы».
    pub wifi_ssid: String,
    pub wifi_password: String,
    pub show_password: bool,

    pub net: NetInfo,
    pub receiver: ReceiverState,
    pub files: VecDeque<SavedFile>,
    pub output_dir: PathBuf,
    pub notice: Option<Notice>,
}

impl App {
    pub fn new(ctx: &egui::Context) -> Self {
        let (board_commands, board_command_queue) = mpsc::channel();
        let (board_sender, board_events) = mpsc::channel();
        let (receiver_sender, receiver_events) = mpsc::channel();
        let (net_sender, net_events) = mpsc::channel();

        let output_dir = output_dir();
        let repaint = |ctx: &egui::Context| {
            let ctx = ctx.clone();
            move || ctx.request_repaint()
        };

        board::spawn(board_command_queue, board_sender, repaint(ctx));
        receiver::spawn(
            RECEIVER_PORT,
            output_dir.clone(),
            receiver_sender,
            repaint(ctx),
        );
        netinfo::spawn(net_sender, repaint(ctx));

        Self {
            board_commands,
            board_events,
            receiver_events,
            net_events,
            ports: Vec::new(),
            selected_port: None,
            link: LinkState::Disconnected,
            status: None,
            phones: Vec::new(),
            board_log: VecDeque::new(),
            wifi_ssid: String::new(),
            wifi_password: String::new(),
            show_password: false,
            net: NetInfo::default(),
            receiver: ReceiverState::Starting,
            files: VecDeque::new(),
            output_dir,
            notice: None,
        }
    }

    /// Забирает всё, что накопили фоновые потоки, и обновляет состояние.
    pub fn process_events(&mut self) {
        while let Ok(event) = self.board_events.try_recv() {
            self.on_board_event(event);
        }
        while let Ok(event) = self.receiver_events.try_recv() {
            self.on_receiver_event(event);
        }
        while let Ok(net) = self.net_events.try_recv() {
            // Удобно сразу предложить плате ту же сеть, к которой подключён ПК.
            if self.wifi_ssid.is_empty() {
                self.wifi_ssid = net.wifi_name.clone().unwrap_or_default();
            }
            self.net = net;
        }
    }

    fn on_board_event(&mut self, event: BoardEvent) {
        match event {
            BoardEvent::Ports(ports) => self.set_ports(ports),
            BoardEvent::Opened(port) => {
                self.link = LinkState::Waiting {
                    port,
                    since: Instant::now(),
                };
                self.notice = None;
            }
            BoardEvent::Recognized => {
                if let LinkState::Waiting { port, .. } = &self.link {
                    self.link = LinkState::Ready { port: port.clone() };
                }
            }
            BoardEvent::Closed(reason) => {
                self.link = LinkState::Disconnected;
                self.status = None;
                self.phones.clear();
                if !reason.is_empty() {
                    self.set_error(reason);
                }
            }
            BoardEvent::Status(status, phones) => {
                if self.wifi_ssid.is_empty() {
                    self.wifi_ssid = status.ssid.clone();
                }
                self.status = Some(status);
                self.phones = phones;
            }
            BoardEvent::Done(what) => {
                let text = match what.as_str() {
                    "WIFI" => "Сеть Wi-Fi сохранена на плате, идёт подключение…",
                    "PC" => "Адрес этого ПК сохранён на плате.",
                    _ => "Плата выполнила команду.",
                };
                self.set_info(text);
            }
            BoardEvent::Refused(reason) => self.set_error(format!("Плата ответила: {reason}")),
            BoardEvent::Log(line) => {
                self.board_log.push_back(line);
                while self.board_log.len() > MAX_LOG_LINES {
                    self.board_log.pop_front();
                }
            }
        }
    }

    fn on_receiver_event(&mut self, event: ReceiverEvent) {
        match event {
            ReceiverEvent::Listening => self.receiver = ReceiverState::Listening,
            ReceiverEvent::BindFailed(reason) => self.receiver = ReceiverState::Failed(reason),
            ReceiverEvent::Saved(file) => {
                self.set_info(format!("Принято фото «{}»", file.name));
                self.files.push_front(file);
                self.files.truncate(MAX_FILES_SHOWN);
            }
            ReceiverEvent::Failed(reason) => self.set_error(reason),
        }
    }

    fn set_ports(&mut self, ports: Vec<PortChoice>) {
        let still_present = |name: &String| ports.iter().any(|port| &port.name == name);
        if !self.selected_port.as_ref().is_some_and(still_present) {
            // Если порт один, выбирать не из чего — подставляем его сами.
            self.selected_port = match ports.as_slice() {
                [only] => Some(only.name.clone()),
                _ => None,
            };
        }
        self.ports = ports;
    }

    pub fn connect(&mut self) {
        if let Some(port) = self.selected_port.clone() {
            self.send_command(BoardCommand::Connect(port));
        }
    }

    pub fn disconnect(&mut self) {
        self.send_command(BoardCommand::Disconnect);
    }

    /// Отправляет плате сеть Wi-Fi из формы и адрес этого ПК.
    pub fn save_settings_to_board(&mut self) {
        let ssid = self.wifi_ssid.trim().to_string();
        let password = self.wifi_password.clone();

        if ssid.is_empty() {
            self.set_error("Укажите название сети Wi-Fi.");
            return;
        }
        if !protocol::is_safe_field(&ssid) || !protocol::is_safe_field(&password) {
            self.set_error("В названии сети и пароле не должно быть табуляции и переводов строки.");
            return;
        }

        self.send_command(BoardCommand::SetWifi { ssid, password });
        self.send_pc_address();
    }

    /// Сообщает плате, на какой адрес слать фото (адрес этого ПК).
    pub fn send_pc_address(&mut self) {
        match self.net.ip.clone() {
            Some(host) => self.send_command(BoardCommand::SetPc {
                host,
                port: RECEIVER_PORT,
            }),
            None => self.set_error("Этот ПК не подключён к сети — адрес для платы неизвестен."),
        }
    }

    /// Плата настроена слать фото не на этот ПК (или адрес ещё не задан).
    pub fn pc_address_mismatch(&self) -> bool {
        match (&self.status, &self.net.ip) {
            (Some(status), Some(ip)) => &status.pc_host != ip || status.pc_port != RECEIVER_PORT,
            _ => false,
        }
    }

    /// Ссылка, которую нужно открыть на телефоне.
    pub fn phone_link(&self) -> Option<String> {
        self.status
            .as_ref()
            .filter(|status| status.wifi_connected && !status.ip.is_empty())
            .map(|status| format!("http://{}/", status.ip))
    }

    pub fn open_output_dir(&mut self) {
        if let Err(error) = std::fs::create_dir_all(&self.output_dir) {
            self.set_error(format!("Не удалось создать папку: {error}"));
            return;
        }
        let program = if cfg!(windows) {
            "explorer"
        } else {
            "xdg-open"
        };
        if let Err(error) = std::process::Command::new(program)
            .arg(&self.output_dir)
            .spawn()
        {
            self.set_error(format!("Не удалось открыть папку: {error}"));
        }
    }

    fn send_command(&mut self, command: BoardCommand) {
        if self.board_commands.send(command).is_err() {
            self.set_error("Поток связи с платой остановился. Перезапустите приложение.");
        }
    }

    fn set_info(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            is_error: false,
        });
    }

    fn set_error(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            is_error: true,
        });
    }
}

/// Папка `received` рядом с программой; если путь к программе узнать
/// не удалось — в текущей папке.
fn output_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(OUTPUT_DIR_NAME)))
        .unwrap_or_else(|| PathBuf::from(OUTPUT_DIR_NAME))
}
