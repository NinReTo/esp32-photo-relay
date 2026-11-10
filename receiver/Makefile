# Сборка приёмника на Linux/macOS:
#   make        — собрать
#   make run    — собрать и запустить
#   make clean  — удалить собранную программу
#
# Сборка на Windows описана в README.md (MinGW или MSVC).

CC     ?= gcc
CFLAGS ?= -O2 -Wall -Wextra

receiver: receiver.c
	$(CC) $(CFLAGS) receiver.c -o receiver

run: receiver
	./receiver

clean:
	rm -f receiver

.PHONY: run clean
