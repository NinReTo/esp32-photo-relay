//! Приёмник фото: слушает TCP-порт и сохраняет то, что присылает плата.
//!
//! Протокол тот же, что у `receiver/receiver.c`:
//! имя файла и `\n`, затем содержимое файла, затем закрытие соединения.

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use crate::filename;

/// Сколько ждать данных от молчащей платы.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Сколько байт имени читать, прежде чем считать его слишком длинным.
const MAX_NAME_LINE: u64 = 4096;
/// Пауза после ошибки приёма соединения, чтобы не крутиться вхолостую.
const ACCEPT_ERROR_PAUSE: Duration = Duration::from_millis(200);

/// Что приёмник сообщает окну.
#[derive(Debug)]
pub enum ReceiverEvent {
    /// Порт занят, приёмник ждёт подключений.
    Listening,
    /// Порт занять не удалось — приёмник не работает.
    BindFailed(String),
    /// Файл принят и сохранён.
    Saved(SavedFile),
    /// Приём файла не удался.
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct SavedFile {
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
    /// Адрес платы, приславшей файл.
    pub from: String,
}

/// Запускает приёмник в отдельном потоке. `notify` вызывается после каждого
/// события, чтобы окно перерисовалось.
pub fn spawn(
    port: u16,
    output_dir: PathBuf,
    events: Sender<ReceiverEvent>,
    notify: impl Fn() + Send + 'static,
) {
    thread::spawn(move || {
        let send = |event: ReceiverEvent| {
            // Если окно уже закрыто, сообщать некому — это не ошибка.
            let _ = events.send(event);
            notify();
        };

        let listener = match prepare(port, &output_dir) {
            Ok(listener) => listener,
            Err(error) => {
                send(ReceiverEvent::BindFailed(error));
                return;
            }
        };
        send(ReceiverEvent::Listening);

        for connection in listener.incoming() {
            match connection {
                Ok(stream) => match receive_file(stream, &output_dir) {
                    Ok(saved) => send(ReceiverEvent::Saved(saved)),
                    Err(error) => send(ReceiverEvent::Failed(error)),
                },
                Err(error) => {
                    send(ReceiverEvent::Failed(format!(
                        "Ошибка подключения: {error}"
                    )));
                    thread::sleep(ACCEPT_ERROR_PAUSE);
                }
            }
        }
    });
}

fn prepare(port: u16, output_dir: &Path) -> Result<TcpListener, String> {
    fs::create_dir_all(output_dir)
        .map_err(|error| format!("Не удалось создать папку {}: {error}", output_dir.display()))?;

    TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).map_err(|error| {
        format!("Не удалось занять порт {port}: {error}. Возможно, уже запущен другой приёмник.")
    })
}

/// Принимает один файл от подключившейся платы и сохраняет его на диск.
fn receive_file(stream: TcpStream, output_dir: &Path) -> Result<SavedFile, String> {
    let from = stream
        .peer_addr()
        .map(|address| address.ip().to_string())
        .unwrap_or_else(|_| "?".to_string());

    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .map_err(|error| format!("Ошибка сокета: {error}"))?;

    let mut reader = BufReader::new(stream);
    let raw_name = read_name_line(&mut reader)?;
    let name = filename::sanitize_name(&raw_name);

    let (mut file, path) = create_file(output_dir, &name)?;
    let copied = io::copy(&mut reader, &mut file);
    drop(file);

    let saved_name = path
        .file_name()
        .map(|os_name| os_name.to_string_lossy().into_owned())
        .unwrap_or(name);

    match copied {
        Ok(0) => {
            // Пустой файл никому не нужен; если удалить не вышло — не страшно.
            let _ = fs::remove_file(&path);
            Err(format!(
                "От {from} не пришло данных, пустой файл не сохранён."
            ))
        }
        Ok(size) => Ok(SavedFile {
            name: saved_name,
            path,
            size,
            from,
        }),
        Err(error) => Err(format!(
            "Приём «{saved_name}» оборвался ({error}), файл может быть неполным."
        )),
    }
}

/// Читает имя файла: байты до `\n`. Возвращает ошибку, если соединение
/// закрылось раньше или имя оказалось неправдоподобно длинным.
fn read_name_line(reader: &mut impl BufRead) -> Result<Vec<u8>, String> {
    let mut line = Vec::new();
    reader
        .take(MAX_NAME_LINE)
        .read_until(b'\n', &mut line)
        .map_err(|error| format!("Не удалось прочитать имя файла: {error}"))?;

    if line.pop() != Some(b'\n') {
        return Err("Соединение закрылось раньше, чем пришло имя файла.".to_string());
    }
    Ok(line)
}

/// Создаёт файл со свободным именем. Если система не приняла присланное имя
/// (например, оно слишком длинное), берёт запасное.
fn create_file(output_dir: &Path, name: &str) -> Result<(File, PathBuf), String> {
    let candidates = [name.to_string(), filename::generated_name()];

    let mut last_error = String::from("не удалось подобрать свободное имя");
    for candidate in &candidates {
        let Some(path) = filename::unique_path(output_dir, candidate) else {
            continue;
        };
        match File::create_new(&path) {
            Ok(file) => return Ok((file, path)),
            Err(error) => last_error = error.to_string(),
        }
    }
    Err(format!("Не удалось создать файл «{name}»: {last_error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::mpsc;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("photo-relay-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    /// Изображает плату: подключается, шлёт заголовок и данные, закрывает соединение.
    fn send(port: u16, header: &[u8], body: &[u8]) {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        stream.write_all(header).unwrap();
        stream.write_all(body).unwrap();
    }

    #[test]
    fn saves_files_and_rejects_bad_streams() {
        let dir = temp_dir("receiver");
        let port = 45_871;
        let (sender, events) = mpsc::channel();
        spawn(port, dir.clone(), sender, || {});

        let wait = || events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(wait(), ReceiverEvent::Listening));

        let body: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        send(port, b"IMG_1.jpg\n", &body);
        let ReceiverEvent::Saved(saved) = wait() else {
            panic!("файл не сохранён")
        };
        assert_eq!(saved.name, "IMG_1.jpg");
        assert_eq!(saved.size, body.len() as u64);
        assert_eq!(fs::read(&saved.path).unwrap(), body);

        send(port, b"IMG_1.jpg\n", b"second");
        let ReceiverEvent::Saved(saved) = wait() else {
            panic!("файл не сохранён")
        };
        assert_eq!(saved.name, "IMG_1_1.jpg");

        send(port, b"../../escape.jpg\n", b"data");
        let ReceiverEvent::Saved(saved) = wait() else {
            panic!("файл не сохранён")
        };
        assert_eq!(saved.path, dir.join("escape.jpg"));

        send(port, b"no newline", b"");
        assert!(matches!(wait(), ReceiverEvent::Failed(_)));

        send(port, b"empty.jpg\n", b"");
        assert!(matches!(wait(), ReceiverEvent::Failed(_)));
        assert!(!dir.join("empty.jpg").exists());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reports_busy_port() {
        let busy = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        let port = busy.local_addr().unwrap().port();
        let (sender, events) = mpsc::channel();
        spawn(port, temp_dir("busy"), sender, || {});

        let event = events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(event, ReceiverEvent::BindFailed(_)));
    }
}
