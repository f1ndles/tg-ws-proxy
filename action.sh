#!/system/bin/sh
MODDIR=${0%/*}
ID="tg_ws_proxy_f1ndle"
TMP_APK="/data/local/tmp/ksuwebui.apk"
ORG_PATH="$PATH"

download() {
    PATH=/data/adb/magisk:/data/data/com.termux/files/usr/bin:$PATH
    if curl --version >/dev/null 2>&1; then
        curl --connect-timeout 10 -Ls "$1"
    else
        busybox wget -T 10 --no-check-certificate -qO- "$1"
    fi
    PATH="$ORG_PATH"
}

install_and_launch() {
    APK_URL="https://github.com/5ec1cff/KsuWebUIStandalone/releases/download/v1.0/KsuWebUI-1.0-34-release.apk"
    echo "- Скачиваю KSUWebUIStandalone..."
    ping -c 1 -w 5 github.com >/dev/null 2>&1 || {
        echo "Нет интернета. Скачайте чуть позже вручную:"
        echo "  https://github.com/5ec1cff/KsuWebUIStandalone/releases"
        am start -a android.intent.action.VIEW -d "https://github.com/5ec1cff/KsuWebUIStandalone/releases"
        exit 1
    }
    download "$APK_URL" > "$TMP_APK" || {
        echo "Ошибка загрузки APK..."
        exit 1
    }
    echo "Устанавливаю..."
    pm install -r "$TMP_APK" || {
        rm -f "$TMP_APK"
        echo "! Ошибка установки APK."
        exit 1
    }
    rm -f "$TMP_APK"
    echo "Готово. Открываю WebUI..."
    am start -n "io.github.a13e300.ksuwebui/.WebUIActivity" -e id "$ID"
}

if pm path io.github.a13e300.ksuwebui >/dev/null 2>&1; then
    echo "Открываю WebUI..."
    am start -n "io.github.a13e300.ksuwebui/.WebUIActivity" -e id "$ID"
elif pm path com.dergoogler.mmrl.wx >/dev/null 2>&1; then
    echo "Открываю WebUI в MMRL WebUI X..."
    am start -n "com.dergoogler.mmrl.wx/.ui.activity.webui.WebUIActivity" -e MOD_ID "$ID"
else
    install_and_launch
fi
