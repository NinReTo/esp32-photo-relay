//! Связь с платой по USB (последовательный порт, 115200 бод).
//!
//! Работает в отдельном потоке: ищет порты, держит соединение, раз в пару
//! секунд спрашивает у платы состояние и передаёт ей настройки из окна.

use std::io::{ErrorKind, Read, Write};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use serialport::{SerialPort, SerialPortType};

use crate::protocol::{self, BoardLine, BoardStatus, Phone};

const BAUD_RATE: u32 = 115_200;
/// Сколько ждать байты от платы за один проход (заодно это пауза рабочего цикла).
const READ_TIMEOUT: Duration = Duration::from_millis(100);
/// Как часто спрашивать у платы состояние.
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Как часто обновлять список портов, пока плата не подключена.
const PORT_SCAN_INTERVAL: Duration = Duration::from_secs(2);
/// Пауза рабочего цикла, пока плата не подключена.
const IDLE_PAUSE: Duration = Duration::from_millis(150);
/// Строка длиннее этого без перевода строки — мусор, а не ответ платы.
const MAX_LINE_BYTES: usize = 4096;

/// Что окно просит сделать.
#[derive(Debug)]
pub enum BoardCommand {
    Connect(String),
    Disconnect,
    SetWifi { ssid: String, password: String },
    SetPc { host: String, port: u16 },
}

/// Что поток связи сообщает окну.
#[derive(Debug)]
pub enum BoardEvent {
    /// Свежий список последовательных портов.
    Ports(Vec<PortChoice>),
    /// Порт открыт (но плата ещё не ответила).
    Opened(String),
    /// Плата ответила и это наша прошивка.
    Recognized,
    /// Порт закрыт. Причина пустая, если его закрыл сам пользователь.
    Closed(String),
    /// Состояние платы и список телефонов.
    Status(BoardStatus, Vec<Phone>),
    /// Плата выполнила команду.
    Done(String),
    /// Плата отказалась выполнять команду.
    Refused(String),
    /// Строка журнала платы.
    Log(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PortChoice {
    pub name: String,
    /// Описание устройства, если система его знает (например, «CP2102 USB to UART»).
    pub description: String,
}

/// Запускает поток связи с платой. Поток завершается, когда окно закрыто
/// (канал команд разорван).
pub fn spawn(
    commands: Receiver<BoardCommand>,
    events: Sender<BoardEvent>,
    notify: impl Fn() + Send + 'static,
) {
    thread::spawn(move || {
        let send = |event: BoardEvent| {
            // Если окно уже закрыто, сообщать некому — это не ошибка.
            let _ = events.send(event);
            notify();
        };
        Worker {
            link: None,
            last_port_scan: None,
        }
        .run(&commands, &send);
    });
}

struct Worker {
    link: Option<Link>,
    last_port_scan: Option<Instant>,
}

impl Worker {
    fn run(mut self, commands: &Receiver<BoardCommand>, send: &dyn Fn(BoardEvent)) {
        loop {
            loop {
                match commands.try_recv() {
                    Ok(command) => self.handle_command(command, send),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }

            match &mut self.link {
                Some(link) => {
                    if let Err(reason) = link.poll(send) {
                        self.link = None;
                        send(BoardEvent::Closed(reason));
                    }
                }
                None => {
                    self.scan_ports(send);
                    thread::sleep(IDLE_PAUSE);
                }
            }
        }
    }

    fn handle_command(&mut self, command: BoardCommand, send: &dyn Fn(BoardEvent)) {
        match command {
            BoardCommand::Connect(port_name) => {
                self.link = None;
                match Link::open(&port_name) {
                    Ok(link) => {
                        self.link = Some(link);
                        send(BoardEvent::Opened(port_name));
                    }
                    Err(reason) => send(BoardEvent::Closed(reason)),
                }
            }
            BoardCommand::Disconnect => {
                if self.link.take().is_some() {
                    send(BoardEvent::Closed(String::new()));
                }
            }
            BoardCommand::SetWifi { ssid, password } => {
                self.write_command(&format!("WIFI\t{ssid}\t{password}\n"), send);
            }
            BoardCommand::SetPc { host, port } => {
                self.write_command(&format!("PC\t{host}\t{port}\n"), send);
            }
        }
    }

    fn write_command(&mut self, text: &str, send: &dyn Fn(BoardEvent)) {
        let Some(link) = &mut self.link else {
            send(BoardEvent::Refused("плата не подключена".to_string()));
            return;
        };
        if let Err(reason) = link.write(text) {
            self.link = None;
            send(BoardEvent::Closed(reason));
        }
    }

    fn scan_ports(&mut self, send: &dyn Fn(BoardEvent)) {
        let due = self
            .last_port_scan
            .is_none_or(|last| last.elapsed() >= PORT_SCAN_INTERVAL);
        if !due {
            return;
        }
        self.last_port_scan = Some(Instant::now());
        send(BoardEvent::Ports(list_ports()));
    }
}

/// Список последовательных портов. Ошибку перечисления считаем пустым списком:
/// окно покажет «порты не найдены», и через пару секунд будет новая попытка.
fn list_ports() -> Vec<PortChoice> {
    let ports = serialport::available_ports().unwrap_or_default();
    ports
        .into_iter()
        .map(|port| {
            let description = match port.port_type {
                SerialPortType::UsbPort(usb) => usb
                    .product
                    .or(usb.manufacturer)
                    .unwrap_or_else(|| "USB".to_string()),
                SerialPortType::BluetoothPort => "Bluetooth".to_string(),
                SerialPortType::PciPort | SerialPortType::Unknown => String::new(),
            };
            PortChoice {
                name: port.port_name,
                description,
            }
        })
        .collect()
}

/// Открытое соединение с платой.
struct Link {
    port: Box<dyn SerialPort>,
    /// Байты, из которых ещё не собралась целая строка.
    buffer: Vec<u8>,
    last_poll: Option<Instant>,
    recognized: bool,
    /// Ответ на `STATUS` приходит несколькими строками — копим его до `@END`.
    pending_status: Option<BoardStatus>,
    pending_phones: Vec<Phone>,
}

impl Link {
    fn open(port_name: &str) -> Result<Self, String> {
        let mut port = serialport::new(port_name, BAUD_RATE)
            .timeout(READ_TIMEOUT)
            // Линии DTR и RTS на платах ESP32 заведены на сброс —
            // держим их отпущенными, чтобы плата не перезагружалась при подключении.
            .dtr_on_open(false)
            .open()
            .map_err(|error| format!("Не удалось открыть порт {port_name}: {error}"))?;

        // Не на всех переходниках это удаётся; тогда плата просто перезагрузится
        // и через секунду ответит — работе это не мешает.
        let _ = port.write_request_to_send(false);

        Ok(Self {
            port,
            buffer: Vec::new(),
            last_poll: None,
            recognized: false,
            pending_status: None,
            pending_phones: Vec::new(),
        })
    }

    fn write(&mut self, text: &str) -> Result<(), String> {
        self.port
            .write_all(text.as_bytes())
            .and_then(|()| self.port.flush())
            .map_err(|error| format!("Связь с платой потеряна: {error}"))
    }

    /// Один проход: при необходимости спросить состояние, затем прочитать то, что пришло.
    fn poll(&mut self, send: &dyn Fn(BoardEvent)) -> Result<(), String> {
        if self
            .last_poll
            .is_none_or(|last| last.elapsed() >= POLL_INTERVAL)
        {
            self.last_poll = Some(Instant::now());
            // Пока плата не опознана, спрашиваем ещё и имя прошивки.
            let request = if self.recognized {
                "STATUS\n"
            } else {
                "PING\nSTATUS\n"
            };
            self.write(request)?;
        }

        let mut chunk = [0u8; 512];
        match self.port.read(&mut chunk) {
            Ok(0) => {}
            Ok(count) => {
                self.buffer.extend_from_slice(&chunk[..count]);
                self.take_lines(send);
            }
            Err(error) if error.kind() == ErrorKind::TimedOut => {}
            Err(error) => return Err(format!("Связь с платой потеряна: {error}")),
        }
        Ok(())
    }

    /// Вынимает из буфера все целые строки и разбирает их.
    fn take_lines(&mut self, send: &dyn Fn(BoardEvent)) {
        while let Some(end) = self.buffer.iter().position(|&byte| byte == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=end).collect();
            let text = String::from_utf8_lossy(&line);
            self.handle_line(protocol::parse_line(&text), send);
        }
        if self.buffer.len() > MAX_LINE_BYTES {
            self.buffer.clear();
        }
    }

    fn handle_line(&mut self, line: BoardLine, send: &dyn Fn(BoardEvent)) {
        match line {
            BoardLine::Pong { firmware } => {
                if firmware == protocol::FIRMWARE_NAME && !self.recognized {
                    self.recognized = true;
                    send(BoardEvent::Recognized);
                }
            }
            BoardLine::Status(status) => {
                self.pending_status = Some(status);
                self.pending_phones.clear();
            }
            BoardLine::Phone(phone) => self.pending_phones.push(phone),
            BoardLine::End => {
                if let Some(status) = self.pending_status.take() {
                    send(BoardEvent::Status(
                        status,
                        std::mem::take(&mut self.pending_phones),
                    ));
                }
            }
            BoardLine::Done(what) => send(BoardEvent::Done(what)),
            BoardLine::Refused(reason) => send(BoardEvent::Refused(reason)),
            BoardLine::Log(text) => {
                if !text.trim().is_empty() {
                    send(BoardEvent::Log(text));
                }
            }
        }
    }
}
