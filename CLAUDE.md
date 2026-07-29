# Helppye

Copiloto de reuniões em tempo real. Núcleo Tauri 2 (Rust) + frontend React/TypeScript.
Local-first por padrão: transcrição sempre local (Whisper). A sugestão de resposta usa
LLM local via Ollama por padrão, mas o usuário pode optar explicitamente por um provedor
de nuvem (OpenAI, DeepSeek ou Anthropic) — nesse caso a API key fica no keychain do SO,
nunca em texto puro no disco.

## Status

Fundação de áudio e transcrição local implementada. A captura de microfone, a captura de
saída do sistema no Windows via WASAPI Loopback, o pipeline de VAD/segmentação e a
transcrição local com Whisper Base Multilíngue já existem. A Conversation Timeline
organiza transcrições em uma linha do tempo única preservando ordem, origem
(usuário/outra pessoa) e timestamps. A camada atual em construção é a **sugestão de
resposta em streaming** (`src-tauri/src/response_provider/`): turnos elegíveis da outra
pessoa disparam geração de uma sugestão de resposta via LLM (Ollama/OpenAI/DeepSeek/
Anthropic, escolhido pelo usuário), transmitida ao frontend em streaming. Substitui a
antiga detecção de perguntas por regras, removida. Overlay de resposta dedicado (janela
flutuante fora da timeline) ainda **não** está implementado.

## Stack

- **Backend/core:** Tauri 2, Rust estável (edition 2021), Tokio (`rt-multi-thread`),
  `cpal` (captura de microfone multiplataforma), `tracing`/`tracing-subscriber` para
  logging estruturado, `thiserror` para erros tipados, `async-trait`.
- **Frontend:** React 18, TypeScript estrito, Vite, Tailwind CSS, Zustand.
- **Transcrição local:** `whisper-rs`/whisper.cpp, CPU-only por padrão, modelo padrão
  Whisper Base Multilíngue (`ggml-base.bin`) baixado apenas após ação explícita do
  usuário.
- **Planejado, ainda não implementado:** SQLite (histórico persistente além das
  configurações JSON atuais).

## Layout

- `src/` — frontend React/TypeScript. `App.tsx` é a UI atual: onboarding/download do
  modelo de transcrição, configurações avançadas de modelo local, dois painéis de
  captura (microfone e saída do sistema) e a visualização consolidada da Conversation
  Timeline.
- `src-tauri/` — núcleo Rust (comandos Tauri, pipeline de áudio). Ver seção "Módulo de
  áudio" abaixo.
- `docs/` — auditoria de arquitetura, notas de design, roadmap. Ler antes de tocar em
  captura de áudio ou em qualquer coisa relacionada ao Meetily.
- `prompts/` — templates de prompt para LLM, usados a partir da Fase 4 (integração
  Ollama). Vazio por enquanto.
- `tests/` — testes cross-cutting/integração entre frontend e core Rust. Testes
  unitários do pipeline de áudio ficam junto aos módulos em `src-tauri/src/audio/`
  (`#[cfg(test)]`), não aqui.
- `meetily/` — clone local, referência apenas, **não faz parte do pacote** (ver seção
  abaixo). Não modificar.

## Módulo de áudio (`src-tauri/src/audio/`)

Arquitetura por trait: `provider::AudioCaptureProvider` (`list_devices`, `start`,
`stop` opcional) é implementado por cada fonte. Duas fontes hoje:

- **Microfone** (`pipeline::MicrophoneCaptureProvider`) — via `cpal`, multiplataforma.
  Enumeração de dispositivos em `devices.rs`.
- **Saída do sistema** (`platform::SystemAudioProvider`, `#[cfg(target_os = ...)]`
  re-exportado por plataforma a partir de `platform/mod.rs`):
  - **Windows:** implementado via WASAPI loopback em modo compartilhado, isolado sob
    `platform/windows/{mod,com,devices,format,capture}.rs`. Ver
    `docs/windows-wasapi-loopback.md` para o design completo (apartment COM MTA,
    detecção de desconexão via `AUDCLNT_E_DEVICE_INVALIDATED`, conversão de formato,
    e o que foi verificado em sandbox Linux vs. o que ainda precisa de confirmação em
    Windows real).
  - **macOS/Linux:** stubs que retornam `AudioCaptureError::Unsupported` honestamente
    (nada de captura fingida). macOS planejado via Core Audio Process Tap (candidato a
    adaptação do Meetily, ver `docs/third-party-components.md`); Linux planejado via
    PipeWire nativo.

Pipeline compartilhado, independente de fonte:

- `config::CaptureConfig` — device_id opcional, taxa alvo (16 kHz), canais alvo (1,
  sempre mono após downmix), duração de frame (100ms), capacidade do canal (32).
- `types::AudioCaptureEvent` — `Started`/`Frame`/`Stopped`/`DeviceDisconnected`/`Error`,
  todos exceto `Started`/`Frame` carregando um campo `source: AudioSource` para que o
  frontend roteie eventos de mic e de saída de sistema para o painel correto quando
  ambos capturam simultaneamente.
- `sample_convert.rs` — conversão de PCM bruto (i16/i24/i32/f32) para `f32`, pura,
  sem dependência de plataforma, testável em qualquer SO.
- `resampler.rs` — downmix para mono e resample linear para a taxa alvo.
- `level_meter.rs` — cálculo de nível RMS em dBFS para a UI.
- Backpressure: canal `mpsc` limitado, `try_send` (nunca bloqueia o produtor);
  cheio → descarta o frame mais recente e conta em `dropped_frames`, logando a cada 50
  descartes em vez de a cada um. Esse padrão é usado tanto no caminho de mic quanto no
  de loopback do Windows.

`audio/mod.rs` expõe os comandos Tauri (`list_audio_devices_command`,
`list_system_audio_devices_command`, `resolve_device_selection_command`,
`select_input_device_command`, `select_output_device_command`,
`start_microphone_capture_command`, `stop_microphone_capture_command`,
`start_system_audio_capture_command`, `stop_system_audio_capture_command`) e
`CaptureEngineState`. `CaptureEngine` garante exatamente uma sessão ativa por categoria
(entrada e saída), reinicia a categoria correta em trocas de dispositivo e injeta a fila
de transcrição como dependência.

## Transcrição local (`src-tauri/src/transcription/`, `src-tauri/src/model_manager/`)

`transcription::provider::TranscriptionProvider` é o ponto de extensão para backends de
speech-to-text. A implementação atual é `WhisperCppProvider`, baseada em `whisper-rs`,
com inferência CPU-only. `TranscriptionQueue` recebe `AudioSegment`s do pipeline de áudio
em uma fila limitada e não bloqueante; se a transcrição ficar atrasada, segmentos novos
são descartados e contabilizados, sem aplicar backpressure à captura.

`model_manager` implementa o fluxo guiado de primeiro uso: status do modelo, download
explícito, progresso, cancelamento, verificação de SHA-256, instalação atômica,
persistência da seleção e carregamento real no provider. O modelo padrão é o Whisper Base
Multilíngue; modelos personalizados podem ser selecionados por caminho local e são
validados antes de persistir.

## Conversation Timeline (`src-tauri/src/conversation.rs`)

A Timeline é a primeira camada que une as duas fontes de áudio em uma conversa única sem
misturar áudio bruto. Ela consome `TranscriptEvent::Ready`, transforma cada resultado em
um `TranscriptSegment` bruto e monta duas camadas de domínio:

- `ConversationUtterance`: frase/bloco curto formado por segmentos próximos do mesmo
  speaker/source.
- `ConversationTurn`: tudo que um interlocutor falou enquanto manteve a palavra, podendo
  conter várias utterances.

`ConversationAssembler` mantém uma utterance aberta enquanto novos segmentos têm o mesmo
speaker/source, respeitam `same_speaker_utterance_gap_ms` (default inicial: 1800 ms) e
não excedem `maximum_utterance_duration_ms` (default inicial: 120000 ms). O turno acima
dela permanece aberto mesmo com pausas curtas entre utterances; ele só fecha por mudança
de speaker/source, pausa/parada da captura, flush, encerramento de sessão,
`turn_inactivity_timeout_ms` (default inicial: 20000 ms) ou
`maximum_turn_duration_ms` (default inicial: 300000 ms). Segmentos fora de ordem são
tolerados dentro de `out_of_order_tolerance_ms` (default inicial: 1000 ms), com warning;
segmentos fora da tolerância não reabrem turnos já finalizados.

Ao unir texto, a camada remove apenas espaços duplicados, adiciona um espaço entre
trechos e preserva a pontuação produzida pelo transcritor. Não há correção semântica,
reescrita, LLM, resumo ou troca de modelo nesta camada.

Eventos emitidos via `conversation://timeline-event`:
`utterance_started`/`utterance_updated`/`utterance_finalized` e
`turn_started`/`turn_updated`/`turn_finalized`. A Timeline também expõe
`conversation_timeline_snapshot_command` (snapshot com `turns` e `utterances`),
`conversation_flush_turns_command`, `conversation_end_session_command` e
`conversation_raw_segments_command`.

Os timestamps dos segmentos são convertidos pelo `CaptureEngine` para um relógio
monotônico comum do processo antes de entrar na fila de transcrição, para que falas de
microfone e saída do sistema sejam comparáveis na mesma linha do tempo. O
`ResponseEngine` (`response_provider::engine::process_conversation_events`) consome
`ConversationTurn`, não `AudioFrame`, `AudioSegment` ou eventos brutos de transcrição.

## Sugestão de resposta (`src-tauri/src/response_provider/`)

Substitui a antiga detecção local de perguntas por regras (`question_detection.rs`,
removida). Em vez de apenas sinalizar que um turno da outra pessoa parece uma pergunta,
o `ResponseEngine` gera, via LLM e em streaming, uma sugestão real de resposta. Roda
sobre o mesmo `ConversationTurn` elegível de antes (`speaker = OtherPerson`,
`source = SystemOutput`), disparando geração em `UtteranceFinalized` (não é preciso
esperar o turno inteiro terminar). Uma nova utterance no mesmo turno cancela e substitui
a geração em andamento, para nunca sugerir resposta a uma fala que ainda não terminou.

O usuário escolhe o provedor de LLM: Ollama local (padrão) ou um provedor de nuvem
(OpenAI, DeepSeek, Anthropic). A mesma chamada que gera a resposta também decide se deve
responder, via um marcador `[SKIP]` no início do stream quando a fala não exige resposta
— sem uma segunda chamada de classificação. API keys de provedores de nuvem ficam no
keychain do SO via crate `keyring`, nunca em texto puro no disco. Eventos emitidos via
`response://suggestion-event`: `started`, `delta`, `completed`, `skipped`, `cancelled` e
`error`, todos carregando `turn_id` e `generation_id` para o frontend descartar eventos
de uma geração já superada. Ver `docs/response-suggestion.md` para a arquitetura
completa, módulo por módulo.

## Relação com o Meetily

`meetily/` é um clone local, **somente leitura/referência**, do projeto Meetily da
Zackriya Solutions (MIT), usado exclusivamente para pesquisa de arquitetura. **Nunca
modificar `meetily/`.** Ver `docs/meetily-audio-audit.md` para a auditoria completa
(o que é reusável, o que não é, achados de arquitetura/segurança) e
`docs/third-party-components.md` para o registro de qualquer coisa efetivamente
adaptada de lá (com atribuição, também rastreada em `NOTICE`).

## Comandos de validação

Rodar antes de qualquer commit:

```bash
cargo fmt --check                                  # em src-tauri/
cargo check --target x86_64-unknown-linux-gnu
cargo clippy --target x86_64-unknown-linux-gnu -- -D warnings
cargo test --target x86_64-unknown-linux-gnu
npm run typecheck
npm run lint
npm run build
```

**Nota sobre clippy no target Linux:** código exclusivo de Windows/macOS (`#[cfg(...)]`)
aparece como dead-code quando se compila só para Linux — isso é uma característica
inerente e pré-existente de desenvolver esta base multiplataforma a partir de um
sandbox Linux/WSL2, não um bug introduzido por uma mudança específica. Não suprimir
com `#[allow(dead_code)]` — isso mascararia dead-code genuíno futuro. O sinal correto
para código Windows-específico é `cargo check`/`clippy --target x86_64-pc-windows-gnu`
(cross-compile de front-end, sem linker — funciona neste sandbox via
`rustup target add x86_64-pc-windows-gnu`). Se `tauri-build` falhar no cross-compile
para Windows (problema pré-existente de metadata de empacotamento, não relacionado a
mudanças de código de áudio), validar o código Windows-específico isoladamente: copiar
`src-tauri/src/audio/{sample_convert.rs,resampler.rs,config.rs,error.rs,types.rs,
provider.rs}` e `platform/windows/*` para uma crate mínima sem dependência do `tauri`
(sem `tauri-build`) e rodar `cargo check`/`clippy` nela com o target Windows.

## Convenções

- Todo bloco `unsafe` (praticamente só em `platform/windows/`) precisa de um
  comentário `// SAFETY:` explicando por que as precondições estão satisfeitas.
  Funções `unsafe fn` documentam o contrato de segurança em uma seção `# Safety` no
  doc comment.
- Dispositivos devem ser identificados por ID estável sempre que a plataforma expuser um
  ID real (ex. `IMMDevice::GetId` no Windows para saída WASAPI), nunca por substring de
  nome — a auditoria do Meetily (seção 6, achado 7) sinalizou correspondência por nome
  como frágil. Limitação atual: o caminho de microfone via `cpal` ainda usa o nome do
  dispositivo como `id` best-effort, porque o `cpal` não expõe um identificador estável
  multiplataforma nesse nível.
- Não fabricar resultados de teste que não foram de fato executados neste ambiente
  (WSL2/Linux, sem hardware de áudio Windows/macOS real). Ao documentar validação,
  separar explicitamente "verificado neste sandbox" de "ainda precisa de confirmação
  manual" — ver `docs/windows-wasapi-loopback.md` seção 7 como modelo.
- Documentação em `docs/` é escrita em português (segue os documentos existentes:
  `meetily-audio-audit.md`, `third-party-components.md`).
