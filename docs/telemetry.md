# Telemetria de pipeline

A linha do tempo de uma **fala**, do primeiro chunk de áudio até o primeiro token visível
da sugestão, medida ponta a ponta.

Código: `src-tauri/src/telemetry/`.

## Freshness e diagnostico de geracao

`GenerationDiagnostics` complementa o trace com a prova de vinculacao da resposta:
`session_id`, `generation_id`, `turn_id`, `utterance_id`, `utterance_revision`,
`trigger_text` sanitizado, SHA-256 do trigger somente no modo Developer,
`context_utterance_ids`, tamanho do
contexto, resultado de validacao, uso de retry, score de vazamento, caracteres suprimidos
pelo EchoGuard e estado terminal. Tambem expoe `speech_ended_at`,
`transcription_completed_at`, `utterance_finalized_at`, `generation_triggered_at`,
`request_started`, `first_visible_token_at`, `completed_at`,
`utterance_age_at_generation_start_ms` e `utterance_age_at_first_token_ms`.

A `TranscriptionQueue` agora reporta `queue_depth`, `oldest_segment_age_ms`,
`newest_segment_age_ms` e `segments_dropped`. A politica continua `drop_newest`: as
metricas foram adicionadas antes de qualquer mudanca de descarte, porque nao havia
evidencia de producao mostrando que o consumidor estava processando audio velho.

## Por que uma camada nova

As latências já medidas eram parciais e viviam em lugares que não se falam:

- `GenerationDiagnostics` conhece "utterance finalizada → token visível", mas não sabe
  quanto tempo a transcrição levou;
- a fila de transcrição conhece o tempo de inferência, mas não sabe o que aconteceu depois.

Quando o usuário diz "demorou uns 4 segundos", nenhuma das duas responde onde os 4 segundos
foram. Um trace único, correlacionado pelos ids que já existem
(`SegmentId` → `UtteranceId` → `GenerationId`), responde.

## Duas decisões que definem a camada

**Relógio monotônico.** Todo marco é um `Instant`. Epoch está sujeito a ajuste de relógio do
sistema e produziria durações negativas ou absurdas exatamente durante uma reunião longa,
que é quando a medição importa. O trace guarda um `origin: Instant` e os marcos como
`Duration` a partir dele — serializável sem nenhuma conversão para epoch.

**Conteúdo não é telemetria.** Por padrão (`ContentPolicy::Redacted`) nenhum texto
transcrito entra num trace; só tamanhos e contagens. Texto sanitizado — espaços colapsados,
truncado em `SANITIZED_PREVIEW_CHARACTERS = 160` — só aparece sob `ContentPolicy::Developer`,
que corresponde ao "Modo de desenvolvedor" em Configurações, e a mudança de política vale só
para traces criados dali em diante.

## Os 22 marcos

Na ordem em que ocorrem:

| # | `Milestone` | Onde é gravado |
| --- | --- | --- |
| 1 | `speech_start_detected` | `transcription/runtime.rs`, instante monotônico do VAD local |
| 2 | `speech_end_detected` | `transcription/runtime.rs`, instante monotônico do VAD local |
| 3 | `first_audio_chunk_sent` | callback interno do provider, depois do WebSocket send |
| 4 | `activity_start_sent` | callback interno do provider, depois do WebSocket send |
| 5 | `last_audio_chunk_sent` | callback interno do provider; sobrescreve a cada envio |
| 6 | `activity_end_sent` | callback interno do provider, depois do WebSocket send |
| 7 | `first_input_transcription_received` | callback interno do provider |
| 8 | `last_input_transcription_received` | callback interno do provider; sobrescreve por revisão |
| 9 | `server_turn_complete_received` | callback interno do provider; apenas diagnóstico |
| 10 | `local_final_transcript_emitted` | callback interno do provider |
| 11 | `speech_started` | `transcription/runtime.rs`, ao receber o evento do provider |
| 12 | `speech_ended` | `transcription/runtime.rs`, ao receber o evento do provider |
| 13 | `first_audio_chunk` | `transcription/runtime.rs` |
| 14 | `last_audio_chunk` | `transcription/runtime.rs` |
| 15 | `first_partial_transcript` | `transcription/runtime.rs` (só se o provider tem parciais) |
| 16 | `final_transcript` | `transcription/runtime.rs` |
| 17 | `normalization_completed` | `transcription/runtime.rs`, depois do normalizador |
| 18 | `utterance_finalized` | `conversation.rs` |
| 19 | `generation_started` | `response_provider/engine.rs` |
| 20 | `first_http_chunk` | `response_provider/engine.rs` (via `mark_at`) |
| 21 | `first_visible_token` | `response_provider/engine.rs`, depois do `SkipDetector` (via `mark_at`) |
| 22 | `generation_completed` | `response_provider/engine.rs` |

`Milestone` é um **índice denso** de propósito: um trace é `[Option<Duration>; 22]`, sem
alocação por marco e sem custo mensurável no caminho crítico.

Regra de escrita: **primeiro-escrito-vence**, exceto `last_audio_chunk`, que sobrescreve a
cada ocorrência. Sem essa distinção, um segundo chunk moveria o marco do primeiro e
`first_audio_chunk` mediria outra coisa.

## As 5 latências derivadas

```rust
pub struct PipelineLatencies {
    pub speech_start_to_first_partial_ms: Option<u64>,
    pub speech_end_to_activity_end_ms: Option<u64>,
    pub activity_end_to_last_partial_ms: Option<i64>,
    pub activity_end_to_final_transcript_ms: Option<u64>,
    pub speech_end_to_final_transcript_ms: Option<u64>,
    pub speech_ended_to_final_transcript_ms: Option<u64>,
    pub final_transcript_to_utterance_finalized_ms: Option<u64>,
    pub utterance_finalized_to_generation_started_ms: Option<u64>,
    pub generation_started_to_first_visible_token_ms: Option<u64>,
    pub speech_ended_to_first_visible_token_ms: Option<u64>,   // métrica principal de UX
}
```

Cada trecho aponta para um culpado diferente, o que é o ponto de dividir assim:

| Latência | Se está alta, o problema é |
| --- | --- |
| `speech_ended → final_transcript` | o transcritor (modelo grande demais, CPU saturada) |
| `final_transcript → utterance_finalized` | `same_speaker_utterance_gap_ms` — é espera deliberada, não lentidão |
| `utterance_finalized → generation_started` | o disparo. Meta de engenharia: < 100 ms |
| `generation_started → first_visible_token` | o LLM (modelo frio, contexto grande, rede) |
| `speech_ended → first_visible_token` | o total que o usuário sente |

Todas são `Option`. Uma fala que o modelo decidiu não responder simplesmente não tem
`generation_started → first_visible_token`, e reportar `0` ali seria mentira; `between`
devolve `None` quando qualquer um dos dois marcos falta.

## Atributos

Não temporais, e nenhum deles é conteúdo:

```
transcription_provider · transcription_model · transcription_queue_wait_ms
provider_queue_wait_ms · provider_queue_depth · provider_queue_oldest_age_ms · dropped_audio_chunks
audio_chunk_duration_ms · audio_chunks_sent · bytes_sent · websocket_send_latency_ms
automatic_vad_enabled · finalization_strategy · finalization_reason · partial_revision_count
response_provider      · response_model
raw_text_length        · normalized_text_length · normalization_change_count
context_turn_count     · context_character_count
sanitized_text                  (só sob ContentPolicy::Developer)
```

`transcription_queue_wait_ms` separa contenção/backpressure do tempo gasto dentro do
provider. `raw_text_length` vs. `normalized_text_length` mais `normalization_change_count` dão o
suficiente para investigar a normalização sem reter a fala.

## Recorder

`recorder.rs`. É um **singleton de processo** (`OnceLock`), e isso é uma decisão, não
conveniência: os marcos de um mesmo trace são gravados por três subsistemas construídos em
pontos diferentes do `setup()` (runtime de transcrição, timeline, motor de resposta) que não
se conhecem. Injetar o recorder nos três os obrigaria a compartilhar uma dependência que só
existe para observabilidade — acoplamento pior que o singleton. **Testes nunca usam
`recorder()`**: constroem `TelemetryRecorder::new()` isolado.

Tudo é limitado: `MAX_LIVE_TRACES = 64` vivos, `MAX_COMPLETED_TRACES = 256` concluídos. Uma
reunião de horas não pode virar vazamento de memória por observabilidade.

Correlação: `link_segment` / `link_utterance` / `link_generation` registram os índices
reversos, e `trace_for_segment` / `trace_for_utterance` / `trace_for_generation` permitem que
cada subsistema encontre o trace da fala sem carregar um `TraceId` por toda a assinatura.

`discard_session` descarta os traces de uma sessão encerrada — a mesma fronteira de
isolamento que vale para transcrição e geração vale aqui.

O motor de resposta grava com `mark_at` (marco em instante já passado) porque mede seus
próprios `Instant` dentro do laço de streaming, onde não pode pegar o lock do recorder a
cada chunk, e só reporta ao terminar.

## Consumo

- `telemetry_snapshot_command(limit)` — últimos traces concluídos (teto de 50 por chamada,
  porque o resultado é serializado inteiro para o frontend). Só faz sentido atrás de "Modo
  de desenvolvedor": é a visão de onde o tempo foi gasto, não informação de uso normal.
- `telemetry_set_content_policy_command(policy)` — única forma de sair de `Redacted`.
- O harness de benchmark (`benchmarks/README.md`) consome `PipelineTraceSnapshot`
  diretamente.

`GenerationDiagnostics` (evento `diagnostics` em `response://suggestion-event`) continua
existindo e não foi substituído: ele é por geração e chega em tempo real ao frontend; o
trace é por fala e cobre o pipeline inteiro.
