#!/system/bin/sh
BIN_SRC="$MODPATH/system/bin/tg-ws-proxy"
BIN_DST="/data/adb/tg-ws-proxy"
if [ -f "$BIN_SRC" ]; then
    cp "$BIN_SRC" "$BIN_DST"
    chmod 755 "$BIN_DST"
    ui_print "- Бинарник установлен в /data/adb/"
else
    if [ -f "$BIN_DST" ]; then
        ui_print "- Используется существующий бинарник из /data/adb/"
    else
        ui_print "! ВНИМАНИЕ: бинарник tg-ws-proxy не найден! (А куда он делся?)"
        ui_print "! Скачайте его вручную в /data/adb/tg-ws-proxy (Без него ниче не заработает)"
    fi
fi
OLD_CONF="/data/adb/modules/tg_ws_proxy_f1ndle/config.conf"
if [ -f "$OLD_CONF" ]; then
    sed 's/\r//' "$OLD_CONF" > "$MODPATH/config.conf"
    ui_print "Конфиг сохранён из предыдущей версии"
fi
rm -f "$MODPATH/proxy.pid" "$MODPATH/started"
touch "$MODPATH/stopped"
