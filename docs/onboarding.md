# Onboarding

Sequência completa: `welcome → profile → language → permissions → audio-setup →
ai-provider → onboarding-review → ready`. `cloud-login` é um desvio a partir de
`welcome` (não conta como etapa — sem ponto próprio em `ProgressDots`). Progresso
mostrado como pontos discretos (`components/ui/ProgressDots.tsx`), nunca como "Etapa 4
de 8" em destaque. Cada etapa (`components/layout/OnboardingLayout.tsx`) segue a mesma
forma: título curto, descrição de até duas linhas, conteúdo central, uma ação principal
(`PrimaryButton`) e ações discretas (`GhostButton`) — nunca duas ações
dominantes na mesma tela.

Toda a lógica de navegação (o que "Voltar"/"Continuar" fazem em cada etapa) vive em
`src/app/router.tsx`, não dentro das telas — ver `docs/frontend-architecture.md`.

## welcome (`features/welcome/WelcomeScreen.tsx`)

Mark + "Respostas melhores, no momento certo." + uma linha de apoio. "Continuar sem
login" é a única ação dominante; "Entrar" é um `GhostButton` abaixo, não uma segunda
ação com o mesmo peso visual. Sem lista de benefícios, sem cards — a tela de maior
restrição do fluxo inteiro, de propósito.

## cloud-login (`features/welcome/CloudLoginScreen.tsx`)

Alcançado só pelo "Entrar" acima. "Helppye Cloud está chegando" + "Nesta versão, você
já pode usar todos os recursos locais sem criar uma conta." — um beco honesto, não um
formulário de email/senha sem backend por trás. `[Continuar sem login]` leva para o
mesmo lugar que o botão principal de `welcome` levaria.

## profile (`features/profile/ProfileScreen.tsx`)

"Como podemos chamar você?" + "Isso ajuda o Helppye a personalizar sua experiência."
Campo de nome, `[Pular]` (`GhostButton`, ao lado de "Voltar") e `[Continuar]`
(`PrimaryButton`). Nada sobre speakers, contexto de conversa ou qualquer outro conceito
técnico nesta etapa — é só um nome.

## language (`features/language/LanguageScreen.tsx`)

"Qual idioma você usa nas conversas?" com um único item selecionável, elegante
("Português — Brasil", marcado com um ícone de check), não um `<select>` HTML com uma
opção só. `useOnboardingStore.language` já modela isso como uma união extensível para
quando mais idiomas existirem.

## permissions (`features/permissions/PermissionsScreen.tsx`)

"Precisamos ouvir a conversa" — dois itens (`Sua voz` / microfone, `A outra pessoa` /
áudio do sistema), cada um com ícone, descrição de uma linha, estado ("Permitido" ou o
botão "Permitir") e, só em erro, um link "Saiba mais" que expande detalhes de
plataforma (WASAPI/PipeWire/permissões do SO).

**Não existe um comando Rust dedicado a "checar permissão"** — tentar iniciar a captura
*é* a checagem: é exatamente o gatilho que faria o SO pedir permissão numa máquina
real, e falha do mesmo jeito que uma permissão negada falharia. `Permitir` chama
`start_microphone_capture_command`/`start_system_audio_capture_command`
(`hooks/useAudioCapture.ts`) e reflete o resultado real (`audio://capture-event`), sem
simular sucesso.

## audio-setup (`features/audio-setup/AudioSetupScreen.tsx`)

Um teste guiado, não um formulário de dispositivos: "Vamos testar o áudio — Fale
alguma coisa e reproduza um som no computador." com dois blocos (`DeviceTestBlock`,
compartilhado com a seção "Áudio" de Configurações), cada um mostrando nome do
dispositivo, barra de nível ao vivo e status textual (`Ouvindo`/`Sem sinal`/`Trocando
dispositivo...`/`Dispositivo desconectado`/`Sem permissão`) — nunca um ID interno.

**Antes de mostrar o teste**, esta tela verifica se o modelo de transcrição local está
pronto (`features/audio-setup/ModelPrepareStep.tsx`, usando `hooks/useModelStatus.ts`).
Não é uma etapa própria no fluxo — o antigo "gate" que bloqueava o app inteiro antes de
mostrar qualquer coisa (incluindo `welcome`) foi dobrado para dentro desta etapa, porque
é o primeiro ponto do fluxo em que transcrição realmente importa. O download **nunca**
começa sozinho: só após o clique explícito em "Baixar e continuar", igual ao
comportamento anterior (ver `CLAUDE.md`).

## ai-provider (`features/ai-provider/AiProviderScreen.tsx`)

"Escolha como gerar respostas" — quatro cartões (`ProviderOption`): Ollama ("Local e
privado", badge "Recomendado"), OpenAI ("Rápido e preciso"), Anthropic ("Respostas
naturais"), DeepSeek ("Alternativa em nuvem"). Cada cartão mostra só nome, descrição de
uma linha e badge — nenhuma menção a endpoint, streaming ou limite de tokens nesta
visão.

Selecionar um cartão revela o painel daquele provedor logo abaixo:

- **Ollama** (`features/ai-provider/OllamaPanel.tsx`): status real de conexão — uma
  checagem HTTP direta do frontend contra `http://localhost:11434/api/tags`
  (`services/ollamaService.ts`), possível porque o CSP do app
  (`src-tauri/tauri.conf.json`) já libera esse endereço para o próprio pipeline de
  streaming. Quando conectado, "Alterar modelo" mostra os modelos de fato instalados
  (não um campo de texto às cegas). URL do servidor e `keep_alive` ficam atrás de
  "Configuração avançada".
- **OpenAI/Anthropic/DeepSeek** (`features/ai-provider/CloudProviderPanel.tsx`):
  "Conecte sua conta usando uma API key" + campo mascarado + `[Conectar]`. Depois de
  salva: "Chave configurada · Modelo: ..." + `[Remover]`. Não existe um botão "Testar
  conexão" para provedores de nuvem — o CSP do app não libera os domínios deles para
  fetch direto do frontend (só Ollama, local, está liberado), e fabricar um "conectado"
  sem checagem real seria pior do que não ter o botão. A chave nunca fica em estado do
  componente por mais tempo que o necessário para chamar
  `response_set_api_key_command`, que a grava no keychain do SO.

## onboarding-review (`features/onboarding-review/OnboardingReviewScreen.tsx`)

"Está tudo certo" — quatro linhas curtas (idioma, microfone, saída de áudio, provedor ·
modelo), cada uma com um ícone de edição que leva direto à etapa correspondente
(`onEdit`, via `app/router.tsx`), não uma volta genérica ao início do fluxo. Sem cards
grandes, sem repetição de descrições já mostradas nas etapas anteriores.

## ready (`features/ready/ReadyScreen.tsx`)

Tela de pouso para toda visita de retorno (`resolveInitialScreen` manda direto para cá
sempre que `onboardingComplete` é verdadeiro — ver `docs/frontend-architecture.md`).
"Tudo pronto" (ou "Tudo pronto, {nome}" quando o nome foi preenchido) + `[Começar
sessão]` como única ação dominante, com a dica "⌘/Ctrl D para iniciar rapidamente"
logo abaixo. Uma linha secundária discreta no rodapé mostra provedor · modelo ·
idioma · status do áudio — reconhecendo que a configuração existe, sem competir com o
botão principal. Configurações fica atrás de um ícone discreto no canto, não de um
botão de texto.
