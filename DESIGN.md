# Design system do painel de status do Mac

Fonte de verdade para a interface em `www/`. Toda decisão visual vem de um token abaixo, de uma referência do quadro ou da camada de identidade. O resto é padrão do agente e sai. Skill: `reference-driven-design`.

Estado em 2026-11-12: implementado em `www/app.css` e `www/app.js`, embutidos no binário. Cada seção foi conferida contra a referência do seu papel, em 1280 e 390 px, nos temas escuro e claro.

## Quadro de referências

| Ref | Fonte | Papel | Consultada |
| --- | --- | --- | --- |
| A | [Beszel, página de sistema](https://beszel.dev/image/system-full.png) | layout, cabeçalho do host, grade de gráficos, tema escuro | 2026-11-11 |
| B | [Stats (macOS), popups](https://cdn.mac-stats.com/assets/images/popups.png) | densidade e o bloco "Agora" (seções com rótulo entre fios, linhas rótulo/valor com ponto de cor) | 2026-11-11 |
| C | [Vercel Observability](https://7nyt0uhk7sse4zvn.public.blob.vercel-storage.com/docs-assets/static/docs/concepts/observability/O11y-Tab-Light.png) | tabelas (cabeçalho cinza, divisórias, barra de proporção atrás do número), tema claro | 2026-11-11 |

Conflito resolvido: tema. A é escuro e C é claro; o painel segue `prefers-color-scheme`, escuro com os valores de A e claro com os de C. Decisão do agente, o dono ainda não escolheu tema.

## Medidas (pixels amostrados, não estimativa, salvo indicação)

A, escuro (imagem em 1x):
- canvas `#161718`; card `#191A1C`; borda do card 1px `#222224`; margem externa 17px; espaço entre cards 16px; padding interno ~24px (estimado).
- título do card `#F7F7F8`, ~20px semibold (altura de caixa alta 22px); subtítulo `#A8AAAD` ~14px; rótulos de eixo `#94969A` ~12px; linha de grade `#202123`.
- gráfico de área: preenchimento `#213768`, linha `#2D5ED2`.
- cabeçalho do host: nome `#F7F7F8` ~24px semibold; linha de metadados `#E1E1E2` ~14px com ícone, itens separados por fio vertical `#525356` de 16px; ponto de estado "Up" `#43A461`.
- seletor de período no canto direito do cabeçalho.

C, claro (imagem em 2x; valores divididos por 2):
- canvas `#FAFAFA`; card `#FFFFFF`; borda 1px `#E8E8E8`; raio ~12px (estimado).
- título `#171717`; texto secundário e cabeçalho de tabela `#666666`; eixo `#8F8F8F`; grade `#E6E6E6`.
- linha `#0070F3`, preenchimento `#E5F1FE`; barra de proporção `#F2F2F2` atrás do número; legenda com ponto de 8px.

B, módulos de "Agora" (popups do Stats):
- um painel por módulo (CPU, Memória, Disco, Rede) com título centralizado ~15px semibold;
- anel de uso de ~84px, traço 8px, trilho cinza, partes coloridas por componente (CPU: usuário/sistema; RAM: app/wired/comprimida), valor no centro ~17px semibold;
- seções separadas por rótulo curto em caixa alta cinza, centrado entre dois fios ("DETALHES", "CARGA MÉDIA");
- linhas rótulo à esquerda e valor à direita, ~22px, com marcador **quadrado** de 9px e raio 2px quando a linha é uma série;
- barra empilhada da composição da memória; pílula de estado sólida com texto branco em caixa alta ("UP"/"No");
- rede: dois valores grandes lado a lado com unidade menor e legenda com marcador embaixo.

## Identidade

| Movimento | Origem | Função | Onde |
| --- | --- | --- | --- |
| Comando colável: `$ comando` em monoespaçada, fundo de superfície | o painel é de máquina que se opera por SSH | marca o que dá para rodar (túnel, `acari guide`, `harden.sh`) | só em dicas de ação; no máximo uma por card |

## Tokens (implementados em `www/app.css` como variáveis CSS)

| Token | Escuro (A) | Claro (C) | Notas |
| --- | --- | --- | --- |
| bg | `#161718` | `#FAFAFA` | |
| surface | `#191A1C` | `#FFFFFF` | |
| border | `#222224` | `#E8E8E8` | decorativa; controles usam border-strong |
| border-strong | `#525356` | `#8F8F8F` | select e foco de borda; ≥3:1 |
| text | `#F7F7F8` | `#171717` | |
| text-2 | `#A8AAAD` | `#666666` | |
| text-3 (eixo) | `#94969A` | `#767676` | claro: `#8F8F8F` de C tem 3,3:1; ajustado para 4,5:1 |
| grid | `#202123` | `#E6E6E6` | |
| series-line | `#2D5ED2` | `#0070F3` | |
| series-fill | `#213768` | `#E5F1FE` | |
| series-2 | `#E8833A` | `#D9661F` | sistema, wired, saída de rede |
| series-3 | `#9B7BE0` | `#7C5CCF` | memória comprimida (macOS), espera de E/S (Linux) |
| track | `#2C2E31` | `#E8E8E8` | trilho do anel e da barra empilhada |
| bar | `#2C2E31` | `#F2F2F2` | proporção atrás do número; no escuro A não tem barra, `#222224` sumia |
| ok | `#43A461` | `#43A461` | só no ponto; o texto diz o estado |
| space | 16px entre cards, 24px dentro | igual | |
| radius | 12px | 12px | estimado |
| font | `Inter, system-ui, -apple-system, sans-serif` | igual | sem CDN; no Mac cai na SF, mesma classe |
| números | `font-variant-numeric: tabular-nums` | igual | |

## Regras

MUST
1. Cabeçalho do host em card próprio: nome, linha de metadados com fios verticais, ponto de estado "Ao vivo"/"Parado", seletor de período à direita (A).
2. Gráficos em grade de 2 colunas (1 abaixo de 760px), cada card com título, subtítulo de uma linha, eixo Y com 4–5 marcas, eixo X com horas, grade horizontal (A).
3. "Agora" são quatro módulos no estilo B (anel, seções entre fios, linhas com marcador quadrado). A mesma cor de série vale para o módulo e para o gráfico correspondente (entrada = series, saída = series-2).
4. Tabelas no estilo C: cabeçalho em text-2, divisória de 1px, número alinhado à esquerda dentro de uma barra de 96px que cresce com o valor, quando a coluna é contagem. Abaixo de 760px cada linha vira bloco: a primeira célula é o título, as outras têm o rótulo da coluna.
6. Gráficos desenhados na largura real do card (ResizeObserver); a margem do eixo Y acompanha o rótulo mais largo. Nunca esticar o SVG.
5. Estado sempre com texto; cor só no ponto.

MUST NOT
1. Sombras, gradientes, vidro, cards coloridos por significado.
2. Cor de série fora de gráficos, anéis e marcadores; vermelho fora de alerta.
3. Caixa alta espaçada fora do bloco "Agora".

## Desvios
- B usa vermelho para "sistema" e "comprimida"; aqui vermelho é só alerta, então essas séries usam laranja e violeta.
- Pílula de alerta `#D93036` com texto branco (4,6:1) em vez do vermelho mais claro do Stats.
- Eixo no claro: `#8F8F8F` → `#767676` (4,5:1 sobre branco).

## Telas fora do papel das referências
- Nenhuma. A página é uma só. "Aparelhos na rede" (Linux) segue as tabelas de C.
- "Leitura" (conclusões tiradas dos registros) e "Quem entrou" (SSH) seguem as tabelas de C e o mesmo formato estado/item/detalhe das proteções; problema e atenção vêm primeiro.

## Checklist
- [ ] Toda cor, tamanho e raio vem de token.
- [ ] Comparado lado a lado com A, B e C na mesma largura (1280 e 390).
- [ ] Contraste, foco visível, rótulos, `prefers-reduced-motion`.

## Histórico
- 2026-11-11: referências A, B e C escolhidas; tokens medidos; página reestilizada.
- 2026-11-12: página movida para o host-status (Rust). Módulos genéricos por sistema: partes de CPU e memória vêm do coletor; temperatura, internet, fail2ban e aparelhos na rede no Linux.
- 2026-11-11: primeira versão rejeitada pelo dono (três linguagens desconectadas, validação só do topo). Refeita: módulos de B fiéis ao Stats, cores de série compartilhadas entre módulos e gráficos, SSH agrupado, acari em português, revisão da página inteira.
- 2026-11-13: card "Leitura" abaixo do cabeçalho e tabela "Quem entrou" no SSH, conferidos em 1280 (claro e escuro) e 390.
