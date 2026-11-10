//! Сведения о сети этого компьютера: его IP-адрес и название сети Wi-Fi.

use std::net::{Ipv4Addr, UdpSocket};
use std::process::Command;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

/// Как часто перепроверять сеть (ПК могли переключить на другую).
const REFRESH_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Default)]
pub struct NetInfo {
    /// IP-адрес ПК в локальной сети; `None`, если сети нет.
    pub ip: Option<String>,
    /// Название сети Wi-Fi; `None`, если ПК подключён кабелем или узнать не удалось.
    pub wifi_name: Option<String>,
}

/// Запускает поток, который периодически сообщает окну сведения о сети.
/// Поток завершается, когда окно закрыто.
pub fn spawn(events: Sender<NetInfo>, notify: impl Fn() + Send + 'static) {
    thread::spawn(move || {
        let mut previous = None;
        loop {
            let current = NetInfo {
                ip: local_ip(),
                wifi_name: wifi_name(),
            };
            if previous.as_ref() != Some(&current) {
                if events.send(current.clone()).is_err() {
                    return;
                }
                notify();
                previous = Some(current);
            }
            thread::sleep(REFRESH_INTERVAL);
        }
    });
}

/// Узнаёт, с какого адреса ПК выходит в сеть. Для UDP `connect` ничего
/// не отправляет — система лишь выбирает подходящий сетевой адаптер.
fn local_ip() -> Option<String> {
    // Адрес из диапазона, отведённого для примеров в документации: он точно не в локальной сети.
    const OUTSIDE_ADDRESS: (Ipv4Addr, u16) = (Ipv4Addr::new(192, 0, 2, 1), 9);

    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect(OUTSIDE_ADDRESS).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then(|| ip.to_string())
}

#[cfg(windows)]
fn wifi_name() -> Option<String> {
    use std::os::windows::process::CommandExt;
    /// Не показывать чёрное окно консоли при запуске служебной программы.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let output = Command::new("netsh")
        .args(["wlan", "show", "interfaces"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    parse_netsh(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(not(windows))]
fn wifi_name() -> Option<String> {
    let run = |program: &str, arguments: &[&str]| {
        let output = Command::new(program)
            .args(arguments)
            .env("LC_ALL", "C")
            .output()
            .ok()?;
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    };

    let from_iwgetid = run("iwgetid", &["-r"])
        .map(|text| text.trim().to_string())
        .filter(|name| !name.is_empty());

    from_iwgetid
        .or_else(|| parse_nmcli(&run("nmcli", &["-t", "-f", "active,ssid", "dev", "wifi"])?))
}

/// Достаёт название сети из вывода `netsh wlan show interfaces`:
/// строка вида `    SSID                   : Имя сети` (но не `BSSID`).
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_netsh(output: &str) -> Option<String> {
    output
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(key, _)| key.trim() == "SSID")
        .map(|(_, value)| value.trim().to_string())
        // netsh пишет в кодировке консоли, и название не латиницей приходит испорченным.
        // Лучше не показать ничего, чем подставить в форму мусор.
        .filter(|name| !name.is_empty() && !name.contains(char::REPLACEMENT_CHARACTER))
}

/// Достаёт название сети из вывода `nmcli -t -f active,ssid dev wifi`:
/// строка активной сети начинается с `yes:`.
#[cfg_attr(windows, allow(dead_code))]
fn parse_nmcli(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|line| line.strip_prefix("yes:"))
        // Двоеточие внутри названия nmcli записывает как `\:`.
        .map(|name| name.trim().replace("\\:", ":"))
        .filter(|name| !name.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_ssid_but_not_bssid_in_netsh_output() {
        let output = "\
    Name                   : Wi-Fi\r\n\
    State                  : connected\r\n\
    SSID                   : Home Net: 5G\r\n\
    BSSID                  : aa:bb:cc:dd:ee:ff\r\n";
        // Всё после первого двоеточия — название, даже если в нём есть своё двоеточие.
        assert_eq!(parse_netsh(output), Some("Home Net: 5G".to_string()));
    }

    #[test]
    fn netsh_without_wifi_gives_nothing() {
        assert_eq!(
            parse_netsh("There is no wireless interface on the system."),
            None
        );
        assert_eq!(parse_netsh("    BSSID : aa:bb:cc:dd:ee:ff"), None);
        assert_eq!(parse_netsh("    SSID  : "), None);
    }

    #[test]
    fn garbled_netsh_name_is_dropped() {
        assert_eq!(parse_netsh("    SSID  : \u{FFFD}\u{FFFD}\u{FFFD} 5G"), None);
    }

    #[test]
    fn nmcli_escaped_colon_is_restored() {
        assert_eq!(
            parse_nmcli("yes:Home\\: 5G\n"),
            Some("Home: 5G".to_string())
        );
    }

    #[test]
    fn finds_active_network_in_nmcli_output() {
        assert_eq!(
            parse_nmcli("no:Neighbour\nyes:Home\nno:Cafe\n"),
            Some("Home".to_string())
        );
        assert_eq!(parse_nmcli("no:Neighbour\n"), None);
        assert_eq!(parse_nmcli(""), None);
    }
}
