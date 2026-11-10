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

  Настройки (сеть Wi-Fi и адрес ПК) можно не вписывать в код: приложение на ПК
  передаёт их по USB, а плата запоминает в своей памяти настроек.

  Команды по USB (115200 бод, поля разделены табуляцией, в конце '\n'):
    PING                      -> @PONG <имя прошивки> <версия>
    STATUS                    -> @STATUS ..., затем @CLIENT ... на каждый телефон, затем @END
    WIFI <имя сети> <пароль>  -> @OK WIFI
    PC <IP-адрес> <порт>      -> @OK PC
  Ответы платы начинаются с '@', чтобы приложение отличало их от обычного журнала.
*/

#include <WiFi.h>
#include <WebServer.h>
#include <Preferences.h>

// ============ НАСТРОЙКИ ПО УМОЛЧАНИЮ — можно оставить пустыми ============
// Используются, только пока приложение на ПК не передало свои настройки.

const char* ssid     = "";                 // название сети Wi-Fi
const char* password = "";                 // пароль сети Wi-Fi

const char*    pcHost = "";                // IP-адрес ПК, на котором запущен приёмник
const uint16_t pcPort = 5050;              // порт приёмника

// ==========================================================================

const uint16_t httpPort         = 80;      // порт веб-сервера на плате
const int32_t  pcConnectTimeout = 5000;    // сколько ждать подключения к ПК, мс
const size_t   maxCommandLength = 200;     // наибольшая длина команды по USB
const int      maxPhones        = 8;       // сколько телефонов помнить в списке

const char* firmwareName    = "esp32-photo-relay";
const int   protocolVersion = 1;

WebServer   server(httpPort);
WiFiClient  pcClient;                      // TCP-соединение с приёмником на ПК
Preferences settings;                      // память настроек, переживает перезагрузку

// Действующие настройки: из памяти платы или значения по умолчанию
String   wifiSsid;
String   wifiPassword;
String   targetHost;
uint16_t targetPort;

// Состояние текущей загрузки
bool   uploadStarted = false;              // пришёл ли вообще файл в запросе
bool   uploadFailed  = false;              // была ли ошибка при пересылке
String uploadError   = "";                 // текст ошибки для показа в браузере
size_t bytesSent     = 0;                  // сколько байт переслано на ПК

// Телефон, который открывал страницу или отправлял фото
struct Phone {
  bool          used;
  IPAddress     ip;
  unsigned long lastSeenMs;                // когда обращался в последний раз
  uint16_t      pageViews;                 // сколько раз открывал страницу
  uint16_t      uploads;                   // сколько фото отправил
};

Phone  phones[maxPhones];
String commandLine = "";                   // команда по USB, которая ещё набирается
bool   commandTooLong = false;             // набираемая команда не поместилась в отведённую длину
bool   wifiWasConnected = false;           // чтобы заметить момент подключения к Wi-Fi

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

// ------------------------------ Настройки ------------------------------

// Читает настройки из памяти платы; если их там нет — берёт значения по умолчанию.
void loadSettings() {
  settings.begin("relay", false);
  wifiSsid     = settings.getString("ssid", ssid);
  wifiPassword = settings.getString("password", password);
  targetHost   = settings.getString("pcHost", pcHost);
  targetPort   = settings.getUShort("pcPort", pcPort);
}

// Начинает подключение к Wi-Fi и сразу возвращает управление — ждать будем в loop().
void startWifi() {
  WiFi.disconnect();
  wifiWasConnected = false;

  if (wifiSsid.length() == 0) {
    Serial.println("Wi-Fi не настроен. Задайте сеть в приложении на ПК.");
    return;
  }

  Serial.printf("Подключение к Wi-Fi \"%s\"...\n", wifiSsid.c_str());
  WiFi.begin(wifiSsid.c_str(), wifiPassword.c_str());
}

// Сообщает в журнал о подключении к Wi-Fi и о его потере.
void watchWifi() {
  bool connected = (WiFi.status() == WL_CONNECTED);
  if (connected == wifiWasConnected) {
    return;
  }
  wifiWasConnected = connected;

  if (!connected) {
    Serial.println("Связь с Wi-Fi потеряна, плата подключится заново сама.");
    return;
  }

  Serial.println("Wi-Fi подключён.");
  Serial.print("IP-адрес платы: ");
  Serial.println(WiFi.localIP());
  Serial.print("Откройте на телефоне: http://");
  Serial.print(WiFi.localIP());
  Serial.println("/");
}

// ------------------------------ Список телефонов ------------------------------

// Находит запись телефона по адресу. Если её нет — занимает свободную
// или ту, к которой дольше всего не обращались.
Phone& findPhone(const IPAddress& ip) {
  int slot = 0;
  unsigned long now = millis();
  unsigned long oldestAge = 0;

  for (int i = 0; i < maxPhones; i++) {
    if (phones[i].used && phones[i].ip == ip) {
      return phones[i];
    }
  }

  for (int i = 0; i < maxPhones; i++) {
    if (!phones[i].used) {
      slot = i;
      break;
    }
    unsigned long age = now - phones[i].lastSeenMs;
    if (age >= oldestAge) {
      oldestAge = age;
      slot = i;
    }
  }

  phones[slot].used      = true;
  phones[slot].ip        = ip;
  phones[slot].pageViews = 0;
  phones[slot].uploads   = 0;
  return phones[slot];
}

// Запоминает обращение телефона, который сейчас подключён к веб-серверу.
void notePhone(bool openedPage, bool sentPhoto) {
  Phone& phone = findPhone(server.client().remoteIP());
  phone.lastSeenMs = millis();
  if (openedPage) { phone.pageViews++; }
  if (sentPhoto)  { phone.uploads++; }
}

// ------------------------------ Команды по USB ------------------------------

// Возвращает поле команды с заданным номером (поля разделены табуляцией).
String commandField(const String& line, int index) {
  int start = 0;
  for (int i = 0; i < index; i++) {
    int tab = line.indexOf('\t', start);
    if (tab < 0) {
      return "";
    }
    start = tab + 1;
  }
  int end = line.indexOf('\t', start);
  return (end < 0) ? line.substring(start) : line.substring(start, end);
}

void sendStatus() {
  bool connected = (WiFi.status() == WL_CONNECTED);
  unsigned long now = millis();

  Serial.printf("@STATUS\t%d\t%s\t%s\t%d\t%s\t%u\t%lu\n",
                connected ? 1 : 0,
                wifiSsid.c_str(),
                connected ? WiFi.localIP().toString().c_str() : "",
                connected ? (int)WiFi.RSSI() : 0,
                targetHost.c_str(),
                (unsigned)targetPort,
                now / 1000);

  for (int i = 0; i < maxPhones; i++) {
    if (!phones[i].used) {
      continue;
    }
    Serial.printf("@CLIENT\t%s\t%lu\t%u\t%u\n",
                  phones[i].ip.toString().c_str(),
                  (now - phones[i].lastSeenMs) / 1000,
                  (unsigned)phones[i].pageViews,
                  (unsigned)phones[i].uploads);
  }

  Serial.println("@END");
}

void handleCommand(const String& line) {
  String name = commandField(line, 0);

  if (name == "PING") {
    Serial.printf("@PONG\t%s\t%d\n", firmwareName, protocolVersion);

  } else if (name == "STATUS") {
    sendStatus();

  } else if (name == "WIFI") {
    String newSsid = commandField(line, 1);
    if (newSsid.length() == 0) {
      Serial.println("@ERR\tимя сети не задано");
      return;
    }
    wifiSsid     = newSsid;
    wifiPassword = commandField(line, 2);
    settings.putString("ssid", wifiSsid);
    settings.putString("password", wifiPassword);
    Serial.println("@OK\tWIFI");
    startWifi();

  } else if (name == "PC") {
    String newHost = commandField(line, 1);
    long   newPort = commandField(line, 2).toInt();
    IPAddress parsed;
    if (!parsed.fromString(newHost) || newPort < 1 || newPort > 65535) {
      Serial.println("@ERR\tневерный адрес или порт ПК");
      return;
    }
    targetHost = newHost;
    targetPort = (uint16_t)newPort;
    settings.putString("pcHost", targetHost);
    settings.putUShort("pcPort", targetPort);
    Serial.println("@OK\tPC");

  } else {
    Serial.println("@ERR\tнеизвестная команда");
  }
}

// Собирает команду из пришедших по USB символов и выполняет её по переводу строки.
void readCommands() {
  while (Serial.available() > 0) {
    char ch = (char)Serial.read();

    if (ch == '\n') {
      if (commandTooLong) {
        // Обрезанную команду выполнять нельзя: сохранилась бы, например, половина пароля.
        Serial.println("@ERR\tкоманда слишком длинная");
      } else if (commandLine.length() > 0) {
        handleCommand(commandLine);
      }
      commandLine = "";
      commandTooLong = false;
    } else if (ch == '\r') {
      // Возврат каретки перед переводом строки пропускаем.
    } else if (commandLine.length() < maxCommandLength) {
      commandLine += ch;
    } else {
      commandTooLong = true;
    }
  }
}

// ------------------------------ Веб-сервер ------------------------------

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
  notePhone(true, false);
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

    if (targetHost.length() == 0) {
      failUpload("адрес ПК не задан. Настройте плату в приложении на ПК.");
      return;
    }

    if (!pcClient.connect(targetHost.c_str(), targetPort, pcConnectTimeout)) {
      failUpload("не удалось подключиться к ПК " + targetHost + ":" + String(targetPort) +
                 ". Проверьте, что приёмник запущен.");
      return;
    }

    // Заголовок протокола: имя файла и перевод строки.
    String header = filename + "\n";
    if (pcClient.print(header) != header.length()) {
      failUpload("не удалось отправить имя файла на ПК.");
    }

  } else if (upload.status == UPLOAD_FILE_WRITE) {
    // Большое фото передаётся долго — не забываем отвечать приложению по USB.
    readCommands();

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
    if (!uploadFailed) {
      notePhone(false, true);   // в счётчик идут только фото, дошедшие до ПК
    }
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

// ------------------------------ Запуск ------------------------------

void setup() {
  Serial.setRxBufferSize(1024);   // запас, чтобы команды не терялись, пока плата занята
  Serial.begin(115200);
  delay(200);
  Serial.println();
  Serial.println("esp32-photo-relay запущен.");

  loadSettings();

  WiFi.mode(WIFI_STA);          // обычный клиент сети, а не точка доступа
  WiFi.setAutoReconnect(true);
  startWifi();

  server.on("/", HTTP_GET, handleRoot);
  server.on("/upload", HTTP_POST, handleUploadDone, handleUploadChunk);
  server.onNotFound(handleNotFound);
  server.begin();

  Serial.println("Веб-сервер запущен.");
}

void loop() {
  readCommands();
  watchWifi();
  server.handleClient();
}
