#!/system/bin/sh
MODDIR=${0%/*}
BIN_NAME="tg-ws-proxy"
BIN="/data/adb/$BIN_NAME"
[ ! -f "$BIN" ] && BIN="$MODDIR/system/bin/$BIN_NAME"
LOG="$MODDIR/proxy.log"
CONF="$MODDIR/config.conf"
PID_FILE="$MODDIR/proxy.pid"
STARTED="$MODDIR/started"
STOPPED="$MODDIR/stopped"
chmod 755 "$BIN"
chmod 777 "$MODDIR"
E="bm9za29tbmFkem9yLmNvLnVrIGNha2Vpc2FsaWUuY28udWsgS2FydG9zaGthLmNvLnVrIHBjbGVhZC5jby51aw=="

generate_secret() {
    echo "$(date)$RANDOM$(uptime)" | md5sum | cut -d' ' -f1
}

select_best_domain() {
    DECODED=$(echo "$E" | base64 -d)
    BEST_DOMAIN=""
    MIN_LATENCY=9999
    for dom in $DECODED; do
        PING_RES=$(ping -c 3 -W 2 "$dom" 2>/dev/null | tail -1)
        if [ -n "$PING_RES" ]; then
            LATENCY=$(echo "$PING_RES" | cut -d'/' -f5 | cut -d'.' -f1)
            if [ -n "$LATENCY" ] && [ "$LATENCY" -lt "$MIN_LATENCY" ]; then
                MIN_LATENCY=$LATENCY
                BEST_DOMAIN=$dom
            fi
        fi
    done
    if [ -z "$BEST_DOMAIN" ]; then
        set -- $DECODED
        shift $(($RANDOM % $#))
        BEST_DOMAIN=$1
    fi
    echo "$BEST_DOMAIN"
}

if [ -f "$PID_FILE" ]; then
    PID=$(cat "$PID_FILE")
    if kill -0 "$PID" 2>/dev/null; then
        kill "$PID"
        rm -f "$PID_FILE"
        rm -f "$STARTED"
        touch "$STOPPED"
        echo "Статус: Остановлено"
        exit 0
    fi
    rm -f "$PID_FILE"
fi

if [ ! -f "$CONF" ]; then
    SECRET=$(generate_secret)
    cat <<CONFEOF > "$CONF"
PORT=1443
HOST=127.0.0.1
SECRET=$SECRET
CF_DOMAIN=f1ndle.biz
FAKE_TLS=www.mozilla.org
AUTOSTART=OFF
CD_BYPASS=OFF
AUTO_TG=OFF
DEFAULT_DOMAINS=ON
CF_PRIORITY=ON
CF_BALANCE=OFF
DC_IP=1:149.154.175.50,2:149.154.167.220,3:149.154.167.151,4:149.154.167.220,5:149.154.171.5,203:91.105.192.100
CONFEOF
else
    . "$CONF"
    SECRET=$(printf "%s" "$SECRET" | tr -d '\r\n ')
fi

pkill -9 "$BIN_NAME" 2>/dev/null

if [ -z "$SECRET" ] || [ ${#SECRET} -ne 32 ] || echo "$SECRET" | grep -q "[^0-9a-fA-F]"; then
    SECRET=$(generate_secret)
    grep -v "^SECRET=" "$CONF" > "${CONF}.tmp"
    echo "SECRET=$SECRET" >> "${CONF}.tmp"
    mv "${CONF}.tmp" "$CONF"
fi

if [ "$CD_BYPASS" = "ON" ]; then
    BEST=$(select_best_domain)
    CLEAN_DOMAIN=$(echo "$BEST" | sed -E 's/^kws[0-9]*\.//')
    grep -v "^CF_DOMAIN=" "$CONF" > "${CONF}.tmp"
    echo "CF_DOMAIN=$CLEAN_DOMAIN" >> "${CONF}.tmp"
    mv "${CONF}.tmp" "$CONF"
else
    CLEAN_DOMAIN="$CF_DOMAIN"
fi

setprop net.dns1 8.8.8.8
setprop net.dns2 1.1.1.1
rm -f "$LOG"
rm -f "$STARTED" "$STOPPED"
ARGS="--port $PORT --host $HOST --secret $SECRET"
[ -n "$CLEAN_DOMAIN" ] && ARGS="$ARGS --cf-domain $CLEAN_DOMAIN"
[ -n "$FAKE_TLS" ] && ARGS="$ARGS --listen-faketls-domain $FAKE_TLS"
[ "$DEFAULT_DOMAINS" = "ON" ] && ARGS="$ARGS --default-domains"
[ "$CF_PRIORITY" = "ON" ] && ARGS="$ARGS --cf-priority"
[ "$CF_BALANCE" = "ON" ] && ARGS="$ARGS --cf-balance"
if [ -n "$DC_IP" ]; then
    _tmp_dc=$(echo "$DC_IP" | tr ',' ' ')
    for _e in $_tmp_dc; do
        ARGS="$ARGS --dc-ip $_e"
    done
fi
export RUST_LOG=info
nohup $BIN $ARGS > "$LOG" 2>&1 &
NEW_PID=$!
echo $NEW_PID > "$PID_FILE"
for i in $(seq 1 10); do
    sleep 1
    if [ -f "$LOG" ]; then
        RAW_LINK=$(grep -o "tg://proxy?server=[^ ]*" "$LOG" | tail -n 1)
        if [ -n "$RAW_LINK" ]; then
            LINK=$(echo "$RAW_LINK" | sed "s/server=[^&]*/server=$HOST/")
            echo "Статус: Работает на $PORT"
            touch "$STARTED"
            if [ "$AUTO_TG" = "ON" ]; then
                am start -a android.intent.action.VIEW -d "$LINK" >/dev/null 2>&1
            fi
            exit 0
        fi
    fi
    if ! kill -0 $NEW_PID 2>/dev/null; then
        rm -f "$PID_FILE"
        touch "$STOPPED"
        exit 1
    fi
done
if kill -0 $NEW_PID 2>/dev/null; then
    touch "$STARTED"
else
    rm -f "$PID_FILE"
    touch "$STOPPED"
fi
