# Sugestão de resposta em streaming (`src-tauri/src/response_provider/`)

Substitui a antiga detecção local de perguntas por regras (`question_detection.rs`,
removida). Em vez de apenas sinalizar que um turno da outra pessoa parece uma pergunta,
o pipeline atual gera, via LLM e em streaming, uma sugestão real de resposta para o
usuário — mantendo a filosofia local-first como padrão, mas permitindo que o usuário
escolha explicitamente um provedor de nuvem quando quiser.

## Visão geral do fluxo

```
Conversation Timeline (turns/utterances)
        │  UtteranceFinalized (turno elegível)
        ▼
ResponseEngine::trigger_generation
        │  cancela geração anterior do mesmo turno, se houver
        ▼
context::build_request (histórico limitado + turno atual)
        │
        ▼
ResponseProvider ativo (Ollama | OpenAI | DeepSeek | Anthropic)
        │  stream de ResponseChunk::Delta/Done
        ▼
SkipDetector (decide [SKIP] vs. conteúdo real, sem segunda chamada ao LLM)
        │
        ▼
events::emit_response_suggestion_event → `response://suggestion-event` (frontend)
```

O gatilho é `ConversationTimelineEvent::UtteranceFinalized`, não o fim do turno inteiro:
assim que a pessoa termina de falar uma frase (não necessariamente de falar por
completo), já existe contexto suficiente para começar a gerar uma sugestão, o que ajuda
a manter a latência de ponta a ponta baixa. Se a pessoa continuar falando no mesmo turno
depois disso (nova utterance no mesmo turno), a geração em andamento é cancelada e uma
nova é iniciada — evitando sugerir uma resposta para uma fala que na verdade ainda não
tinha terminado.

## Elegibilidade

Igual à extinta detecção de perguntas: só turnos com `speaker = OtherPerson` e
`source = SystemOutput` disparam geração (`engine::is_eligible_turn`). O usuário nunca
recebe uma "sugestão de resposta" para a própria fala.

## Módulo por módulo

- **`provider.rs`** — abstração comum (`ResponseProvider`, `ResponseRequest`,
  `ResponseChunk::{Delta, Done}`, `ResponseProviderError`). Cada provedor devolve um
  stream de deltas de texto, nunca a resposta inteira de uma vez — é o que permite
  exibir a sugestão sendo digitada em tempo real em vez de esperar a resposta completa.
- **`context.rs`** — monta o `ResponseRequest` a partir do histórico de turnos e do
  turno atual. Teto de 6 turnos e 6000 caracteres de histórico (`MAX_HISTORY_TURNS`,
  `MAX_HISTORY_CHARS`), 300 tokens de saída (`MAX_OUTPUT_TOKENS`) — contexto limitado de
  propósito para manter prompt pequeno e latência baixa em vez de reenviar a conversa
  inteira a cada geração. O `SYSTEM_PROMPT` instrui o modelo a responder com o marcador
  fixo `[SKIP]` quando a fala mais recente não for uma pergunta/pedido que exija resposta
  — a mesma chamada que gera a resposta também decide se deve responder, sem uma segunda
  chamada de classificação.
- **`skip_detector.rs`** — `SkipDetector` consome os deltas do stream incrementalmente e
  decide, com o menor atraso possível, se o texto acumulado é exatamente `[SKIP]`
  (suprime a resposta inteira) ou diverge dele (libera o texto acumulado como conteúdo
  real, mais qualquer delta seguinte, direto). Máquina de estados simples: `Pending`
  enquanto o buffer é um prefixo do marcador, decide `Skip`/`NotSkip` assim que diverge
  ou completa o marcador.
- **`engine.rs`** — `ResponseEngine`: mantém o provedor ativo, a configuração, um
  histórico rolante de até 20 turnos finalizados (`MAX_HISTORY_TURNS`) e um mapa de
  gerações em andamento por `TurnId`. `trigger_generation` cria um `generation_id`
  monotônico e um `CancellationToken`; se já havia uma geração para aquele turno, ela é
  cancelada e um evento `Cancelled` é emitido para a geração anterior antes de iniciar a
  nova. `process_conversation_events` é o ponto de entrada chamado a cada lote de eventos
  da timeline: acumula turnos finalizados no histórico e dispara geração em
  `UtteranceFinalized` de turnos elegíveis.
- **`events.rs`** — evento `response://suggestion-event`
  (`ResponseSuggestionEvent::{Started, Delta, Completed, Skipped, Cancelled, Error}`),
  cada variante carregando `turn_id` + `generation_id` para o frontend descartar eventos
  de uma geração já superada.
- **`config_store.rs`** — configuração não-secreta persistida em JSON (mesmo padrão de
  escrita atômica via arquivo temporário + rename de `model_manager::config_store`):
  `ResponseProviderKind` (`ollama`/`open_ai`/`deep_seek`/`anthropic`), `model`, `base_url`
  opcional. Padrão: Ollama local, modelo `llama3.1`. Arquivo ausente ou corrompido cai
  para o padrão em vez de impedir a inicialização do app.
- **`secrets.rs`** — API keys de provedores de nuvem armazenadas no keychain do SO via
  crate `keyring` (Windows Credential Manager / macOS Keychain / Secret Service no
  Linux), **nunca** em texto puro — decisão explícita do usuário durante o design desta
  camada, em detrimento de uma alternativa mais simples (JSON em texto puro). Ollama não
  tem conta associada (`account_for` devolve `None`) por não usar API key.
- **`net.rs`** — utilidades de streaming HTTP compartilhadas: `line_stream` (bytes brutos
  → linhas completas, lidando com quebra de linha partida entre chunks) e
  `sse_data_payloads` (extrai payloads `data: ...` de Server-Sent Events sobre um
  `line_stream`). Ollama usa NDJSON puro (`line_stream` direto); OpenAI-compatível e
  Anthropic usam SSE (as duas funções em conjunto).
- **`ollama.rs`** — `POST {base_url}/api/chat` (`base_url` padrão
  `http://localhost:11434`), `stream: true`, uma linha JSON completa por chunk.
- **`openai_compatible.rs`** — cliente único reaproveitado por OpenAI
  (`https://api.openai.com/v1`) e DeepSeek (`https://api.deepseek.com/v1`), já que ambas
  expõem a mesma API de chat completions: `POST {base_url}/chat/completions`,
  autenticação `Authorization: Bearer`, streaming SSE no formato `choices[0].delta.content`,
  fim de stream marcado por `data: [DONE]`.
- **`anthropic.rs`** — Messages API nativa (`POST {base_url}/v1/messages`, padrão
  `https://api.anthropic.com`), autenticação `x-api-key` + header `anthropic-version`,
  streaming SSE com eventos nomeados (`content_block_delta`/`text_delta`,
  `message_stop`, `error`) — formato diferente do usado por OpenAI/DeepSeek, por isso não
  compartilha o parsing de chunk com `openai_compatible.rs` (só a extração SSE genérica de
  `net.rs`).
- **`mod.rs`** — comandos Tauri (`response_provider_status_command`,
  `response_set_provider_config_command`, `response_set_api_key_command`,
  `response_delete_api_key_command`) e `ResponseEngineState`, construído uma vez em
  `.setup()` com o caminho de configuração resolvido a partir do diretório de dados do
  app.

## Troca de provedor e de credencial

Trocar a configuração (`response_set_provider_config_command`) reconstrói o provedor
ativo e persiste a nova configuração atomicamente. Salvar ou remover uma API key
(`response_set_api_key_command`/`response_delete_api_key_command`) chama
`ResponseEngine::reload_provider_if_current`, que só reconstrói o provedor ativo se ele
for do tipo cuja credencial acabou de mudar — evita exigir que o usuário reenvie a
configuração inteira só para atualizar uma chave. Se um provedor de nuvem estiver
selecionado sem API key configurada (ou com falha de leitura do keychain), o provedor
ativo vira um `MisconfiguredProvider` que devolve `ResponseProviderError::Credential` na
primeira tentativa de geração, em vez de falhar silenciosamente ou no momento da troca de
configuração.

## Frontend

`src/responseSuggestionViewModel.ts` reduz os eventos de
`response://suggestion-event` num `Record<TurnId, SuggestionState>`. Segue a mesma
semântica de supersessão do backend: eventos que não sejam `started` só são aplicados se
o `generation_id` do evento ainda casar com o `generationId` armazenado para aquele
turno — eventos de uma geração já cancelada são descartados silenciosamente. A sugestão é
renderizada em `App.tsx` (`ConversationTimelineView`) como um painel anexado abaixo da
última utterance do turno elegível ao qual pertence, não como reescrita/destaque do texto
transcrito do turno. `ResponseProviderSettings` (também em `App.tsx`) permite escolher
provedor/modelo/URL base e gerenciar a API key, seguindo o mesmo padrão visual de
`AdvancedTranscriptionSettings`.

## O que foi verificado neste ambiente vs. o que ainda precisa de confirmação manual

Este sandbox é Linux/WSL2, sem backend de keychain em execução (sem Secret Service) e
sem acesso de rede de teste contra os endpoints reais dos provedores.

**Verificado neste sandbox:**
- `cargo fmt --check`, `cargo check --target x86_64-unknown-linux-gnu`,
  `cargo clippy --target x86_64-unknown-linux-gnu -- -D warnings` (mesmos avisos
  pré-existentes de dead-code Windows-`cfg`-gated documentados no `CLAUDE.md`, nenhum
  novo introduzido por este módulo) e `cargo test --target x86_64-unknown-linux-gnu`
  (128 testes, todos passando) — cobre parsing de streaming NDJSON/SSE (`net.rs`),
  montagem de contexto/prompt (`context.rs`), a máquina de estados do `SkipDetector`
  (marcador completo em um chunk, partido entre chunks, divergência no meio do
  marcador, stream vazio), carregamento/salvamento/corrupção de `config_store.rs` e
  mapeamento de erros do `keyring` em `secrets.rs`.
- `npm run typecheck`, `npm run lint`, `npm run build` — limpos, incluindo o reducer
  `applyResponseSuggestionEvent` e seus testes manuais (`responseSuggestionViewModel.test.ts`).

**Ainda precisa de confirmação manual (não fabricado aqui):**
- Chamadas reais de streaming contra Ollama, OpenAI, DeepSeek e Anthropic — o parsing de
  NDJSON/SSE foi testado com payloads sintéticos, não contra os endpoints reais.
- Armazenamento/leitura real de API key no keychain do SO (Windows Credential Manager,
  macOS Keychain, Secret Service no Linux) — este sandbox não tem nenhum backend de
  keyring utilizável para validar `secrets.rs` fim a fim; o mapeamento de erro
  `NoStorageAccess`/`NoDefaultStore` → `SecretError::Unavailable` está coberto por
  lógica, não por um keychain real indisponível de fato.
- Latência de ponta a ponta (meta de ~1s) em condição real de rede/GPU/CPU contra cada
  provedor.
- Qualidade da decisão de `[SKIP]` e da sugestão de resposta em conversas reais e longas
  (o prompt e o teto de contexto foram desenhados a partir dos requisitos, não ajustados
  empiricamente contra transcrições reais).
- Comportamento do cancelamento/substituição de geração (turno que continua sendo falado
  enquanto uma sugestão já está sendo gerada) sob timing real de fala humana, em vez de
  timers/eventos sintéticos de teste.
- Teste manual da UI (`ResponseProviderSettings`, painel de sugestão em streaming) em um
  app Tauri rodando de fato — este ambiente é código-only, sem sessão gráfica para abrir
  o app.
