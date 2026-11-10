/*
  esp32-photo-relay — прошивка ESP32

  Плата подключается к домашнему Wi-Fi, поднимает веб-сервер со страницей
  загрузки фото и пересылает принятые байты на ПК по TCP.
  Файл нигде не сохраняется: плата работает как «труба» между
  HTTP-загрузкой с телефона и TCP-приёмником на ПК.

  Протокол обмена с приёмником:
    1. имя файла и символ перевода строки '\n';
    2. содержимое файла как есть;
    3. закрытие соединения — признак конца файла.
*/

#include <WiFi.h>
#include <WebServer.h>

// ===================== НАСТРОЙКИ — поменяйте перед прошивкой =====================

const char* ssid     = "ИМЯ_ВАШЕЙ_СЕТИ";   // название сети Wi-Fi
const char* password = "ПАРОЛЬ_ОТ_СЕТИ";   // пароль сети Wi-Fi

const char*    pcHost = "192.168.1.100";   // IP-адрес ПК, на котором запущен приёмник
const uint16_t pcPort = 5050;              // порт приёмника (тот же, что у receiver)

// ==================================================================================

const uint16_t httpPort         = 80;     // порт веб-сервера на плате
const int32_t  pcConnectTimeout = 5000;   // сколько ждать подключения к ПК, мс

WebServer  server(httpPort);
WiFiClient pcClient;                      // TCP-соединение с приёмником на ПК

// Состояние текущей загрузки
bool   uploadStarted = false;             // пришёл ли вообще файл в запросе
bool   uploadFailed  = false;             // была ли ошибка при пересылке
String uploadError   = "";                // текст ошибки для показа в браузере
size_t bytesSent     = 0;                 // сколько байт переслано на ПК

// Страница загрузки. Хранится во флеш-памяти программы, оперативную память не занимает.
const char uploadPage[] PROGMEM = R"rawliteral(<!DOCTYPE html>
<html lang="ru">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Фото на ПК</title>
<style>
  * { box-sizing: border-box; }
  body {
    margin: 0; padding: 24px 16px;
    font-family: -apple-system, "Segoe UI", Roboto, sans-serif;
    background: #f2f4f7; color: #1c2430;
  }
  main {
    max-width: 420px; margin: 0 auto; padding: 24px 20px;
    background: #fff; border-radius: 16px;
    box-shadow: 0 2px 12px rgba(0, 0, 0, .08);
  }
  h1 { margin: 0 0 8px; font-size: 22px; }
  p  { margin: 0 0 20px; font-size: 15px; color: #5b6676; }
  input[type=file] {
    display: block; width: 100%; margin-bottom: 12px; padding: 14px 12px;
    font-size: 16px; background: #f7f9fb;
    border: 1px dashed #b8c2cf; border-radius: 12px;
  }
  label.gallery {
    display: flex; align-items: center; gap: 8px;
    margin-bottom: 20px; font-size: 15px; color: #5b6676;
  }
  label.gallery input { width: 20px; height: 20px; }
  button {
    width: 100%; padding: 16px; font-size: 17px; font-weight: 600;
    color: #fff; background: #1f6feb; border: 0; border-radius: 12px;
  }
  button:active { background: #1558c0; }
</style>
</head>
<body>
<main>
  <h1>Отправить фото на ПК</h1>
  <p>Сделайте снимок или выберите фото — оно сразу уйдёт на компьютер.</p>
  <form method="POST" action="/upload" enctype="multipart/form-data">
    <input id="photo" type="file" name="photo" accept="image/*" capture="environment" required>
    <label class="gallery">
      <input id="gallery" type="checkbox"> Выбрать из галереи, а не снимать
    </label>
    <button type="submit">Отправить</button>
  </form>
</main>
<script>
  // С признаком capture телефон сразу открывает камеру.
  // Галочка убирает его, и тогда можно выбрать готовое фото из галереи.
  var photo = document.getElementById('photo');
  document.getElementById('gallery').addEventListener('change', function () {
    if (this.checked) { photo.removeAttribute('capture'); }
    else { photo.setAttribute('capture', 'environment'); }
  });
</script>
</body>
</html>
)rawliteral";

// Запоминает первую ошибку загрузки и закрывает соединение с ПК.
void failUpload(const String& message) {
  if (!uploadFailed) {
    uploadFailed = true;
    uploadError  = message;
    Serial.println("Ошибка: " + message);
  }
  pcClient.stop();
}

// GET / — отдать страницу загрузки.
void handleRoot() {
  server.send_P(200, "text/html; charset=utf-8", uploadPage);
}

// Вызывается много раз во время POST /upload — на начало файла, на каждый кусок и на конец.
void handleUploadChunk() {
  HTTPUpload& upload = server.upload();

  if (upload.status == UPLOAD_FILE_START) {
    // Новый файл: сбрасываем состояние прошлой загрузки.
    pcClient.stop();
    uploadStarted = true;
    uploadFailed  = false;
    uploadError   = "";
    bytesSent     = 0;

    // Перевод строки отделяет имя от данных, поэтому внутри имени его быть не должно.
    String filename = upload.filename;
    filename.replace("\r", "");
    filename.replace("\n", "");

    Serial.printf("Начало загрузки: %s\n", filename.c_str());

    if (!pcClient.connect(pcHost, pcPort, pcConnectTimeout)) {
      failUpload(String("не удалось подключиться к ПК ") + pcHost + ":" + pcPort +
                 ". Проверьте, что приёмник запущен.");
      return;
    }

    // Заголовок протокола: имя файла и перевод строки.
    String header = filename + "\n";
    if (pcClient.print(header) != header.length()) {
      failUpload("не удалось отправить имя файла на ПК.");
    }

  } else if (upload.status == UPLOAD_FILE_WRITE) {
    // После ошибки остаток файла просто пропускаем, чтобы дочитать запрос до конца.
    if (uploadFailed) {
      return;
    }

    // Очередной кусок сразу уходит в TCP-сокет, на плате ничего не копится.
    size_t written = pcClient.write(upload.buf, upload.currentSize);
    bytesSent += written;

    if (written != upload.currentSize) {
      failUpload("связь с ПК оборвалась, файл передан не полностью.");
    }

  } else if (upload.status == UPLOAD_FILE_END) {
    // Закрытие соединения — признак конца файла для приёмника.
    pcClient.stop();
    Serial.printf("Конец загрузки: получено %u байт, переслано %u байт\n",
                  (unsigned)upload.totalSize, (unsigned)bytesSent);

  } else if (upload.status == UPLOAD_FILE_ABORTED) {
    failUpload("телефон прервал загрузку.");
    uploadStarted = false;   // ответ на прерванный запрос не отправляется, состояние сбрасываем сами
  }
}

// Вызывается один раз, когда весь POST /upload принят — отвечаем браузеру.
void handleUploadDone() {
  const char* textType = "text/plain; charset=utf-8";

  if (!uploadStarted) {
    server.send(400, textType, "Ошибка: файл не выбран.");
    return;
  }
  uploadStarted = false;

  if (uploadFailed) {
    server.send(502, textType, "Ошибка: " + uploadError);
    return;
  }

  server.send(200, textType,
              "Готово! Фото отправлено на ПК, передано байт: " + String(bytesSent));
}

void handleNotFound() {
  server.send(404, "text/plain; charset=utf-8", "Страница не найдена.");
}

void setup() {
  Serial.begin(115200);
  delay(200);
  Serial.println();
  Serial.printf("Подключение к Wi-Fi \"%s\"", ssid);

  WiFi.mode(WIFI_STA);          // обычный клиент сети, а не точка доступа
  WiFi.begin(ssid, password);

  while (WiFi.status() != WL_CONNECTED) {
    delay(500);
    Serial.print(".");
  }

  Serial.println();
  Serial.println("Wi-Fi подключён.");
  Serial.print("IP-адрес платы: ");
  Serial.println(WiFi.localIP());
  Serial.print("Откройте на телефоне: http://");
  Serial.print(WiFi.localIP());
  Serial.println("/");
  Serial.printf("Фото пересылаются на ПК %s:%u\n", pcHost, pcPort);

  server.on("/", HTTP_GET, handleRoot);
  server.on("/upload", HTTP_POST, handleUploadDone, handleUploadChunk);
  server.onNotFound(handleNotFound);
  server.begin();

  Serial.println("Веб-сервер запущен.");
}

void loop() {
  server.handleClient();
}
