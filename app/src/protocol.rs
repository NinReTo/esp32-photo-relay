//! Разбор строк, которые плата присылает по USB.
//!
//! Ответы на команды начинаются с `@`, поля разделены табуляцией.
//! Всё остальное — обычный журнал платы.

/// Имя, которым прошивка отвечает на `PING`.
pub const FIRMWARE_NAME: &str = "esp32-photo-relay";

/// Состояние платы из ответа на команду `STATUS`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BoardStatus {
    pub wifi_connected: bool,
    /// Сеть Wi-Fi, заданная на плате (может быть пустой).
    pub ssid: String,
    /// IP-адрес платы; пустой, пока нет подключения к Wi-Fi.
    pub ip: String,
    /// Уровень сигнала, дБм.
    pub rssi: i32,
    /// Адрес ПК, на который плата шлёт фото.
    pub pc_host: String,
    pub pc_port: u16,
    pub uptime_seconds: u64,
}

/// Телефон, который открывал страницу платы или отправлял фото.
#[derive(Debug, Clone, PartialEq)]
pub struct Phone {
    pub ip: String,
    pub seconds_since_seen: u64,
    pub page_views: u32,
    pub uploads: u32,
}

/// Одна строка от платы.
#[derive(Debug, Clone, PartialEq)]
pub enum BoardLine {
    /// Ответ на `PING`: плата с нашей прошивкой.
    Pong {
        firmware: String,
    },
    Status(BoardStatus),
    Phone(Phone),
    /// Конец ответа на `STATUS`.
    End,
    /// Команда выполнена (в поле — её название).
    Done(String),
    /// Плата отказалась выполнять команду.
    Refused(String),
    /// Обычная строка журнала.
    Log(String),
}

/// Разбирает строку от платы. Строки с `@`, которые разобрать не удалось,
/// попадают в журнал как есть — приложение из-за них не падает.
pub fn parse_line(line: &str) -> BoardLine {
    let line = line.trim_end_matches(['\r', '\n']);
    let fields: Vec<&str> = line.split('\t').collect();

    let parsed = match fields.as_slice() {
        ["@PONG", firmware, ..] => Some(BoardLine::Pong {
            firmware: firmware.to_string(),
        }),
        ["@STATUS", rest @ ..] => parse_status(rest).map(BoardLine::Status),
        ["@CLIENT", rest @ ..] => parse_phone(rest).map(BoardLine::Phone),
        ["@END"] => Some(BoardLine::End),
        ["@OK", what, ..] => Some(BoardLine::Done(what.to_string())),
        ["@ERR", reason, ..] => Some(BoardLine::Refused(reason.to_string())),
        _ => None,
    };

    parsed.unwrap_or_else(|| BoardLine::Log(line.to_string()))
}

fn parse_status(fields: &[&str]) -> Option<BoardStatus> {
    let [connected, ssid, ip, rssi, pc_host, pc_port, uptime] = fields else {
        return None;
    };
    Some(BoardStatus {
        wifi_connected: *connected == "1",
        ssid: ssid.to_string(),
        ip: ip.to_string(),
        rssi: rssi.parse().ok()?,
        pc_host: pc_host.to_string(),
        pc_port: pc_port.parse().ok()?,
        uptime_seconds: uptime.parse().ok()?,
    })
}

fn parse_phone(fields: &[&str]) -> Option<Phone> {
    let [ip, seconds, page_views, uploads] = fields else {
        return None;
    };
    Some(Phone {
        ip: ip.to_string(),
        seconds_since_seen: seconds.parse().ok()?,
        page_views: page_views.parse().ok()?,
        uploads: uploads.parse().ok()?,
    })
}

/// Проверяет, что значение можно передать полем команды:
/// табуляция и перевод строки сломали бы разбор на плате.
pub fn is_safe_field(value: &str) -> bool {
    !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_of_connected_board() {
        let line = "@STATUS\t1\tMy Home WiFi\t192.168.1.57\t-61\t192.168.1.10\t5050\t125\r\n";
        assert_eq!(
            parse_line(line),
            BoardLine::Status(BoardStatus {
                wifi_connected: true,
                ssid: "My Home WiFi".into(),
                ip: "192.168.1.57".into(),
                rssi: -61,
                pc_host: "192.168.1.10".into(),
                pc_port: 5050,
                uptime_seconds: 125,
            })
        );
    }

    #[test]
    fn parses_status_of_unconfigured_board() {
        let BoardLine::Status(status) = parse_line("@STATUS\t0\t\t\t0\t\t5050\t3") else {
            panic!("ожидалось состояние");
        };
        assert!(!status.wifi_connected);
        assert!(status.ssid.is_empty() && status.ip.is_empty() && status.pc_host.is_empty());
    }

    #[test]
    fn parses_phone() {
        assert_eq!(
            parse_line("@CLIENT\t192.168.1.23\t14\t3\t2"),
            BoardLine::Phone(Phone {
                ip: "192.168.1.23".into(),
                seconds_since_seen: 14,
                page_views: 3,
                uploads: 2,
            })
        );
    }

    #[test]
    fn parses_short_replies() {
        assert_eq!(
            parse_line("@PONG\tesp32-photo-relay\t1"),
            BoardLine::Pong {
                firmware: "esp32-photo-relay".into()
            }
        );
        assert_eq!(parse_line("@END"), BoardLine::End);
        assert_eq!(parse_line("@OK\tWIFI"), BoardLine::Done("WIFI".into()));
        assert_eq!(
            parse_line("@ERR\tнет сети"),
            BoardLine::Refused("нет сети".into())
        );
    }

    #[test]
    fn everything_else_is_log() {
        assert_eq!(
            parse_line("Wi-Fi подключён.\r\n"),
            BoardLine::Log("Wi-Fi подключён.".into())
        );
        // Испорченные ответы не роняют приложение, а уходят в журнал.
        assert!(matches!(parse_line("@STATUS\t1\tsсеть"), BoardLine::Log(_)));
        assert!(matches!(
            parse_line("@STATUS\t1\ta\tb\tшум\tc\t5050\t1"),
            BoardLine::Log(_)
        ));
        assert!(matches!(
            parse_line("@CLIENT\t1.2.3.4\t-5\t1\t1"),
            BoardLine::Log(_)
        ));
        assert!(matches!(parse_line(""), BoardLine::Log(_)));
    }

    #[test]
    fn control_characters_are_not_safe_in_fields() {
        assert!(is_safe_field("Моя сеть 5G"));
        assert!(!is_safe_field("a\tb"));
        assert!(!is_safe_field("a\nPC\t1.1.1.1\t1"));
    }
}
