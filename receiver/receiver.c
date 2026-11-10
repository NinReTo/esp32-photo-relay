/*
 * esp32-photo-relay — приёмник на ПК
 *
 * Слушает TCP-порт, принимает от ESP32 поток байт и сохраняет его в файл
 * в папке received/.
 *
 * Протокол:
 *   1. имя файла и символ перевода строки '\n';
 *   2. содержимое файла как есть;
 *   3. закрытие соединения — признак конца файла.
 *
 * Сборка:
 *   Linux/macOS:      gcc receiver.c -o receiver
 *   Windows (MinGW):  gcc receiver.c -o receiver.exe -lws2_32
 *   Windows (MSVC):   cl /utf-8 receiver.c ws2_32.lib
 *
 * Запуск:  receiver [порт]      (по умолчанию порт 5050)
 */

#ifdef _MSC_VER
  /* MSVC предупреждает об обычных функциях вроде localtime — здесь они уместны. */
  #define _CRT_SECURE_NO_WARNINGS
#endif

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#ifdef _WIN32
  #ifndef _WIN32_WINNT
    #define _WIN32_WINNT 0x0601   /* нужно старым версиям MinGW, чтобы была доступна inet_ntop */
  #endif
  #include <winsock2.h>
  #include <ws2tcpip.h>
  #include <windows.h>
  #include <direct.h>
  #ifdef _MSC_VER
    #pragma comment(lib, "ws2_32.lib")
  #endif
  typedef SOCKET socket_t;
  typedef int    socklen_type;
  #define close_socket(s) closesocket(s)
  #define make_dir(path)  _mkdir(path)
#else
  #include <sys/types.h>
  #include <sys/socket.h>
  #include <sys/stat.h>
  #include <sys/time.h>
  #include <netinet/in.h>
  #include <arpa/inet.h>
  #include <unistd.h>
  typedef int       socket_t;
  typedef socklen_t socklen_type;
  #define INVALID_SOCKET  (-1)
  #define close_socket(s) close(s)
  #define make_dir(path)  mkdir(path, 0755)
#endif

#define DEFAULT_PORT        5050
#define OUTPUT_DIR          "received"
#define MAX_NAME_LEN        200     /* наибольшая длина имени файла, байт */
#define MAX_PATH_LEN        512
#define RECV_BUFFER_SIZE    4096
#define RECV_TIMEOUT_SEC    30      /* сколько ждать данных от молчащей платы */
#define MAX_NAME_ATTEMPTS   1000    /* сколько вариантов имени пробовать, если файл уже есть */

/* ---------- Запуск и остановка сокетов (нужны только на Windows) ---------- */

static int sockets_init(void)
{
#ifdef _WIN32
    WSADATA wsa_data;
    /* Чтобы русский текст в консоли Windows не превращался в кракозябры. */
    SetConsoleOutputCP(CP_UTF8);
    return WSAStartup(MAKEWORD(2, 2), &wsa_data) == 0;
#else
    return 1;
#endif
}

static void sockets_cleanup(void)
{
#ifdef _WIN32
    WSACleanup();
#endif
}

static void print_socket_error(const char *what)
{
#ifdef _WIN32
    fprintf(stderr, "Ошибка сокета (%s), код %d\n", what, WSAGetLastError());
#else
    perror(what);
#endif
}

/* Ограничивает время ожидания данных, чтобы зависшая плата не блокировала приёмник навсегда. */
static void set_recv_timeout(socket_t sock, int seconds)
{
#ifdef _WIN32
    DWORD timeout_ms = (DWORD)seconds * 1000;
    setsockopt(sock, SOL_SOCKET, SO_RCVTIMEO, (const char *)&timeout_ms, sizeof(timeout_ms));
#else
    struct timeval timeout;
    timeout.tv_sec  = seconds;
    timeout.tv_usec = 0;
    setsockopt(sock, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout));
#endif
}

/* ---------- Работа с файлами ---------- */

/*
 * Открывает файл, имя которого записано в UTF-8.
 * На Windows обычный fopen не понимает UTF-8, поэтому имя переводится в UTF-16.
 */
static FILE *open_file(const char *path, const char *mode)
{
#ifdef _WIN32
    wchar_t wide_path[MAX_PATH_LEN];
    wchar_t wide_mode[8];
    if (MultiByteToWideChar(CP_UTF8, 0, path, -1, wide_path, MAX_PATH_LEN) == 0 ||
        MultiByteToWideChar(CP_UTF8, 0, mode, -1, wide_mode, 8) == 0) {
        return NULL;
    }
    return _wfopen(wide_path, wide_mode);
#else
    return fopen(path, mode);
#endif
}

static int file_exists(const char *path)
{
    FILE *file = open_file(path, "rb");
    if (file == NULL) {
        return 0;
    }
    fclose(file);
    return 1;
}

static void remove_file(const char *path)
{
#ifdef _WIN32
    wchar_t wide_path[MAX_PATH_LEN];
    if (MultiByteToWideChar(CP_UTF8, 0, path, -1, wide_path, MAX_PATH_LEN) != 0) {
        _wremove(wide_path);
    }
#else
    remove(path);
#endif
}

/* ---------- Имя файла ---------- */

/* Запасное имя по текущей дате и времени, например photo_20261110_200512.jpg */
static void generate_name(char *name, size_t size)
{
    time_t now = time(NULL);
    struct tm *local = localtime(&now);
    if (local == NULL || strftime(name, size, "photo_%Y%m%d_%H%M%S.jpg", local) == 0) {
        snprintf(name, size, "photo.jpg");
    }
}

/*
 * Проверяет, не совпадает ли имя (до первой точки) с именем устройства Windows:
 * CON, PRN, AUX, NUL, COM1..COM9, LPT1..LPT9. Запись в такой «файл» ушла бы
 * в устройство, например в последовательный порт.
 */
static int is_reserved_name(const char *name)
{
    static const char *const devices[] = { "CON", "PRN", "AUX", "NUL", "COM", "LPT" };
    char stem[5] = "";
    size_t length = 0;
    size_t i;

    while (name[length] != '\0' && name[length] != '.') {
        if (length >= 4) {
            return 0;   /* имена устройств не длиннее четырёх букв */
        }
        stem[length] = name[length];
        if (stem[length] >= 'a' && stem[length] <= 'z') {
            stem[length] = (char)(stem[length] - 'a' + 'A');
        }
        length++;
    }
    stem[length] = '\0';

    for (i = 0; i < sizeof(devices) / sizeof(devices[0]); i++) {
        if (strncmp(stem, devices[i], 3) != 0) {
            continue;
        }
        if (i < 4) {
            if (length == 3) {
                return 1;                       /* CON, PRN, AUX, NUL */
            }
        } else if (length == 4 && stem[3] >= '1' && stem[3] <= '9') {
            return 1;                           /* COM1..COM9, LPT1..LPT9 */
        }
    }
    return 0;
}

/*
 * Делает из присланного имени безопасное: без папок и без выхода наверх.
 * Убирает слэши, обратные слэши, управляющие символы, знаки, запрещённые
 * в именах файлов Windows, и все сочетания "..".
 * Если после чистки ничего осмысленного не осталось — подставляет запасное имя.
 */
static void sanitize_name(const char *raw, char *safe, size_t size)
{
    const char *forbidden = "/\\:*?\"<>|";
    size_t length = 0;
    size_t i;
    char *dots;

    for (i = 0; raw[i] != '\0' && length + 1 < size; i++) {
        unsigned char ch = (unsigned char)raw[i];
        if (ch < 32 || ch == 127 || strchr(forbidden, ch) != NULL) {
            continue;
        }
        safe[length++] = (char)ch;
    }
    safe[length] = '\0';

    /* Вырезаем ".." до тех пор, пока они находятся ("...." тоже исчезнет). */
    while ((dots = strstr(safe, "..")) != NULL) {
        memmove(dots, dots + 2, strlen(dots + 2) + 1);
    }

    /* Точки и пробелы по краям имени Windows не любит — убираем. */
    length = strlen(safe);
    while (length > 0 && (safe[length - 1] == '.' || safe[length - 1] == ' ')) {
        safe[--length] = '\0';
    }
    i = 0;
    while (safe[i] == '.' || safe[i] == ' ') {
        i++;
    }
    memmove(safe, safe + i, strlen(safe + i) + 1);

    if (safe[0] == '\0' || is_reserved_name(safe)) {
        generate_name(safe, size);
    }
}

/*
 * Собирает путь в папке received/. Если такой файл уже есть, добавляет
 * к имени номер (photo.jpg -> photo_1.jpg), чтобы не затереть старое фото.
 * Возвращает 1 при успехе, 0 — если свободное имя подобрать не удалось.
 */
static int build_unique_path(char *name, size_t name_size, char *path, size_t path_size)
{
    char base[MAX_NAME_LEN + 1];
    char extension[MAX_NAME_LEN + 1] = "";
    char *dot;
    int attempt;

    snprintf(path, path_size, "%s/%s", OUTPUT_DIR, name);
    if (!file_exists(path)) {
        return 1;
    }

    snprintf(base, sizeof(base), "%s", name);
    dot = strrchr(base, '.');
    if (dot != NULL && dot != base) {
        snprintf(extension, sizeof(extension), "%s", dot);
        *dot = '\0';
    }

    for (attempt = 1; attempt <= MAX_NAME_ATTEMPTS; attempt++) {
        snprintf(name, name_size, "%s_%d%s", base, attempt, extension);
        snprintf(path, path_size, "%s/%s", OUTPUT_DIR, name);
        if (!file_exists(path)) {
            return 1;
        }
    }
    return 0;
}

/* ---------- Приём одного файла ---------- */

/*
 * Читает имя файла: байты до символа '\n'.
 * Слишком длинное имя обрезается, но поток дочитывается до '\n',
 * чтобы хвост имени не попал в содержимое файла.
 * Возвращает 1, если '\n' получен, и 0, если соединение закрылось раньше.
 */
static int read_name_line(socket_t client, char *line, size_t size)
{
    size_t length = 0;
    char ch;

    for (;;) {
        int received = recv(client, &ch, 1, 0);
        if (received <= 0) {
            return 0;
        }
        if (ch == '\n') {
            break;
        }
        if (ch != '\r' && length + 1 < size) {
            line[length++] = ch;
        }
    }
    line[length] = '\0';
    return 1;
}

/* Принимает один файл от подключившейся платы и сохраняет его на диск. */
static void receive_file(socket_t client)
{
    char raw_name[MAX_NAME_LEN + 1];
    char name[MAX_NAME_LEN + 32];
    char path[MAX_PATH_LEN];
    char buffer[RECV_BUFFER_SIZE];
    unsigned long long total = 0;
    int write_failed = 0;
    int received;
    FILE *file;

    if (!read_name_line(client, raw_name, sizeof(raw_name))) {
        fprintf(stderr, "Соединение закрылось раньше, чем пришло имя файла.\n");
        return;
    }

    sanitize_name(raw_name, name, MAX_NAME_LEN + 1);

    if (!build_unique_path(name, sizeof(name), path, sizeof(path))) {
        fprintf(stderr, "Не удалось подобрать свободное имя для файла \"%s\".\n", name);
        return;
    }

    file = open_file(path, "wb");
    if (file == NULL) {
        /* Система не приняла присланное имя (например, оно слишком длинное) — берём запасное. */
        generate_name(name, sizeof(name));
        if (build_unique_path(name, sizeof(name), path, sizeof(path))) {
            file = open_file(path, "wb");
        }
    }
    if (file == NULL) {
        fprintf(stderr, "Не удалось создать файл %s\n", path);
        return;
    }

    printf("Приём файла \"%s\"...\n", name);

    while ((received = recv(client, buffer, sizeof(buffer), 0)) > 0) {
        if (fwrite(buffer, 1, (size_t)received, file) != (size_t)received) {
            write_failed = 1;
            break;
        }
        total += (unsigned long long)received;
    }

    if (fclose(file) != 0) {
        write_failed = 1;
    }

    if (write_failed) {
        fprintf(stderr, "Ошибка записи на диск: %s (возможно, кончилось место).\n", path);
    } else if (received < 0) {
        print_socket_error("recv");
        fprintf(stderr, "Соединение оборвалось, файл может быть неполным.\n");
    }

    if (total == 0) {
        remove_file(path);
        fprintf(stderr, "Данные не получены, пустой файл не сохранён.\n");
        return;
    }

    printf("Файл сохранён.\n");
    printf("  Имя:    %s\n", name);
    printf("  Путь:   %s\n", path);
    printf("  Размер: %llu байт\n", total);
}

/* ---------- Главная программа ---------- */

/* Создаёт сокет, который слушает заданный порт на всех сетевых адресах ПК. */
static socket_t open_listener(int port)
{
    struct sockaddr_in address;
    socket_t listener = socket(AF_INET, SOCK_STREAM, 0);

    if (listener == INVALID_SOCKET) {
        print_socket_error("socket");
        return INVALID_SOCKET;
    }

#ifndef _WIN32
    {
        /* Позволяет сразу перезапустить приёмник на том же порту. */
        int enable = 1;
        setsockopt(listener, SOL_SOCKET, SO_REUSEADDR, &enable, sizeof(enable));
    }
#endif

    memset(&address, 0, sizeof(address));
    address.sin_family      = AF_INET;
    address.sin_addr.s_addr = htonl(INADDR_ANY);
    address.sin_port        = htons((unsigned short)port);

    if (bind(listener, (struct sockaddr *)&address, sizeof(address)) != 0) {
        print_socket_error("bind");
        fprintf(stderr, "Не удалось занять порт %d. Возможно, он уже используется.\n", port);
        close_socket(listener);
        return INVALID_SOCKET;
    }

    if (listen(listener, 1) != 0) {
        print_socket_error("listen");
        close_socket(listener);
        return INVALID_SOCKET;
    }

    return listener;
}

int main(int argc, char *argv[])
{
    int port = DEFAULT_PORT;
    socket_t listener;

    if (argc > 1) {
        port = atoi(argv[1]);
        if (port < 1 || port > 65535) {
            fprintf(stderr, "Неверный порт \"%s\". Нужно число от 1 до 65535.\n", argv[1]);
            return 1;
        }
    }

    /* Без буферизации сообщения видны сразу, даже если вывод перенаправлен в файл. */
    setvbuf(stdout, NULL, _IONBF, 0);

    if (!sockets_init()) {
        fprintf(stderr, "Не удалось запустить сетевую подсистему.\n");
        return 1;
    }

    /* Если папка уже есть, вызов просто вернёт ошибку — это не страшно. */
    make_dir(OUTPUT_DIR);

    listener = open_listener(port);
    if (listener == INVALID_SOCKET) {
        sockets_cleanup();
        return 1;
    }

    printf("Приёмник esp32-photo-relay запущен.\n");
    printf("Порт: %d, файлы сохраняются в папку \"%s\".\n", port, OUTPUT_DIR);
    printf("Для выхода нажмите Ctrl+C.\n");

    for (;;) {
        struct sockaddr_in client_address;
        socklen_type address_size = sizeof(client_address);
        char client_ip[INET_ADDRSTRLEN] = "?";
        socket_t client;

        printf("\nОжидание подключения...\n");

        client = accept(listener, (struct sockaddr *)&client_address, &address_size);
        if (client == INVALID_SOCKET) {
            print_socket_error("accept");
            continue;
        }

        inet_ntop(AF_INET, &client_address.sin_addr, client_ip, sizeof(client_ip));
        printf("Подключение от %s\n", client_ip);

        set_recv_timeout(client, RECV_TIMEOUT_SEC);
        receive_file(client);
        close_socket(client);
    }

    /* Сюда программа не доходит: цикл выше бесконечный, выход — по Ctrl+C. */
}
