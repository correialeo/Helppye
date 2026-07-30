# Design system

Esta página documenta a linguagem visual do Helppye: de onde ela vem, o que ela é
concretamente (tokens, componentes), e o que foi deliberadamente escondido da
experiência principal. Para a estrutura de código que implementa isto, ver
`docs/frontend-architecture.md`; para o fluxo de onboarding tela-a-tela, ver
`docs/onboarding.md`; para a experiência de sessão ao vivo, ver
`docs/session-experience.md`.

## Referência de produto

O Perssua (Lucas Montano) foi usado como referência de **princípios** de UX — não como
fonte a copiar. Nada de logo, nome, slogan, textos, paleta exata, assets, componentes
idênticos, estrutura pixel a pixel, código ou imagens do Perssua está neste projeto; o
Helppye tem identidade visual própria (paleta, mark, textos, componentes construídos do
zero). O que foi *reinterpretado* — a categoria de experiência, não a aparência
específica — é:

- **Onboarding extremamente simples**: uma decisão por tela, nunca um formulário longo.
  Ver `docs/onboarding.md`.
- **Baixa carga cognitiva**: cada tela responde "existe informação demais aqui?" com
  não — o que sobra vira configuração avançada, modo de desenvolvedor, ou é removido.
- **Aparência de produto desktop nativo**: sem chrome de dashboard SaaS, sem sidebar,
  sem grade de cards.
- **Janela de sessão compacta e elegante**: ver §Janela de sessão abaixo.
- **Configuração técnica escondida atrás de ações secundárias**: ver
  §Complexidade ocultada.
- **Foco na sugestão da IA, não na infraestrutura**: a única coisa "dominante" na tela
  de sessão é o texto sugerido.

Não declaramos em nenhum lugar que o Helppye "é igual" ao Perssua — só que ele foi usado
como referência de princípios, com implementação e identidade inteiramente próprias.

## Personalidade

Limpa, silenciosa, inteligente, confiável, discreta, moderna, minimalista, premium,
focada, rápida. Deliberadamente evitando: dashboard SaaS genérico, painel
administrativo, ferramenta de desenvolvedor à mostra, aplicativo cheio de cards, chat
genérico, IDE, terminal, painel de observabilidade — essa última categoria inteira
(turnos, latência, eventos brutos) existe, mas só dentro de "Modo de desenvolvedor" (ver
§Complexidade ocultada), nunca na experiência normal.

## Paleta

Hierarquia de superfícies em quase-preto, definida como variáveis CSS
(`src/index.css`) e exposta ao Tailwind como `bg-app`/`bg-surface`/`bg-surface-raised`
(`tailwind.config.js`):

| Token | Valor | Uso |
|---|---|---|
| `app` | `rgb(9 9 11)` | fundo da janela |
| `surface` | `rgb(17 17 20)` | cards, inputs, blocos |
| `surface-raised` | `rgb(26 26 31)` | popovers, modais, hover de superfície |

Texto: `text-neutral-100`/`-50` para texto principal (nunca branco puro — ver `body`
em `index.css`, `antialiased` + `text-neutral-100`), `text-neutral-400`/`-500` para
texto de apoio.

**Uma única cor de destaque** — `brand` (`tailwind.config.js`), um violeta próprio
(`#7C5CFC` como `brand-500`), deliberadamente distinto do "indigo" genérico do
Tailwind. Nenhuma tela usa uma segunda cor de destaque competindo com ela.

Bordas/divisores usam `border-white/8` a `border-white/20` (opacidade sobre branco, sem
token dedicado — ver comentário em `tailwind.config.js`) em vez de uma cor sólida
própria: é exatamente "baixo contraste sobre fundo escuro" de graça, sem indireção
extra.

Cores semânticas (sucesso/atenção/erro) são discretas por construção —
`components/ui/InlineNotice.tsx` e `components/feedback/StatusIndicator.tsx` usam
`emerald`/`amber`/`red` sempre em preenchimento de baixa opacidade (`bg-emerald-500/8`,
não `bg-emerald-500`), nunca como bloco saturado.

### Gradientes

Reservados para exatamente quatro lugares, de propósito — usar um gradiente fora desta
lista é um desvio do sistema, não uma opção de estilo:

1. Botão principal (`PrimaryButton` usa preenchimento sólido `bg-brand-600` hoje —
   reservado para uma versão futura com leve gradiente, se necessário).
2. Brilho de fundo (`shadow-glow-brand` em `BrandMark`).
3. Estado de geração (o cursor pulsante em `SuggestionPanel` durante streaming).
4. Detalhe de identidade (`BrandMark`).

Medidores de nível de áudio e barras de progresso (`AudioLevelMeter`, `ProgressBar`)
usam preenchimento **sólido**, não gradiente — não estão nessa lista.

## Profundidade

`shadow-soft`/`shadow-raised` (`tailwind.config.js`) são sombras suaves de baixo
contraste, não glassmorphism — nenhum componente usa `backdrop-blur` como efeito
decorativo. O único blur do app é o backdrop nativo de `<dialog>`
(`[&::backdrop]:bg-black/60` em `components/ui/Dialog.tsx`), funcional (separar o modal
do conteúdo por trás), não estético.

## Tipografia

Títulos curtos (`StepHeader` — `text-xl font-semibold`), descrição de apoio de uma ou
duas linhas no máximo (`text-sm text-neutral-400`), largura de leitura controlada
(`max-w-xs`/`max-w-sm` em toda tela de onboarding). Nenhuma tela de onboarding tem um
parágrafo — ver `docs/onboarding.md` para o texto exato de cada etapa.

## Ícones

`lucide-react` — a única dependência visual nova adicionada nesta reformulação (ver
"Por que uma biblioteca de ícones" abaixo). Ícones nunca são a única forma de comunicar
um estado: todo ícone de status vem acompanhado de um rótulo em texto
(`StatusIndicator`, `IconButton` exige `aria-label`). Nenhum emoji é usado como elemento
de interface.

### Por que uma biblioteca de ícones

A instrução de não instalar "uma biblioteca visual grande" mira frameworks de
componentes completos (MUI, Chakra, Ant...) que trariam opinião própria de layout,
tema e comportamento por cima do sistema deste documento. `lucide-react` é o oposto
disso: cada ícone é um componente SVG independente e tree-shakeable — importar
`Mic`/`Settings`/`Copy` não traz nenhum componente de UI, tema ou runtime extra junto.
A alternativa (desenhar ~20 SVGs à mão) trocaria uma dependência pequena e bem
mantida por um conjunto de assets frágil e sem consistência visual entre si.

## Componentes (`src/components/`)

`ui/`: `PrimaryButton`, `SecondaryButton`, `GhostButton`, `IconButton`, `TextInput`,
`PasswordInput`, `Select`, `Toggle`, `ProgressDots`, `StepHeader`, `InlineNotice`,
`Dialog`, `Tooltip`, `Kbd`, `BrandMark`, `ProviderOption`, `DeviceOption`.
`layout/`: `OnboardingLayout`, `AppShell`. `feedback/`: `StatusIndicator`,
`AudioLevelMeter`, `ProgressBar`. Sessão-específicos (`SuggestionPanel`,
`SessionHeader`, `SessionFooter`, `TranscriptPeek`, `TranscriptDrawer`) vivem em
`features/session/` — não são primitivos reutilizáveis fora dali.

Nenhum destes é construído sobre um "design system" abstrato (sem `cva`, sem tema
runtime, sem tokens indiretos além do que o Tailwind já oferece) — são componentes
React simples com props explícitas, seguindo a instrução de não introduzir abstração
além do necessário.

## Animações

150–250ms em toda transição funcional (`tailwind.config.js`
`transitionDuration.DEFAULT: "180ms"`, `animation.rise-in: "220ms"`,
`animation.fade-in: "200ms"`) — mudança de tela, abertura de modal, seleção,
feedback de cópia. `@media (prefers-reduced-motion: reduce)` (`src/index.css`) zera
duração/iteração de toda animação e transição incondicionalmente — não é opt-in por
componente, é uma regra global.

## Janela de sessão

Redimensionada (`hooks/useWindowMode.ts`) para ser visivelmente mais compacta que o
resto do app: ~380×620px lógicos contra ~420×760px do app principal, com tamanho
mínimo próprio (~320×420px) para continuar utilizável quando o usuário encolhe a
janela manualmente. Estrutura de cima para baixo: `SessionHeader` (mark + status +
menu — a barra mais fina do app), a última fala da outra pessoa (`TranscriptPeek`,
secundária: `text-sm text-neutral-400`, `line-clamp-2`), a sugestão
(`SuggestionPanel`, ocupa o espaço restante, `text-[15px]` — a única coisa em destaque
tipográfico real na tela), `SessionFooter` (dois indicadores de status + cronômetro).
Sem timeline completa visível por padrão (ver `TranscriptDrawer`, sob demanda), sem
menu lateral, sem diagnóstico visível — ver §Complexidade ocultada.

## Complexidade ocultada

Removido da experiência principal inteiramente (não existe mais em nenhuma tela
comum): a UI de debug original em `App.tsx` (painéis `<details>` de "Turnos
consolidados"/"Diagnóstico de sugestão de resposta" sempre presentes, gated só por
`import.meta.env.DEV`).

Movido para telas secundárias, sempre acessíveis mas nunca no caminho principal:

- **Configurações avançadas de provedor** (URL do Ollama, `keep_alive`) — atrás de
  `<details>` "Configuração avançada" em `OllamaPanel`, nunca na visão principal do
  cartão do provedor.
- **Modelo do provedor de nuvem** — campo com placeholder, preenchido só se o usuário
  quiser mudar do padrão recomendado.
- **IDs de dispositivo de áudio, sample rate, canais** — nunca mostrados; só o nome do
  dispositivo (`DeviceOption`).

Movido para "Modo de desenvolvedor" (`Settings` → toggle → `DeveloperToolsScreen`,
desligado por padrão, nunca altera o layout normal enquanto desligado — ver
`docs/frontend-architecture.md` §Ferramentas de desenvolvedor):

- turn IDs, utterance IDs, revisões, contagem de segmentos;
- `finalization_reason`, `gap_ms_used`, `silence_detected_ms`;
- `http_status`, `event_emitted`, `cancel_reason`, `raw_prefix` (prefixo bruto da
  resposta do provedor, antes do `SkipDetector`);
- toda a decomposição de latência
  (`utterance_finalized_to_request_started_ms`,
  `request_to_first_http_chunk_ms`, `request_to_first_visible_token_ms`,
  `end_of_speech_to_first_visible_token_ms`);
- o controle de `same_speaker_utterance_gap_ms` (antes um card sempre visível em modo
  dev no `App.tsx` antigo);
- "Copiar diagnóstico" (serializa turnos/utterances/diagnósticos correntes como JSON).

## Acessibilidade

- **Teclado**: todo controle é um elemento real (`<button>`, `<input>`), nunca uma
  `<div onClick>`; `Select`/`SessionHeader`'s menu implementam navegação por seta/Enter/
  Escape; `Dialog` usa `<dialog>` nativo (foco preso, Escape fecha, foco retorna ao
  elemento que abriu o modal automaticamente).
- **Rótulos**: `IconButton` exige `aria-label`; `TextInput`/`PasswordInput` sempre
  renderizam um `<label htmlFor>` real quando `label` é passado.
- **Foco visível**: um único anel de foco consistente (`:focus-visible` em
  `src/index.css`), inclusive em controles customizados.
- **`prefers-reduced-motion`**: respeitado globalmente, não por componente.
