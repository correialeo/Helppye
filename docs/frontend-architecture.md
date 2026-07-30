# Arquitetura do frontend

O frontend deixou de ser um único `App.tsx` de ~1300 linhas concentrando onboarding,
configuração técnica e a timeline de debug. Esta página documenta a estrutura atual,
sob `src/`; para os princípios visuais que motivaram a reorganização (não só a
reorganização em si), ver `docs/design-system.md`. Para o fluxo de onboarding
tela-a-tela, ver `docs/onboarding.md`.

## Layout de pastas

```
src/
  app/              App.tsx (init/providers/error boundary), router.tsx (navegação),
                    appFlow.ts (AppScreen + lógica pura de sequência/resumo),
                    ErrorBoundary.tsx
  components/
    ui/             Primitivos genéricos: PrimaryButton, SecondaryButton, GhostButton,
                    IconButton, TextInput, PasswordInput, Select, Toggle, ProgressDots,
                    StepHeader, InlineNotice, Dialog, Tooltip, Kbd, BrandMark,
                    ProviderOption, DeviceOption
    layout/         OnboardingLayout (shell de cada etapa do onboarding), AppShell
                    (shell de ready/settings/dev tools)
    feedback/       StatusIndicator, AudioLevelMeter, ProgressBar
  features/         Uma pasta por tela/domínio: welcome, profile, language,
                    permissions, audio-setup, ai-provider, onboarding-review, ready,
                    session, settings, developer-tools
  hooks/            useAudioCapture, useModelStatus, useResponseProvider,
                    useConversationTimeline, useResponseSuggestions, useOllamaProbe,
                    useKeyboardShortcuts, useWindowMode
  services/         Wrappers finos e tipados sobre `invoke`/`listen` — nenhuma tela
                    chama `invoke("nome_cru_do_comando")` diretamente
  stores/           useOnboardingStore (Zustand + persist), useAudioCaptureStore
                    (Zustand, em memória)
  types/            Tipos espelhando os estados/DTOs do Rust: audio, conversation,
                    model, responseProvider
  utils/            cx, format (bytes/segundos/tempo), audio (dBFS)
```

`App.tsx` só faz quatro coisas, de propósito: resolve a tela inicial a partir do que foi
persistido (`appFlow.resolveInitialScreen`), monta os providers globais
(`AudioCaptureProvider`), aplica o tamanho de janela conforme a tela
(`useWindowMode`) e envolve tudo num `ErrorBoundary`. Toda a navegação real vive em
`app/router.tsx`; toda a aparência de cada tela vive em `features/`.

## Estado principal — `AppScreen`

```ts
export type AppScreen =
  | "welcome" | "cloud-login" | "profile" | "language" | "permissions"
  | "audio-setup" | "ai-provider" | "onboarding-review"
  | "ready" | "session" | "settings";
```

Nenhuma navegação usa booleanos soltos (`showSettings`, `isInSession`, ...) — o app
inteiro está sempre em exatamente um desses onze estados, armazenado em
`useOnboardingStore.screen`. `app/router.tsx` é um `switch` sobre esse valor; cada
`case` só passa callbacks de navegação para a tela, nunca lógica de navegação para
dentro dela.

### Ferramentas de desenvolvedor não são um `AppScreen`

`DeveloperToolsScreen` é deliberadamente **não** um dos onze valores acima — é uma
sobreposição de tela cheia controlada por um `useState` local em `AppRouter`
(`developerToolsOpen`), independente da tela ativa por baixo. Isso permite abri-la tanto
a partir de "Configurações" quanto do menu da sessão (⋯ → "Abrir diagnóstico") sem
precisar que `AppScreen` cresça para acomodar um estado que, na prática, é um modal em
tela cheia sobre a tela atual — voltar simplesmente fecha a sobreposição e devolve
exatamente à tela que estava por baixo, sem precisar lembrar "de onde vim".

## Persistência

`useOnboardingStore` usa o middleware `persist` do Zustand sobre `localStorage`
(`stores/useOnboardingStore.ts`), com uma allowlist explícita em `partialize` — só isto
é escrito em disco:

- `onboardingComplete`
- `screen` (a etapa atual, para retomar um onboarding incompleto de onde parou)
- `userName`
- `language`
- `devMode`

**Nunca persistido aqui:** seleção de dispositivo de áudio e configuração de provedor de
IA (modelo, `base_url`, `keep_alive`) — esses já são persistidos no lado Rust
(`device_selection.json` via `resolve_device_selection_command`/
`select_input_device_command`/`select_output_device_command`; `response_provider.json`
via `response_set_provider_config_command`) e são lidos de lá a cada montagem
(`hooks/useAudioCapture.ts`, `hooks/useResponseProvider.ts`), não duplicados no
`localStorage` — duas fontes de verdade para o mesmo dado divergem mais cedo ou mais
tarde. **Nunca, em hipótese alguma:** API keys de provedores de nuvem. Elas só existem
em memória pelo tempo mínimo entre o usuário digitar e `services/responseProviderService.ts`
chamar `response_set_api_key_command`, que as grava no keychain do SO — nunca em texto
puro em disco, nunca em `localStorage`, nunca em log.

## Estado de captura de áudio é global, não por tela

`useAudioCaptureStore` (sem `persist` — vive só enquanto o app está aberto) é
deliberadamente compartilhado entre `permissions`, `audio-setup`, `session` e
`settings`, em vez de estado local por componente como na versão anterior
(`DeviceCapturePanel`). Motivo: essas quatro telas mostram/controlam as mesmas duas
fontes de captura, e reassinar um listener `audio://capture-event` novo a cada
montagem de tela perderia o evento `started` disparado por uma tela anterior. Uma única
assinatura global (`hooks/useAudioCapture.ts` `AudioCaptureProvider`, montada uma vez em
`App.tsx`) mantém a store atualizada; cada tela só lê dela via `useAudioCapture(source)`.

## Sessão: início/fim de captura

`app/router.tsx` centraliza a orquestração: `startSession` para qualquer captura
residual, inicia as duas fontes do zero e só então navega para `"session"`; `endSession`
para as duas fontes, encerra a sessão da Conversation Timeline
(`conversation_end_session_command`) e volta para `"ready"`. A tela `"ready"` nunca tem
captura ativa por baixo — abrir configurações ou navegar por ali não deixa microfone/
áudio do sistema capturando em segundo plano sem o usuário estar numa sessão.

## Redimensionamento de janela

`hooks/useWindowMode.ts` redimensiona a janela via `@tauri-apps/api/window`
(`getCurrentWindow().setSize()`/`setMinSize()`) conforme a tela: um tamanho para o app
principal (onboarding/ready/settings) e um tamanho mais compacto para `"session"` — ver
`docs/design-system.md` §Janela de sessão. É só uma chamada de API já existente do
Tauri, não uma mudança de backend; a única mudança de configuração necessária foi
adicionar `core:window:allow-set-size`/`allow-set-min-size` a
`src-tauri/capabilities/default.json`, já que o conjunto de permissões padrão
(`core:default`) só inclui consultas de leitura sobre a janela.

## Um comando novo no backend, e por quê

O resto desta reformulação é 100% frontend (nenhuma mudança de lógica em
`ResponseEngine`, pipeline de áudio, transcrição, VAD ou agrupamento de conversa), com
uma exceção: `conversation_regenerate_suggestion_command`
(`src-tauri/src/conversation.rs`). O botão "Regenerar" e o atalho
Ctrl/Cmd+Shift+Enter (seção 20/24 do pedido original) exigem uma forma de disparar
manualmente uma nova geração — algo que não existia antes (a geração só disparava
automaticamente via `UtteranceFinalized`). O comando novo não introduz lógica de
geração: ele só localiza o turno pelo ID já visível no frontend, confirma elegibilidade
com `is_eligible_turn` (já pública) e chama `ResponseEngine::trigger_generation` — a
mesma função que `process_conversation_events` já chama automaticamente, com um
`GenerationTrigger` sintético (`finalization_reason: "manual_regenerate"`) para que o
diagnóstico continue distinguindo a origem de cada geração.
