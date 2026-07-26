# Helppye

Copiloto de reuniões em tempo real. Núcleo Tauri 2 (Rust) + frontend React/TypeScript,
local-first: transcrição local, LLM local via Ollama, sem dependência de nuvem.

## Status

Fundação inicial. A infraestrutura de captura de áudio está sendo construída de forma
incremental. Detecção de perguntas e overlay de resposta ainda **não** estão
implementados — a captura de áudio está sendo estabilizada primeiro (mic + saída do
sistema, nessa ordem).

## Stack

- **Backend/core:** Tauri 2, Rust estável (edition 2021), Tokio (`rt-multi-thread`),
  `cpal` (captura de microfone multiplataforma), `tracing`/`tracing-subscriber` para
  logging estruturado, `thiserror` para erros tipados, `async-trait`.
- **Frontend:** React 18, TypeScript estrito, Vite, Tailwind CSS, Zustand.
- **Planejado, ainda não implementado:** Ollama (LLM local), SQLite (config/histórico).

## Layout

- `src/` — frontend React/TypeScript. `App.tsx` é a UI atual: dois painéis de captura
  (microfone e saída do sistema), cada um parametrizado por uma `PanelConfig` com os
  comandos Tauri a chamar e o `AudioSource` a filtrar nos eventos.
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
`list_system_audio_devices_command`, `start_microphone_capture_command`,
`stop_microphone_capture_command`, `start_system_audio_capture_command`,
`stop_system_audio_capture_command`) e `AudioState` (dois `CancellationToken` opcionais
independentes, um por fonte, sob mutex).

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
- Dispositivos são identificados por ID estável (ex. `IMMDevice::GetId` no Windows),
  nunca por substring de nome — a auditoria do Meetily (seção 6, achado 7) sinalizou
  correspondência por nome como frágil.
- Não fabricar resultados de teste que não foram de fato executados neste ambiente
  (WSL2/Linux, sem hardware de áudio Windows/macOS real). Ao documentar validação,
  separar explicitamente "verificado neste sandbox" de "ainda precisa de confirmação
  manual" — ver `docs/windows-wasapi-loopback.md` seção 7 como modelo.
- Documentação em `docs/` é escrita em português (segue os documentos existentes:
  `meetily-audio-audit.md`, `third-party-components.md`).
