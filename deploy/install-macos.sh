#!/bin/bash
# Instala o host-status num Mac como LaunchAgent do usuário. Sem sudo.
# Rodar da pasta que tem host-status (binário) e config.json.
#   bash install-macos.sh             instala ou atualiza
#   bash install-macos.sh --rollback  remove e religa o painel antigo em Python, se havia
set -euo pipefail

SOURCE="$(cd "$(dirname "$0")" && pwd)"
LABEL="local.host-status"
DATA="$HOME/Library/Application Support/host-status"
BIN="$HOME/.local/bin/host-status"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
LOG="$HOME/Library/Logs/host-status.log"
OLD_DATA="$HOME/Library/Application Support/mac-status"
SAVED="$DATA/painel-antigo"
DOMAIN="gui/$(id -u)"

if [ "$(id -u)" -eq 0 ]; then
  echo "rode como o usuário da sessão, sem sudo" >&2
  exit 1
fi

if [ "${1:-}" = "--rollback" ]; then
  launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
  rm -f "$PLIST"
  if [ -d "$SAVED" ]; then
    for old in "$SAVED"/*.plist; do
      [ -f "$old" ] || continue
      cp "$old" "$HOME/Library/LaunchAgents/"
      launchctl bootstrap "$DOMAIN" "$HOME/Library/LaunchAgents/$(basename "$old")" && echo "religado: $(basename "$old" .plist)"
    done
  fi
  echo "host-status removido. O banco continua em $DATA."
  exit 0
fi

for file in host-status config.json; do
  [ -f "$SOURCE/$file" ] || { echo "falta $SOURCE/$file" >&2; exit 1; }
done
chmod 0755 "$SOURCE/host-status"
xattr -c "$SOURCE/host-status" 2>/dev/null || true
"$SOURCE/host-status" --version
"$SOURCE/host-status" --config "$SOURCE/config.json" --check-config

install -d -m 0755 "$HOME/.local/bin" "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
install -d -m 0700 "$DATA"
install -m 0755 "$SOURCE/host-status" "$BIN"
install -m 0644 "$SOURCE/config.json" "$DATA/config.json"

# Painel antigo em Python: para os agentes e guarda os plists para o rollback.
for old in local.mac-status.collect local.mac-status.serve; do
  if [ -f "$HOME/Library/LaunchAgents/$old.plist" ]; then
    launchctl bootout "$DOMAIN/$old" 2>/dev/null || true
    install -d -m 0700 "$SAVED"
    mv "$HOME/Library/LaunchAgents/$old.plist" "$SAVED/"
    echo "desligado: $old"
  fi
done

# Histórico do painel antigo: só na primeira instalação.
if [ -f "$OLD_DATA/status.db" ] && [ ! -f "$DATA/status.db" ]; then
  "$BIN" --config "$DATA/config.json" --data "$DATA" --once > /dev/null
  sqlite3 "$DATA/status.db" <<SQL
ATTACH '$OLD_DATA/status.db' AS old;
INSERT OR IGNORE INTO ssh_events (uid, ts, kind, method, user, ip, key)
  SELECT uid, ts, kind, method, user, ip, key FROM old.ssh_events;
INSERT OR IGNORE INTO samples (ts, cpu_pct, load1, mem_used_pct, swap_used_mb, disk_used_pct,
    disk_free_bytes, rx_bytes, tx_bytes, rx_bps, tx_bps, mem_p1_bytes, mem_p2_bytes, mem_p3_bytes)
  SELECT ts, cpu_pct, load1, mem_used_pct, swap_used_mb, disk_used_pct, disk_free_bytes,
    rx_bytes, tx_bytes, rx_bps, tx_bps, mem_active_bytes, mem_wired_bytes, mem_compressed_bytes
  FROM old.samples;
SQL
  echo "histórico importado de $OLD_DATA (o original fica lá)"
fi

cat > "$PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>$BIN</string>
    <string>--config</string><string>$DATA/config.json</string>
    <string>--data</string><string>$DATA</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>ProcessType</key><string>Background</string>
  <key>StandardErrorPath</key><string>$LOG</string>
</dict>
</plist>
PLIST
plutil -lint "$PLIST" > /dev/null
launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
launchctl bootstrap "$DOMAIN" "$PLIST"

loopback="$(grep -oE '"127\.0\.0\.1:[0-9]+"' "$DATA/config.json" | head -n 1 | tr -d '"')"
loopback="${loopback:-127.0.0.1:8787}"
for path in live.json current.json; do
  ok=""
  for _ in $(seq 1 45); do
    if curl -fs --max-time 3 -o /dev/null "http://$loopback/$path"; then ok=1; break; fi
    sleep 2
  done
  [ -n "$ok" ] || { echo "a página não respondeu $path; veja $LOG" >&2; exit 1; }
done
port="${loopback##*:}"
if lsof -nP -iTCP:"$port" -sTCP:LISTEN | awk 'NR>1 {print $9}' | grep -qE '^\*:'; then
  echo "a porta $port abriu em todas as interfaces; parando" >&2
  launchctl bootout "$DOMAIN/$LABEL"
  exit 1
fi
echo "ok: http://$loopback/"
