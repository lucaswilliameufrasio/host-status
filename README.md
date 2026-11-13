# host-status

Um binário que coleta métricas, acessos SSH e checagens de proteção de uma máquina macOS ou Linux e serve uma página de status. Ele substituiu os painéis em Python do `personal-compute-engines`.

## O que mostra

- **Agora**, atualizado a cada 5 s: CPU (usuário, sistema, espera de E/S no Linux, temperatura quando há sensor), carga, memória (Ativa/Wired/Comprimida no macOS, Usada/Cache no Linux), swap, disco, rede, Tailscale e internet (gateway, WAN, DNS).
- **Histórico** de 1 h e 24 h: CPU, memória empilhada, carga, rede.
- **Acesso remoto e proteção**: `sshd` (senha, root, `AllowUsers`), firewall, fail2ban, atualizações automáticas e pendentes, reinício pendente; no macOS, compartilhamento de tela, Remote Apple Events, FileVault, SIP e apps de acesso remoto. Mais os serviços declarados na configuração, checados por porta.
- **SSH**: entradas, falhas e conexões sem login, agrupadas, com origem e chave; origens dos últimos 7 dias; sessões abertas.
- **Leitura**: conclusões tiradas dos registros, com estado. Quem entrou por SSH em 7 dias (todas por chave? de quais origens e redes? com quais aparelhos, pelo comentário da chave em `authorized_keys`), tentativas sem login em 24 h (quantas, de quantos endereços, os mais insistentes e os usuários mais tentados, pesadas contra senha liberada e fail2ban), serviços do systemd em falha, processos mortos por falta de memória, portas novas abertas para qualquer rede e aparelhos novos na rede.
- **Processos**, **portas abertas** fora do loopback, **aparelhos na rede** (Linux, opcional: fping + ARP, fabricante pelo `ieee-data`) e o resumo do [acari](https://github.com/lucaswilliameufrasio/acari) quando instalado.

## Consumo

Medido em 2026-11-12 com o serviço rodando:

| Máquina | Memória (RSS) | CPU |
| --- | --- | --- |
| Mini PC Atom x5, Debian 13 | 8,0 MB | menos de 1% |
| Mini PC Atom x5, Ubuntu 22.04 | 7,5 MB | menos de 1% |
| Desktop, Ubuntu 24.04 | 2,8 MB | 0,3% |
| Mac mini M2 Pro (macOS 27.2) | 12–14 MB (o macOS conta páginas compartilhadas do sistema) | 0% |

A amostra de 5 s lê o kernel direto (`/proc`, `sysctl`, `host_statistics`, `getifaddrs`) e não abre processos; no macOS, só a rota padrão sai de um comando, uma vez por minuto. A coleta completa roda numa thread à parte a cada minuto e é pulada se a anterior ainda estiver rodando. A checagem de atualizações do apt roda a cada 6 h, em segundo plano.

## Configuração

JSON pequeno, todos os campos opcionais:

```json
{
  "name": "servidor-casa",
  "role": "Escritório",
  "lan_devices": true,
  "labels": { "00:00:5e:00:53:02": "Roteador" },
  "services": [{ "label": "go2rtc API", "port": 1984, "host": "tailscale" }],
  "bind": ["127.0.0.1:8787", "tailscale:80"],
  "acari": "/home/user/.local/bin/acari"
}
```

`tailscale` em `bind` e em `services` vira o IP da Tailscale da máquina; se ele mudar, o servidor refaz o bind sozinho. Campo desconhecido é erro. `host-status --config ARQUIVO --check-config` valida.

## Instalar

### Linux (systemd)

Numa pasta com o binário `host-status`, `deploy/host-status.service`, `deploy/install-linux.sh` e a configuração como `config.json`:

```bash
sudo bash install-linux.sh             # ou: su - -c 'bash /caminho/install-linux.sh'
sudo bash install-linux.sh --rollback
```

Roda como root, porque o journal do sshd, `sshd -T`, o fail2ban e o dono de cada porta só aparecem para root. O unit fecha o resto: sistema de arquivos só leitura (exceto `/var/lib/host-status`), sem novos privilégios, até 64 MB de memória e 20% de CPU. O instalador desliga o painel antigo em Python (timer e Caddy) quando encontra, libera a porta na `tailscale0` se o ufw estiver ativo (ou só para `100.64.0.0/10` no firewalld), e para o serviço se a porta abrir em todas as interfaces.

### macOS (LaunchAgent, sem sudo)

Numa pasta com o binário e `config.json`:

```bash
bash install-macos.sh
bash install-macos.sh --rollback
```

Instala em `~/.local/bin/host-status`, dados em `~/Library/Application Support/host-status/`, log em `~/Library/Logs/host-status.log`. Na primeira instalação importa o histórico do painel em Python.

Limites do macOS: o binário não é assinado pela Apple, e o `netstat` chamado por ele não lista sockets TCP. Por isso as portas vêm do `lsof` (só processos do usuário), mais SSH pelo estado do launchd e uma checagem de portas de serviços do sistema (Kerberos, SMB, AFP, ARD, VNC). As mensagens de autenticação do sshd ficam pouco tempo no log unificado; o coletor relê os últimos 10 minutos a cada minuto.

## Opções

```text
host-status [--config ARQUIVO] [--data PASTA] [--once] [--check-config] [--diagnose] [--version]
```

`--once` faz uma coleta completa e imprime o JSON. `--diagnose` mostra leituras cruas da plataforma.

Rotas: `/`, `/app.js`, `/app.css`, `/live.json` (5 s), `/current.json` (1 min). Só GET e HEAD, sem cache, com `Content-Security-Policy: default-src 'self'`.

## Construir

```bash
cargo test
mise exec -- cargo zigbuild --release --target x86_64-unknown-linux-musl   # Linux estático
mise exec -- cargo zigbuild --release --target aarch64-apple-darwin        # macOS Apple Silicon
```

`mise.toml` fixa o zig e o cargo-zigbuild. O alvo da Apple precisa de `rustup target add aarch64-apple-darwin`.

Dependências: tokio (runtime de uma thread) e hyper para HTTP, rusqlite (SQLite embutido), serde e serde_json, libc.

## Visual

A página segue [`DESIGN.md`](DESIGN.md): Beszel para cabeçalho e gráficos, Stats para os módulos de "Agora", Vercel para as tabelas.

## Licença

[Apache-2.0](LICENSE).
