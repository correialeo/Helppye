# Sugestão de resposta em streaming (`src-tauri/src/response_provider/`)

Substitui a antiga detecção local de perguntas por regras (`question_detection.rs`,
removida). Em vez de apenas sinalizar que um turno da outra pessoa parece uma pergunta,
o pipeline atual gera, via LLM e em streaming, uma sugestão real de resposta para o
usuário — mantendo a filosofia local-first como padrão, mas permitindo que o usuário
escolha explicitamente um provedor de nuvem quando quiser.

## Visão geral do fluxo

```
Conversation Timeline (turns/utterances)
        │  silêncio ≥ same_speaker_utterance_gap_ms → timer dedicado finaliza a
        │  utterance sozinho (sem esperar novo segmento/flush/stop/turno fechar)
        │  UtteranceFinalized (turno elegível)
        ▼
process_conversation_events → GenerationTrigger (finalization_reason, gap_ms_used,
        │  silence_detected_ms, utterance_finalized_at)
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

**Esse gatilho depende de um timer dedicado, não só do próximo segmento.** Antes desta
correção, `UtteranceFinalized` só era emitido reativamente — quando um novo segmento de
transcrição chegava e o assembler comparava seu gap com o fim da utterance aberta, ou por
flush/stop/fim de sessão manuais. Isso significava que, se a outra pessoa simplesmente
parasse de falar (o caso comum: terminou uma pergunta e está esperando resposta), nada
finalizava a utterance — e portanto nada disparava a geração — até que *algo mais*
acontecesse (outra fala, um flush manual, parar a captura). `ConversationTimeline` agora
mantém um `tokio::time::sleep` dedicado por utterance aberta (reagendado a cada novo
segmento, comparando a `revision` da utterance para descartar timers obsoletos sem
precisar de cancelamento explícito): se ele expirar sem que a utterance tenha mudado,
ela finaliza sozinha, por silêncio, mantendo o turno aberto. Ver a seção
"Timer dedicado da utterance vs. fechamento do turno" abaixo e
`ConversationTimeline::reschedule_utterance_timer`/`fire_utterance_timeout` em
`conversation.rs`.

## Timer dedicado da utterance vs. fechamento do turno

Fechar a utterance e fechar o turno são decisões independentes:

- **Utterance** fecha por silêncio (`same_speaker_utterance_gap_ms`, via o timer
  dedicado ou reativamente se um segmento tardio chegar primeiro), por troca de
  speaker/source, por duração máxima, ou por flush/stop/fim de sessão.
- **Turno** só fecha por troca de speaker/source, `turn_inactivity_timeout_ms`
  (bem maior, 20s por padrão — ainda avaliado só reativamente, não tem timer dedicado:
  o turno pode ficar aberto indefinidamente sem prejudicar a geração, que já depende só
  da utterance), duração máxima, ou flush/stop/fim de sessão.

A finalização da utterance nunca é bloqueada por o turno continuar aberto — é
exatamente o oposto do bug original: o turno podia (e ainda pode) ficar aberto por até
20s agrupando a conversa, mas isso não pode impedir a geração de começar assim que a
utterance mais recente termina.

`ConversationTimelineEvent::UtteranceFinalized` carrega `finalization_reason`
(`conversation::UtteranceFinalizationReason`: `inactivity_timeout`, `speaker_changed`,
`source_changed`, `capture_stopped`, `manual_flush`, `session_ended`,
`maximum_duration`), `gap_ms_used` (`same_speaker_utterance_gap_ms` vigente no momento),
`silence_detected_ms` (quando mensurável) e `session_id`. A geração automática do
`ResponseEngine` funciona para qualquer motivo, mas os três esperados no dia a dia são
`inactivity_timeout` (o caso comum: silêncio após a fala), `speaker_changed` e
`source_changed` (a pessoa terminou de falar porque alguém mais começou).

`same_speaker_utterance_gap_ms` (default 1800ms) é configurável em runtime, sem rebuild,
via `conversation_get_utterance_gap_ms_command`/`conversation_set_utterance_gap_ms_command`
— exposto no frontend só em modo dev (`UtteranceGapDevControl` em `App.tsx`, com atalhos
para 1200/1500/1800/2200ms) para calibrar o trade-off entre latência e responder cedo
demais (no meio de uma pergunta ainda incompleta).

## Elegibilidade

Igual à extinta detecção de perguntas: só turnos com `speaker = OtherPerson` e
`source = SystemOutput` disparam geração (`engine::is_eligible_turn`). O usuário nunca
recebe uma "sugestão de resposta" para a própria fala.

## Diagnóstico em modo dev

Sem instrumentação, a UI só distinguia `streaming`/`skipped`/`cancelled`/`error`/
`completed` — e `completed` com texto vazio (resposta do LLM vazia) e `skipped`
(marcador `[SKIP]` detectado) apareciam ambos como "Nenhuma resposta sugerida" na tela,
tornando impossível diferenciar, sem logs, se a requisição não chegou ao provedor, se o
modelo decidiu `[SKIP]` de propósito, se o parser do stream interpretou mal o início da
resposta, se uma nova utterance cancelou a geração cedo demais, ou se a resposta veio
genuinamente vazia.

Para isso, toda geração emite, ao final (`ResponseSuggestionEvent::Diagnostics`), um
`GenerationDiagnostics` com:

- `generation_id`, `turn_id`, `provider`, `model` — identificação da geração.
- `request_started` — epoch ms de quando a chamada ao provedor começou.
- `http_status` — código HTTP da resposta bem-sucedida, `None` se a conexão falhou antes
  disso (hipótese "a requisição nem chega ao provedor").
- `first_chunk_received` — epoch ms do primeiro chunk do stream, `None` se nenhum chegou.
- `raw_prefix` — até ~80 caracteres brutos recebidos do provedor, **antes** de qualquer
  filtragem do `SkipDetector` — permite confirmar se o modelo de fato respondeu `[SKIP]`
  literalmente, em vez de supor.
- `skip_detected` — se o `SkipDetector` decidiu `Skip`.
- `cancel_reason` — hoje só existe uma causa possível: `"new_utterance"` (uma nova
  utterance no mesmo turno substituiu esta geração).
- `latency_ms` — duração total da geração, do início da chamada ao provedor até o
  evento final.
- `final_text_length` — tamanho (em caracteres) do texto final acumulado.
- `event_emitted` — um dos cinco estados finais possíveis, como string:
  `"skipped"`, `"error"`, `"cancelled"`, `"completed_empty"`, `"completed_with_text"`.
  `completed_empty` e `completed_with_text` derivam de `Completed` conforme o texto final
  estar vazio (ou só espaço em branco) ou não — este é justamente o par de estados que
  antes colapsava, na UI, na mesma mensagem que `skipped`.
- `finalization_reason`, `gap_ms_used`, `silence_detected_ms` — contexto do gatilho
  (`GenerationTrigger`, montado a partir de `ConversationTimelineEvent::UtteranceFinalized`
  em `process_conversation_events`), para responder "por que essa geração começou agora".
- `utterance_finalized_to_request_started_ms` — da utterance finalizada até a chamada ao
  provedor começar, com relógio monotônico (`Instant`, não epoch). Meta de engenharia:
  < 100 ms — é a métrica que mede diretamente o atraso de disparo corrigido neste
  trabalho; se esse número ainda for alto, o problema voltou a ser o disparo, não o LLM.
- `request_to_first_http_chunk_ms` — até o primeiro chunk HTTP bruto, que pode ser
  metadado vazio, não necessariamente texto visível.
- `request_to_first_visible_token_ms` — até o primeiro texto que o `SkipDetector` de fato
  libera para a UI (distinto do chunk HTTP bruto acima de propósito: um só chunk pode
  conter só o início do marcador `[SKIP]`, sem nada visível ainda).
- `end_of_speech_to_first_visible_token_ms` — métrica principal de UX, soma as duas
  anteriores: do silêncio (fim da fala) até o primeiro texto visível na tela.

`suggestionStatusLabel` (`App.tsx`) mostra rótulos distintos para `preparing`
("Analisando fala..."), `completed_empty` ("Resposta gerada veio vazia") e `skipped`
("Nenhuma resposta sugerida"), e um painel `<details>` "Diagnóstico de sugestão de
resposta" (gated por `showSegments = import.meta.env.DEV`, mesmo padrão usado para as
antigas avaliações do detector de perguntas) lista os campos acima por turno, para
inspeção manual durante o desenvolvimento.

## Módulo por módulo

- **`provider.rs`** — abstração comum (`ResponseProvider`, `ResponseRequest`,
  `ResponseChunk::{Delta, Done}`, `ResponseProviderError`, `ResponseStreamMeta`). Cada
  provedor devolve um stream de deltas de texto, nunca a resposta inteira de uma vez — é
  o que permite exibir a sugestão sendo digitada em tempo real em vez de esperar a
  resposta completa. `stream_reply` devolve `(ResponseStream, ResponseStreamMeta)`: o
  `http_status` da resposta bem-sucedida vai para `ResponseStreamMeta` porque, sem ele,
  não havia como o diagnóstico de uma geração confirmar que a requisição de fato chegou
  ao provedor e obteve `200` antes de o stream começar a produzir chunks.
- **`context.rs`** — monta o `ResponseRequest` a partir do histórico de turnos e do
  turno atual. Teto de 4 turnos e 5000 caracteres de histórico (`MAX_HISTORY_TURNS`,
  `MAX_HISTORY_CHARS`), 160 tokens de saída (`MAX_OUTPUT_TOKENS`), `temperature = 0.2`
  (`TEMPERATURE`) — contexto e saída limitados de propósito para manter prompt pequeno e
  latência baixa em vez de reenviar a conversa inteira a cada geração; baixa
  `temperature` favorece uma resposta direta e previsível em vez de variedade criativa,
  o que também ajuda a manter a latência estável entre chamadas. O `SYSTEM_PROMPT`
  instrui o modelo a responder com o marcador fixo `[SKIP]` quando a fala mais recente
  não for uma pergunta/pedido que exija resposta — a mesma chamada que gera a resposta
  também decide se deve responder, sem uma segunda chamada de classificação.
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
  `UtteranceFinalized` de turnos elegíveis, montando um `GenerationTrigger` (instante de
  finalização com relógio monotônico, motivo, gap configurado, silêncio observado) a
  partir do evento. `run_generation` monta um `GenerationDiagnostics` por geração —
  incluindo as métricas de latência derivadas do `GenerationTrigger` e dos instantes
  capturados durante o streaming (primeiro chunk HTTP, primeiro texto visível) — e o
  fecha em `finish_generation`, chamado em **todo** caminho de saída (skip, erro,
  cancelamento ou conclusão natural) via uma macro local (`finish!`) para garantir que
  nenhum retorno adiantado esqueça de liberar o slot de `generations` daquele turno — é
  esse fechamento incondicional que garante que uma geração seguinte para o mesmo turno
  nunca veja um estado "fantasma" de uma anterior já encerrada (coberto por teste, ver
  "Testes" abaixo). Genérico sobre `R: tauri::Runtime` em vez de `AppHandle` fixo
  (`= AppHandle<Wry>`) só para poder ser exercitado com `tauri::test::mock_app` sem uma
  janela/webview real.
- **`events.rs`** — evento `response://suggestion-event`
  (`ResponseSuggestionEvent::{Started, Delta, Completed, Skipped, Cancelled, Error,
  Diagnostics}`), cada variante carregando `turn_id` + `generation_id` para o frontend
  descartar eventos de uma geração já superada. `Diagnostics` carrega um
  `GenerationDiagnostics` completo (ver abaixo) e é emitido ao final de toda geração,
  além do evento "de negócio" correspondente (`Skipped`/`Error`/`Cancelled`/`Completed`).
- **`config_store.rs`** — configuração não-secreta persistida em JSON (mesmo padrão de
  escrita atômica via arquivo temporário + rename de `model_manager::config_store`):
  `ResponseProviderKind` (`ollama`/`open_ai`/`deep_seek`/`anthropic`), `model`, `base_url`
  opcional, `ollama_keep_alive` opcional (só usado pelo provider Ollama; default `"10m"`
  via `#[serde(default = ...)]`, para que arquivos salvos antes deste campo existir
  continuem carregando). Padrão: Ollama local, modelo `llama3.1`. Arquivo ausente ou
  corrompido cai para o padrão em vez de impedir a inicialização do app.
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
  `http://localhost:11434`), `stream: true`, uma linha JSON completa por chunk. Uma única
  instância de `reqwest::Client` por provider (reaproveitada por toda a vida dele —
  reconstruído só quando a configuração muda, ver `engine::build_provider`, nunca uma vez
  por geração), para não jogar fora o pool de conexões HTTP a cada chamada. Envia
  `"options": {"temperature", "num_predict"}` (sem isso, `max_output_tokens` nunca era
  aplicado de fato — a geração não tinha teto e podia continuar bem além do necessário),
  `"think": false` (desliga o modo de raciocínio estendido de modelos híbridos como o
  Qwen3, sem depender de parsing de tags de pensamento) e, se configurado,
  `"keep_alive"` (`DEFAULT_KEEP_ALIVE = "10m"`, ver `config_store.rs`) — sem
  `keep_alive`, o Ollama descarrega o modelo da memória por padrão logo após ficar
  ocioso, e a chamada seguinte paga o custo de recarregá-lo (segundos) além da própria
  inferência.
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
`response://suggestion-event` num `Record<TurnId, SuggestionState>` via
`applyResponseSuggestionEvent`. Segue a mesma semântica de supersessão do backend:
eventos que não sejam `started` só são aplicados se o `generation_id` do evento ainda
casar com o `generationId` armazenado para aquele turno — eventos de uma geração já
cancelada são descartados silenciosamente. `SuggestionStatus` tem sete valores:
`preparing`, `streaming`, `completed_with_text`, `completed_empty`, `skipped`,
`cancelled`, `error` — o evento `completed` do backend vira `completed_with_text` ou
`completed_empty` conforme o texto final (`.trim()`) estar vazio ou não. A sugestão é
renderizada em `App.tsx` (`ConversationTimelineView`) como um painel anexado abaixo da
última utterance do turno elegível ao qual pertence, não como reescrita/destaque do
texto transcrito do turno.

**A resposta anterior não desaparece assim que uma nova geração começa.** `started` não
zera mais o texto visível: se havia uma sugestão `completed_with_text` para o turno, ela
migra para `previousText` e o status vira `preparing` ("Analisando fala..." na UI). O
primeiro `delta` com conteúdo real limpa `previousText` e substitui o que está na tela;
se a nova geração terminar sem conteúdo (skip, vazia, erro, cancelada), `previousText`
continua disponível para a UI não regredir para "nenhuma sugestão" quando havia uma boa
resposta momentos atrás. Isso cobre o caso de continuação de fala descrito acima: a
resposta à primeira pergunta ("Em qual situação você usaria monolitos?") permanece
visível enquanto a segunda ("Em qual situação você usaria microsserviços?") ainda está
sendo preparada, em vez de piscar para um painel vazio entre as duas.

`applyResponseSuggestionDiagnostics` reduz os eventos `diagnostics` separadamente, num
`Record<TurnId, ResponseSuggestionDiagnostics>` só usado pelo painel de depuração — não
afeta `SuggestionState`. `ResponseProviderSettings` (também em `App.tsx`) permite
escolher provedor/modelo/URL base/`ollama_keep_alive` e gerenciar a API key, seguindo o
mesmo padrão visual de `AdvancedTranscriptionSettings`. `UtteranceGapDevControl` (só em
`import.meta.env.DEV`) permite trocar `same_speaker_utterance_gap_ms` em runtime sem
rebuild, com atalhos para 1200/1500/1800/2200ms.

## Testes

- **`conversation.rs`** — o timer dedicado é exercitado de ponta a ponta com
  `#[tokio::test(start_paused = true)]` + `tokio::time::advance` (nunca `sleep` real):
  silêncio absoluto finaliza a utterance sem novo segmento; a finalização mantém o turno
  aberto; uma continuação antes do gap reagenda o timer e o timer obsoleto vira no-op
  (nenhuma finalização duplicada); duas utterances remotas consecutivas geram dois
  eventos de finalização independentes; um flush manual antes do timer expirar também
  torna o timer subsequente um no-op; `set_utterance_gap_ms` muda de fato quanto tempo o
  timer espera. Uma dica de implementação para quem for mexer nesses testes: um timer
  recém-`tokio::spawn`ado precisa ser `yield_now`ado *antes* de `advance` para registrar
  seu prazo na timer wheel — avançar o relógio antes disso não acorda nada.
- **`response_provider/engine.rs`** — `ResponseEngine::for_test` injeta um
  `ResponseProvider` fake (`FakeProvider`, com um modo que responde com texto fixo e um
  modo `Hangs` que nunca produz nada, para testar cancelamento) sem tocar o
  `config_store` real. Os testes usam `tauri::test::mock_app()` (por isso
  `trigger_generation`/`run_generation`/`finish_generation`/`process_conversation_events`
  são genéricas sobre `R: tauri::Runtime` em vez de fixas em `AppHandle<Wry>`) e um
  listener (`app.listen_any`) capturando os eventos emitidos para verificar: só o turno
  elegível dispara geração (fala do usuário nunca dispara); uma nova utterance no mesmo
  turno cancela a geração ainda em andamento; o estado é liberado após uma conclusão
  natural, então um disparo seguinte para o mesmo turno não vê cancelamento algum
  "fantasma"; `end_of_speech_to_first_visible_token_ms` e os demais campos de latência
  são de fato calculados a partir do `GenerationTrigger`.
- **`response_provider/ollama.rs`** — um servidor HTTP/1.1 mínimo, escrito à mão sobre
  `tokio::net::TcpListener` (sem crate de mock), captura o corpo bruto da requisição para
  confirmar que `keep_alive`, `options.num_predict`, `options.temperature` e
  `think: false` realmente vão no JSON enviado ao Ollama — e que `keep_alive` fica
  totalmente ausente (não `null`) quando não configurado.
- **`src/responseSuggestionViewModel.test.ts`** — cobre a transição `started` →
  `preparing` com `previousText`, a limpeza de `previousText` no primeiro delta com
  conteúdo, e a permanência de `previousText` quando a nova geração termina sem conteúdo.

## O que foi verificado neste ambiente vs. o que ainda precisa de confirmação manual

Este sandbox é Linux/WSL2, sem backend de keychain em execução (sem Secret Service) e
sem acesso de rede de teste contra os endpoints reais dos provedores.

**Verificado neste sandbox:**
- `cargo fmt --check`, `cargo check --target x86_64-unknown-linux-gnu`,
  `cargo clippy --target x86_64-unknown-linux-gnu -- -D warnings` (mesmos avisos
  pré-existentes de dead-code Windows-`cfg`-gated documentados no `CLAUDE.md`, nenhum
  novo introduzido por este módulo — o único aviso novo que apareceu ao longo deste
  trabalho, `large_enum_variant` em `ResponseSuggestionEvent` depois de
  `GenerationDiagnostics` ganhar as novas métricas, foi corrigido colocando
  `Diagnostics(Box<GenerationDiagnostics>)`, não suprimido) e
  `cargo test --target x86_64-unknown-linux-gnu` (143 testes, todos passando,
  repetidamente, incluindo em paralelo — cobre parsing de streaming NDJSON/SSE
  (`net.rs`), montagem de contexto/prompt (`context.rs`), a máquina de estados do
  `SkipDetector` (marcador completo em um chunk, partido entre chunks, divergência no
  meio do marcador, stream vazio), carregamento/salvamento/corrupção de
  `config_store.rs`, mapeamento de erros do `keyring` em `secrets.rs`, o timer dedicado
  da utterance com relógio de teste pausado/avançado (`conversation.rs`), o disparo e
  ciclo de vida da geração com um provider fake e `tauri::test::mock_app`
  (`engine.rs`), e o corpo exato da requisição HTTP enviada ao Ollama
  (`ollama.rs`) — ver "Testes" acima para a lista completa por arquivo.
- `npm run typecheck`, `npm run lint`, `npm run build` — limpos, incluindo os reducers
  `applyResponseSuggestionEvent`/`applyResponseSuggestionDiagnostics` e seus testes
  manuais (`responseSuggestionViewModel.test.ts`, rodados via `npx tsx`), cobrindo a
  distinção `completed_with_text`/`completed_empty`, a transição `preparing`/
  `previousText` e o preenchimento de `ResponseSuggestionDiagnostics` a partir do
  evento bruto.

**Ainda precisa de confirmação manual (não fabricado aqui):**
- A causa raiz real do sintoma que motivou o diagnóstico original ("Gerando
  sugestão..." seguido de "Nenhuma resposta sugerida" ao testar contra um Ollama de
  verdade) foi identificada por auditoria de código, não reproduzida contra um Ollama
  real neste sandbox: a finalização de utterance era puramente reativa (só reavaliada
  quando um novo segmento chegava), então sem o timer dedicado agora implementado, uma
  utterance após a qual ninguém mais falava simplesmente nunca finalizava, e a geração
  nunca disparava. Falta ao usuário rodar o cenário de validação manual descrito no
  relatório final (três perguntas consecutivas, sem flush/stop) contra um Ollama real e
  confirmar os tempos observados batem com as metas de engenharia.
- Chamadas reais de streaming contra Ollama, OpenAI, DeepSeek e Anthropic — o parsing de
  NDJSON/SSE foi testado com payloads sintéticos (incluindo, para o Ollama, um servidor
  HTTP real minimalista rodando em `127.0.0.1`, mas ainda não o Ollama de verdade), não
  contra os endpoints reais.
- Efeito real de `keep_alive`/contexto reduzido/`think: false` na latência observada —
  o teste de `ollama.rs` confirma que os bytes corretos são enviados, não o quanto isso
  de fato reduz `request_to_first_visible_token_ms` contra um Ollama/Qwen3 reais.
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
