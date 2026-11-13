#!/bin/bash
# Instala o host-status num Linux com systemd. Rodar como root, da pasta que tem
# host-status (binário), host-status.service e config.json.
#   sudo bash install-linux.sh             instala ou atualiza
#   sudo bash install-linux.sh --rollback  remove e religa o painel antigo, se havia
set -euo pipefail

SOURCE="$(cd "$(dirname "$0")" && pwd)"
STATE=/var/lib/host-status
MIGRATED="$STATE/painel-antigo.txt"

if [ "$(id -u)" -ne 0 ]; then
  echo "rode como root: sudo bash $0 $*" >&2
  exit 1
fi

if [ "${1:-}" = "--rollback" ]; then
  systemctl disable --now host-status.service 2>/dev/null || true
  rm -f /etc/systemd/system/host-status.service
  systemctl daemon-reload
  if command -v firewall-cmd >/dev/null && firewall-cmd --state >/dev/null 2>&1 && [ -f /etc/host-status/config.json ]; then
    for port in $(grep -oE '"tailscale:[0-9]+"' /etc/host-status/config.json | grep -oE '[0-9]+'); do
      rule="rule family=\"ipv4\" source address=\"100.64.0.0/10\" port port=\"$port\" protocol=\"tcp\" accept"
      firewall-cmd --permanent --remove-rich-rule="$rule" >/dev/null 2>&1 || true
      firewall-cmd --remove-rich-rule="$rule" >/dev/null 2>&1 || true
    done
  fi
  if [ -f "$MIGRATED" ]; then
    while read -r unit; do
      [ -n "$unit" ] && systemctl enable --now "$unit" && echo "religado: $unit"
    done < "$MIGRATED"
    rm -f "$MIGRATED"
  fi
  echo "host-status removido. O banco continua em $STATE."
  exit 0
fi

for file in host-status host-status.service config.json; do
  if [ ! -f "$SOURCE/$file" ]; then
    echo "falta $SOURCE/$file" >&2
    exit 1
  fi
done
chmod 0755 "$SOURCE/host-status"
"$SOURCE/host-status" --version
"$SOURCE/host-status" --config "$SOURCE/config.json" --check-config

if grep -q '"lan_devices": *true' "$SOURCE/config.json"; then
  missing=""
  command -v fping >/dev/null || missing="$missing fping"
  [ -f /usr/share/ieee-data/oui.txt ] || [ -f /var/lib/ieee-data/oui.txt ] || missing="$missing ieee-data"
  if [ -n "$missing" ]; then
    echo "== instalando:$missing"
    DEBIAN_FRONTEND=noninteractive apt-get install -y $missing
  fi
fi

install -d -m 0755 /etc/host-status
install -m 0755 "$SOURCE/host-status" /usr/local/bin/host-status
install -m 0644 "$SOURCE/config.json" /etc/host-status/config.json
install -m 0644 "$SOURCE/host-status.service" /etc/systemd/system/host-status.service
install -d -m 0700 "$STATE"

# Painel antigo em Python, de instalações anteriores: timer do coletor e Caddy servindo a página.
touch "$MIGRATED"
for timer in $(systemctl list-unit-files --no-legend '*-status-collect.timer' 2>/dev/null | awk '{print $1}'); do
  if systemctl is-enabled --quiet "$timer" 2>/dev/null; then
    systemctl disable --now "$timer"
    echo "$timer" >> "$MIGRATED"
    echo "desligado: $timer"
  fi
done
if systemctl is-active --quiet caddy 2>/dev/null && grep -qE '/var/lib/[a-z]+-status/www' /etc/caddy/Caddyfile 2>/dev/null; then
  systemctl disable --now caddy
  echo "caddy.service" >> "$MIGRATED"
  echo "desligado: caddy (só servia o painel antigo)"
fi

# Portas da Tailscale no ufw, quando ele está ligado.
if command -v ufw >/dev/null && ufw status 2>/dev/null | grep -q "Status: active"; then
  for port in $(grep -oE '"tailscale:[0-9]+"' "$SOURCE/config.json" | grep -oE '[0-9]+'); do
    ufw allow in on tailscale0 to any port "$port" proto tcp comment host-status >/dev/null
    echo "ufw: liberada a porta $port só na tailscale0"
  done
fi

# Portas da Tailscale no firewalld (Oracle Cloud e afins): só a faixa da Tailscale, nunca a internet.
if command -v firewall-cmd >/dev/null && firewall-cmd --state >/dev/null 2>&1; then
  for port in $(grep -oE '"tailscale:[0-9]+"' "$SOURCE/config.json" | grep -oE '[0-9]+'); do
    rule="rule family=\"ipv4\" source address=\"100.64.0.0/10\" port port=\"$port\" protocol=\"tcp\" accept"
    firewall-cmd --permanent --add-rich-rule="$rule" >/dev/null
    firewall-cmd --add-rich-rule="$rule" >/dev/null
    echo "firewalld: liberada a porta $port só para 100.64.0.0/10"
  done
fi

systemctl daemon-reload
systemctl enable host-status.service >/dev/null
systemctl restart host-status.service

loopback="$(grep -oE '"127\.0\.0\.1:[0-9]+"' "$SOURCE/config.json" | head -n 1 | tr -d '"')"
tailscale_port="$(grep -oE '"tailscale:[0-9]+"' "$SOURCE/config.json" | head -n 1 | grep -oE '[0-9]+' || true)"
if [ -n "$loopback" ]; then
  for path in live.json current.json; do
    ok=""
    for _ in $(seq 1 45); do
      if curl -fs --max-time 3 -o /dev/null "http://$loopback/$path"; then ok=1; break; fi
      sleep 2
    done
    if [ -z "$ok" ]; then
      echo "a página não respondeu $path em http://$loopback/; veja: journalctl -u host-status" >&2
      exit 1
    fi
  done
  echo "ok: http://$loopback/"
fi
if [ -n "$tailscale_port" ]; then
  address="$(ip -4 -o addr show dev tailscale0 2>/dev/null | awk '{print $4}' | cut -d/ -f1 | head -n 1)"
  if [ -n "$address" ]; then
    curl -fsS --max-time 5 -o /dev/null "http://$address:$tailscale_port/" && echo "ok: http://$address:$tailscale_port/"
  fi
  if ss -H -lnt "sport = :$tailscale_port" | awk '{print $4}' | grep -qE '^(0\.0\.0\.0|\*|\[::\]):'; then
    echo "a porta $tailscale_port ficou aberta em todas as interfaces; parando" >&2
    systemctl stop host-status.service
    exit 1
  fi
fi
systemctl --no-pager --lines=0 status host-status.service | head -n 3
