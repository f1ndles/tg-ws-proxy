#!/system/bin/sh
MODDIR=${0%/*}
BIN="/data/adb/tg-ws-proxy"
[ ! -f "$BIN" ] && BIN="$MODDIR/system/bin/tg-ws-proxy"
CONF="$MODDIR/config.conf"
LOG="$MODDIR/proxy.log"
PID_FILE="$MODDIR/proxy.pid"
STARTED="$MODDIR/started"
STOPPED="$MODDIR/stopped"
BIN_NAME="tg-ws-proxy"
E="bm9za29tbmFkem9yLmNvLnVrIGNha2Vpc2FsaWUuY28udWsgS2FydG9zaGthLmNvLnVrIHBjbGVhZC5jby51aw=="
until [ "$(getprop sys.boot_completed)" = "1" ]; do
    sleep 5
done
sleep 10
if [ ! -f "$CONF" ]; then
    exit 1
fi
. "$CONF"
SECRET=$(printf "%s" "$SECRET" | tr -d '\r\n ')
if [ "$AUTOSTART" != "ON" ] || [ -z "$SECRET" ]; then
    rm -f "$STARTED"
    touch "$STOPPED"
    exit 0
fi
if [ ! -f "$BIN" ] || [ ! -x "$BIN" ]; then
    chmod 755 "$BIN" 2>/dev/null
    [ ! -f "$BIN" ] && exit 1
fi
if [ -f "$PID_FILE" ]; then
    PID=$(cat "$PID_FILE")
    if kill -0 "$PID" 2>/dev/null; then
        rm -f "$STOPPED"
        touch "$STARTED"
        exit 0
    fi
    rm -f "$PID_FILE"
fi
pkill -9 "$BIN_NAME" 2>/dev/null
if [ "$CD_BYPASS" = "ON" ]; then
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
    CLEAN_DOMAIN=$(echo "$BEST_DOMAIN" | sed -E 's/^kws[0-9]*\.//')
    grep -v "^CF_DOMAIN=" "$CONF" > "${CONF}.tmp"
    echo "CF_DOMAIN=$CLEAN_DOMAIN" >> "${CONF}.tmp"
    mv "${CONF}.tmp" "$CONF"
else
    CLEAN_DOMAIN="$CF_DOMAIN"
fi
setprop net.dns1 8.8.8.8
setprop net.dns2 1.1.1.1
ARGS="--port ${PORT:-1443} --host ${HOST:-127.0.0.1} --secret $SECRET"
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
rm -f "$STARTED" "$STOPPED"
RETRY=0
while [ $RETRY -lt 5 ]; do
    rm -f "$LOG"
    export RUST_LOG=info
    nohup $BIN $ARGS > "$LOG" 2>&1 &
    NEW_PID=$!
    echo $NEW_PID > "$PID_FILE"
    sleep 3
    if kill -0 "$NEW_PID" 2>/dev/null; then
        touch "$STARTED"
        exit 0
    fi
    RETRY=$((RETRY + 1))
    sleep 5
done
rm -f "$PID_FILE"
touch "$STOPPED"
exit 1
