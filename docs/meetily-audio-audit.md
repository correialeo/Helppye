# Auditoria do código de áudio do Meetily

**Repositório auditado:** `meetily/` (clone local, não modificado durante esta auditoria)
**Commit auditado:** `0281737d87d26352fb0adc78c8c0975f691b23d1` (branch `fix/audio-mixing`, 2026-06-05 19:22:04 +0530)
**Escopo:** exclusivamente a implementação Tauri/Rust suportada em `frontend/src-tauri/src/audio/` (52 arquivos). O backend Python/FastAPI em `backend/` é explicitamente arquivado/não suportado segundo o próprio `meetily/CLAUDE.md` e **não** foi auditado em profundidade.
**Ambiente de auditoria:** WSL2 sem subsistema de áudio (sem ALSA, PulseAudio ou PipeWire; `cargo`/`rustc` não instalados neste sandbox; apenas `node`/`npm` disponíveis). Nenhum trecho de código foi compilado ou executado — esta auditoria é 100% leitura estática de código-fonte. Isso está declarado explicitamente para evitar qualquer alegação de teste que não ocorreu.

---

## 1. Visão geral

O Meetily "Community Edition" é um app desktop Tauri 2.x (núcleo Rust + frontend Next.js/React) que captura áudio de microfone e de saída do sistema, mixa as duas fontes para gravação, aplica VAD (Voice Activity Detection) para filtrar apenas trechos de fala antes de mandar para transcrição via Whisper, e oferece sumarização via LLMs (Ollama local, ou serviços externos).

A arquitetura de áudio segue **dois caminhos paralelos** a partir de um único `AudioPipelineManager` (`pipeline.rs`):

- **Caminho de gravação:** mixagem profissional (ducking por RMS, prevenção de clipping) do mic + áudio do sistema → salvo em disco via `RecordingSaver`.
- **Caminho de transcrição:** o mesmo áudio, filtrado por VAD (`silero_rs`), é enviado ao Whisper.

Esse desenho mistura as duas fontes de áudio *antes* de qualquer lógica de detecção de turno de fala — o oposto do que a especificação do Helppye exige para o MVP (pipelines de mic e sistema devem permanecer **separados** até a lógica de detecção de pergunta). Esse é o achado arquitetural mais importante desta auditoria e é discutido em detalhe na seção 6.

O módulo de áudio é grande (52 arquivos, ~30 submódulos declarados em `mod.rs`) e mostra sinais claros de evolução orgânica/refatoração incompleta: arquivos mortos (`core-old.rs`, `recording_saver_old.rs`, `recording_commands.rs.backup`), um arquivo TypeScript solto dentro do módulo Rust (`system_audio_types.ts`), abstrações duplicadas (`level_monitor.rs` vs. `simple_level_monitor.rs`), e um TODO explícito ("Extract microphone AudioStream logic from core.rs") confirmando que a extração de responsabilidades ainda está em andamento no próprio Meetily. A branch de trabalho atual é literalmente `fix/audio-mixing`, ou seja, o pipeline de áudio está em fluxo ativo, não estabilizado.

Também foi encontrada **divergência entre a documentação interna do Meetily (`CLAUDE.md`) e o código real**, em pelo menos dois pontos concretos (detalhados na seção 6): a janela de mixagem documentada como "50ms" é na verdade 600ms no código, e a captura de áudio de sistema no macOS é documentada como "ScreenCaptureKit + BlackHole" mas o código real usa a API mais nova de **Core Audio Process Tap** (via `cidre`), sem ScreenCaptureKit nem BlackHole. Isso reforça a necessidade de verificar sempre o código-fonte, não a documentação, antes de tomar decisões de reuso — o que foi feito nesta auditoria.

---

## 2. Mapa de arquivos

Responsabilidade e reusabilidade por arquivo/diretório (apenas os relevantes para captura/pipeline de áudio; arquivos de gravação-para-disco, banco de dados e UI foram listados mas não aprofundados por estarem fora do escopo do MVP do Helppye).

### `devices/` — descoberta e configuração de dispositivos
| Arquivo | Responsabilidade | Reusabilidade |
|---|---|---|
| `devices/configuration.rs` | Tipos `AudioDevice`, `DeviceType` | Referência (tipos simples, reescrever com nomes/campos próprios) |
| `devices/discovery.rs` | `list_audio_devices`, `trigger_audio_permission` | Adaptar (lógica de agregação por host `cpal`) |
| `devices/microphone.rs`, `devices/speakers.rs` | Dispositivo default de entrada/saída | Adaptar (triviais, ~10-20 linhas cada, esperado) |
| `devices/fallback.rs` | Fallback de dispositivo (tem testes) | Referência |
| `devices/platform/windows.rs` (257 linhas) | Enumeração/seleção de dispositivo via host WASAPI do `cpal`, com fallback em 3 camadas (WASAPI → host default → apenas default) | Adaptar — lógica sólida, mas seleção de dispositivo é por *substring de nome* (`name.contains(base_name)`), não por ID estável. Reescrever a lógica de identificação. |
| `devices/platform/macos.rs` (34 linhas) | Enumeração via `cpal` (apenas listagem — a captura real de sistema não usa `cpal`, ver `capture/core_audio.rs`); filtra "speakers" da lista de saída | Adaptar (pequeno, direto) |
| `devices/platform/linux.rs` (32 linhas) | Enumeração via host ALSA do `cpal`; identifica "áudio de sistema" apenas por *substring* `"monitor"` no nome do dispositivo (convenção de fontes monitor do PulseAudio) | **Não usar como está** — não há uso de API nativa do PipeWire nem de portals; é uma heurística de nome sobre dispositivos ALSA comuns. Ver seção 6. |

### `capture/` — streams de captura
| Arquivo | Responsabilidade | Reusabilidade |
|---|---|---|
| `capture/microphone.rs` | Stream de captura de microfone (contém `// TODO: Extract microphone AudioStream logic from core.rs` — extração incompleta) | Referência apenas — o arquivo real ainda delega para `core.rs`/`stream.rs` |
| `capture/system.rs` (152 linhas) | `SystemAudioCapture`; no macOS delega para `core_audio.rs`; usa canal **não-limitado** (`futures_channel::mpsc::unbounded`); para não-macOS, a função de start é um `anyhow::bail!("System audio capture not yet implemented for this platform")` explícito | Reescrever (canal não-limitado contradiz requisito de backpressure; stub não-macOS confirma que não há captura de sistema funcional para Windows/Linux por este caminho de código) |
| `capture/core_audio.rs` (445 linhas, macOS apenas) | Captura de áudio de sistema via **Core Audio Process Tap** (API introduzida no macOS 14.4+) usando o binding `cidre`, com ponte para Rust assíncrono via `ringbuf` (SPSC lock-free) + `Waker` | **Adaptar com atenção** — implementação não-trivial e funcional de uma API Apple recente e pouco documentada; maior candidato a reuso real desta auditoria. Contém comentário de correção histórica de bug de eco/áudio duplicado ("CRITICAL FIX: Use ONLY the tap, NOT the output device + tap") e nota de comportamento de permissão (tap retorna silêncio se a permissão `NSAudioCaptureUsageDescription` for negada, não erro). Exige macOS 14.4+, sem fallback documentado para versões anteriores. |
| `capture/backend_config.rs`, `capture/system_audio_stream.rs` | Configuração/streaming auxiliares (têm testes) | Referência |

### Pipeline central
| Arquivo | Responsabilidade | Reusabilidade |
|---|---|---|
| `pipeline.rs` (1079 linhas — maior arquivo do módulo) | `AudioMixerRingBuffer` (mixagem por janela com zero-pad em underrun, overflow descartando amostras antigas com `warn!`/`error!` diferenciados), `ProfessionalAudioMixer` (ducking por RMS), `AudioPipelineManager` (orquestra VAD + mixagem + distribuição) | **Não utilizar diretamente** — mistura mic+sistema antes da lógica de turno (contraria requisito do Helppye); contém `unsafe`/`static mut` sem justificativa de segurança (ver seção 6). O *padrão* de buffer circular limitado com descarte controlado e contagem de drops é uma referência conceitual válida para a política de backpressure do Helppye. **Sem testes unitários** apesar de ser o arquivo mais crítico do módulo. |
| `stream.rs` (482 linhas) | `StreamBackend` (`Cpal` ou `CoreAudio` no macOS), `AudioStream` — abstração de stream com `unsafe impl Send` documentado via comentário (mitigação alegada: uso de `spawn_blocking`, não verificado em todos os pontos de chamada) | Referência — a ideia de um backend enum abstraindo `cpal` vs. captura nativa é reaproveitável conceitualmente, mas os detalhes de `unsafe impl Send` precisam ser re-verificados caso a caso, não copiados como está. Sem testes unitários. |
| `vad.rs` (595 linhas) | `ContinuousVadProcessor` sobre `silero_rs`; constantes ajustadas empiricamente (limiares de fala, tempos de "redemption"/pre/post padding) com histórico comentado de bugs de fragmentação de fala já corrigidos | Adaptar (fase 3/5, fora do escopo imediato de captura) — os valores numéricos empíricos (ex.: `min_speech_time=250ms`, `pre_speech_pad=300ms`) são um dado de referência valioso mesmo numa reimplementação do zero. Tem testes. |
| `device_monitor.rs` | `DeviceEvent`, monitoramento de desconexão com contagem de "consecutive_missing" e limiar diferenciado para dispositivos Bluetooth | Adaptar — padrão de design reaproveitável (detecção de desconexão + tolerância maior para Bluetooth), reescrever a implementação. Tem testes. |
| `recording_manager.rs` | Orquestração de alto nível da gravação; `unsafe impl Send for RecordingManager {}` com comentário raso ("contains types that we've marked as Send" — não identifica quais campos nem por que é seguro) | Não utilizar — fora de escopo do Helppye (Helppye não grava/persiste áudio bruto), e a justificativa do `unsafe` é insuficiente para confiar sem reler todos os campos internos. Sem testes unitários. |
| `recording_saver.rs`, `incremental_saver.rs`, `ffmpeg_mixer.rs`, `encode.rs` | Gravação/codificação de áudio em disco | Não utilizar — Helppye não deve persistir áudio bruto em disco (requisito explícito do MVP) |
| `system_detector.rs` | Callback de propriedade do Core Audio (macOS) para detectar mudanças de dispositivo de sistema; contém `unsafe { std::slice::from_raw_parts(addresses, number_addresses as usize) }` dentro de um callback FFI (`extern "C"`), **sem comentário `// SAFETY:`** explicando o contrato do ponteiro/contagem entregues pelo Core Audio | Referência — padrão de callback FFI plausível e provavelmente correto (o Core Audio garante validade do ponteiro durante o callback), mas não documentado; se adaptado, deve ganhar um comentário de segurança explícito. Tem testes. |

### Diretórios explicitamente fora de escopo para a MVP do Helppye
`recording_state.rs`, `recording_commands.rs`, `recording_preferences.rs`, `retranscription/`, `import/`, `post_processor.rs`, `hardware_detector.rs` (relevante só na Fase 3, transcrição), `transcription/` — todos ligados a gravação-para-disco, banco de dados de reuniões, ou features (reimportação/retranscrição) que não fazem parte do MVP de captura ao vivo do Helppye.

### Achados de "code smell" no mapeamento de arquivos
- Arquivos mortos identificados por nome: `core-old.rs`, `recording_saver_old.rs`, `recording_commands.rs.backup` — nenhum deve ser lido como fonte de verdade.
- `system_audio_types.ts` dentro do módulo Rust — resíduo de acoplamento frontend/backend, irrelevante para Helppye.
- Duas implementações paralelas de monitoramento de nível de áudio (`level_monitor.rs`, `simple_level_monitor.rs`) — sinal de duplicação não resolvida.
- Superfície pública de `mod.rs` muito ampla (mais de 40 símbolos reexportados de ~30 submódulos) — indica acoplamento alto entre captura, mixagem, gravação, banco de dados e comandos Tauri, o que dificulta extrair "apenas a captura" sem arrastar o resto.
- **Cobertura de testes:** existem `#[cfg(test)]` em módulos periféricos (`vad.rs`, `device_monitor.rs`, `buffer_pool.rs`, `capture/core_audio.rs`, `system_detector.rs`, `decoder.rs`, entre outros), mas **nenhum teste unitário** foi encontrado em `pipeline.rs`, `stream.rs`, `recording_manager.rs`, `recording_saver.rs` ou `recording_commands.rs` — exatamente os arquivos mais centrais e mais arriscados do módulo.

---

## 3. Dependências

| Dependência | Finalidade | Licença | Suporte de SO | Maturidade | Risco | Veredito para o Helppye |
|---|---|---|---|---|---|---|
| `cpal` (git pin, rev `51c3b43`, não a versão publicada no crates.io) | I/O de áudio cross-platform (WASAPI/Core Audio/ALSA) | MIT OR Apache-2.0 | Win/macOS/Linux | Madura, mantida pela RustAudio org, mas o Meetily depende de um **fork/revisão não publicada** — sinal de correção ainda não lançada (provavelmente ligada a loopback) | Reprodutibilidade: fixar em git rev em vez de crates.io é frágil a longo prazo | **Usar** — base sólida para listagem de dispositivos e captura de microfone; Helppye deve decidir se fixa a mesma rev ou aguarda um release publicado, documentando a escolha |
| `cidre` (git, macOS apenas) | Bindings Rust para frameworks Apple (Core Audio Process Tap) | MIT (confirmado no repositório) | macOS apenas | Relativamente jovem, pinada em git rev, não crates.io estável | API pode mudar; exige macOS 14.4+ (Process Tap é recente) | Avaliar/adaptar para macOS quando essa plataforma for implementada — não prometer suporte sem testar em hardware real |
| `silero_rs`/`silero` (git, `emotechlab/silero-rs`) | VAD (Voice Activity Detection) | MIT (confirmado no repositório) | Multiplataforma (é um wrapper de modelo ONNX/Torch, não específico de SO) | Pré-1.0, pinado em git rev | Fase 3/5 do Helppye, não afeta a Fase 2 (captura) | Candidato para quando a detecção de fala entrar em pauta; não necessário agora |
| `whisper-rs` | Bindings para whisper.cpp | MIT | Win/macOS/Linux, com feature flags de GPU por SO | Madura, amplamente usada | Baixo | Fase 3 (transcrição), fora do escopo desta etapa |
| `rubato` | Resampling sinc de alta qualidade | MIT OR Apache-2.0 | Todas (pure Rust) | Madura | Baixo | **Usar** — encaixa diretamente no `resampler.rs` do Helppye |
| `ringbuf` | Buffer circular lock-free SPSC/MPSC | MIT OR Apache-2.0 | Todas (pure Rust) | Madura | Baixo | **Usar** — bom encaixe para "nunca bloquear o callback nativo de áudio" |
| `tokio-util` (`CancellationToken`) | Cancelamento cooperativo | MIT | Todas | Madura | Baixo | **Usar** — corresponde exatamente ao requisito de cancelamento explícito do Helppye |
| `async-trait` | Traits assíncronas em objetos | MIT OR Apache-2.0 | Todas | Madura | Baixo | **Usar** — necessário para o trait `AudioCaptureProvider` |
| `ebur128` | Normalização de loudness (EBU R128) | MIT | Todas | Nicho, mas estável | Baixo | Não incluir na MVP — Helppye não grava/normaliza arquivos de áudio |
| `nnnoiseless` | Supressão de ruído (RNNoise em Rust puro) | BSD-3-Clause | Todas (pure Rust) | Madura o bastante | Baixo | Adiar — possível melhoria futura no caminho de transcrição, não crítico agora |
| `symphonia` | Decodificação de múltiplos codecs de áudio | **MPL-2.0** (família de licença diferente — copyleft por arquivo) | Todas | Madura | Médio (obrigações de licença diferentes das demais, precisa atenção separada se usado) | Não incluir na MVP — só é usado para importação/retranscrição de arquivos, fora de escopo |
| `ffmpeg-sidecar` (git, branch main) | Spawna/gerencia binário externo `ffmpeg` | MIT | Todas (depende de binário externo instalado) | — | Adiciona dependência de binário externo | Não incluir — Helppye não codifica/salva arquivos de áudio |
| `esaxx-rs` (patch git, indireto via tokenização) | Dependência transitiva de tokenização (Whisper) | Verificar na origem | — | Fork git pinado | Baixo impacto direto (fase 3, não fase 2) | Reavaliar na Fase 3 |

**Observação geral sobre dependências pinadas em git:** `cpal` (patch), `cidre` e `silero_rs` não vêm do crates.io — todos são revisões de git fixas. Isso é uma escolha de engenharia legítima quando se depende de correções ainda não publicadas, mas é um risco de reprodutibilidade/manutenção que deve ser documentado explicitamente no Helppye se práticas semelhantes forem adotadas (não copiar a prática sem essa documentação).

---

## 4. Partes reutilizáveis

Classificação por componente (reutilizar diretamente / adaptar / reescrever / não utilizar):

| Componente | Classificação | Justificativa |
|---|---|---|
| Enumeração de dispositivos via `cpal` (Windows/macOS) | **Adaptar** | Lógica correta e enxuta (34-257 linhas), mas com tipos e nomes de campo específicos do Meetily; a seleção de dispositivo no Windows é por substring de nome, deve ser reescrita para usar identificação mais estável quando disponível |
| Enumeração de dispositivos no Linux (`monitor` substring) | **Reescrever** | Não há uso real de API do PipeWire; é apenas uma convenção de nome sobre ALSA/Pulse. O Helppye deve investigar a API nativa do PipeWire (conforme já orientado na especificação original), não herdar esta heurística como solução definitiva |
| Captura de sistema no macOS via Core Audio Process Tap (`capture/core_audio.rs`) | **Adaptar (com atribuição)** | Único componente do módulo cuja reimplementação do zero teria custo alto e baixo benefício relativo — é uma integração funcional e não-trivial (445 linhas) com uma API Apple recente e mal documentada publicamente. Deve ser isolado da mixagem/gravação, reconstruído em torno de canais limitados, e testado em hardware macOS 14.4+ real antes de ser confiado (não testável neste ambiente) |
| Captura de sistema no Windows/Linux (`capture/system.rs`) | **Reescrever** | O caminho não-macOS deste arquivo específico é um `bail!` explícito — não há nada funcional para adaptar aqui. A captura real de sistema no Windows provavelmente depende de abrir um dispositivo de saída em modo loopback via WASAPI (o padrão em `devices/platform/windows.rs` é uma referência válida), o que deve ser reimplementado diretamente para Helppye |
| `AudioMixerRingBuffer` / mixagem mic+sistema (`pipeline.rs`) | **Não utilizar** | Contraria o requisito central do MVP do Helppye de manter mic e sistema em pipelines separados até a lógica de detecção de turno. O *padrão* de buffer limitado com descarte controlado (não o código) é uma referência válida para a política de backpressure |
| Canais de comunicação entre callback de áudio e consumidores | **Reescrever** | 48 ocorrências de canais não-limitados (`unbounded`) no módulo — contradiz diretamente o requisito de canais limitados do Helppye. Nenhuma adaptação resolve isso; o design de canais do Helppye deve ser feito do zero com `tokio::sync::mpsc::channel(N)` e política de descarte explícita |
| VAD (`vad.rs`, wrapper de `silero_rs`) | **Adaptar (mais adiante, Fase 3/5)** | Fora do escopo da captura (Fase 2), mas as constantes empíricas de ajuste (limiares, tempos de padding) são conhecimento de referência valioso a preservar em comentários quando o Helppye implementar sua própria segmentação de fala |
| Monitoramento de desconexão de dispositivo (`device_monitor.rs`) | **Adaptar** | O padrão (contagem de falhas consecutivas + tolerância maior para Bluetooth) é razoável e portável; a implementação deve ser reescrita contra os tipos próprios do Helppye (`AudioCaptureEvent::DeviceDisconnected`) |
| Resampling (uso de `rubato` dentro de `pipeline.rs`) | **Adaptar** | Uso direto e simples da API do `rubato`; reaproveitável como base do `resampler.rs` do Helppye |
| Subsistema de gravação/salvamento em disco (`recording_saver.rs`, `incremental_saver.rs`, `ffmpeg_mixer.rs`, `encode.rs`) | **Não utilizar** | Contraria o requisito explícito do Helppye de nunca persistir áudio bruto em disco; é justamente a funcionalidade central do Meetily (gravar reuniões), o oposto do propósito do Helppye |
| `unsafe impl Send` diversos (`stream.rs`, `recording_manager.rs`) | **Não utilizar como está** | Justificativas insuficientes/rasas em pelo menos um caso (`recording_manager.rs`); mesmo o caso com comentário mais completo (`stream.rs`) depende de disciplina em todos os pontos de chamada, não verificável isoladamente |

---

## 5. Licenciamento

*Aviso: esta seção é documentação técnica de obrigações de licença observadas no código-fonte, não parecer jurídico.*

- **Licença do próprio Meetily:** MIT License, `Copyright (c) 2024 Zackriya Solutions` (confirmado por leitura direta de `meetily/LICENSE.md`). A licença MIT permite uso, cópia, modificação e distribuição livres, inclusive em software proprietário, com duas obrigações principais: (1) preservar o aviso de copyright e o texto da licença em cópias ou partes substanciais do software; (2) o software é fornecido "como está", sem garantias.
- **Obrigação prática para o Helppye:** qualquer arquivo ou trecho de código copiado/adaptado do Meetily deve manter um cabeçalho de comentário citando a autoria original (Zackriya Solutions) e referenciar o texto da licença MIT, além de ser registrado em `docs/third-party-components.md` (criado nesta sessão) e refletido em `NOTICE` (também criado). A licença MIT não é copyleft — não exige que o Helppye seja distribuído sob a mesma licença nem que o Helppye seja open source.
- **Atribuição transitiva a montante:** o próprio README do Meetily divulga, na seção "Acknowledgments", que o projeto **"borrowed some code from Whisper.cpp... Screenpipe (mediar-ai)... transcribe-rs"**, além de créditos a NVIDIA (Parakeet) e a istupakov (conversão ONNX). Isso significa que a licença MIT do repositório Meetily cobre apenas o código de autoria do próprio Meetily — trechos emprestados dessas outras fontes carregam as licenças originais desses projetos, que não foram auditadas individualmente aqui (fora do escopo desta auditoria de áudio, já que essas menções dizem respeito majoritariamente a transcrição/Parakeet, não à captura de áudio em si). Caso o Helppye venha a adaptar qualquer código de transcrição do Meetily no futuro, essa cadeia de atribuição precisará ser re-verificada na origem antes de copiar.
- **Dependências de terceiros usadas pelo Meetily têm suas próprias licenças, independentes da licença MIT do repositório Meetily:** `cidre` (MIT, confirmado via repositório GitHub), `silero-rs` (MIT, confirmado via repositório GitHub), `cpal` (MIT OR Apache-2.0), `rubato`/`ringbuf` (MIT OR Apache-2.0), `symphonia` (MPL-2.0 — família de licença diferente, com obrigações por arquivo caso um arquivo dela seja modificado e redistribuído), `nnnoiseless` (BSD-3-Clause). Se o Helppye passar a depender diretamente de algum desses crates (o que é provável para `cpal`, `rubato`, `ringbuf`, `tokio-util`, `async-trait`), essas licenças passam a ser obrigações diretas do Helppye como consumidor do crate via Cargo — não como "código copiado do Meetily" — e são satisfeitas automaticamente pelo próprio ecossistema Cargo (licenças de dependências publicadas não exigem NOTICE manual da forma como código copiado exige), mas vale documentá-las para transparência.
- **Submódulo `backend/whisper.cpp`:** aponta para um fork da Zackriya Solutions do `whisper.cpp` (branch `develop`), sob a licença MIT original do whisper.cpp a montante. Esse submódulo pertence ao backend Python arquivado/não suportado e não é relevante para a linha de captura de áudio do Helppye.

**Conclusão da seção:** não há impedimento legal técnico para adaptar trechos específicos do Meetily (Strategy B) desde que a atribuição MIT seja preservada via cabeçalho + `docs/third-party-components.md` + `NOTICE`, o que foi feito nesta sessão para o único componente recomendado para adaptação (Core Audio Process Tap, macOS).

---

## 6. Riscos técnicos

Riscos concretos identificados por leitura direta do código, em ordem aproximada de relevância para o Helppye:

1. **Canais não-limitados (unbounded) generalizados.** 48 ocorrências de `mpsc::unbounded`/`UnboundedSender`/`UnboundedReceiver` no módulo de áudio, incluindo no caminho de captura de sistema no macOS (`capture/system.rs`). Isso viola diretamente o requisito central do Helppye de backpressure com canais limitados — se um consumidor travar, a memória pode crescer sem limite. Nenhuma parte do sistema de canais deve ser adaptada como está.

2. **`unsafe`/`static mut` sem comentário de segurança em caminho de mixagem quente.** Em `pipeline.rs`, dentro de `AudioMixerRingBuffer::add_samples()`, existe um `static mut SAMPLE_COUNTER` mutado dentro de um bloco `unsafe`, sem nenhum comentário `// SAFETY:`, aparentemente acessível a partir de threads de callback de áudio potencialmente concorrentes (mic e sistema). Isso é uma condição de corrida de dados (data race) tecnicamente é UB em Rust, ainda que de baixo impacto prático (o contador só é usado para log a cada 200 amostras). É um exemplo concreto de "unsafe não documentado" que o Helppye deve evitar replicar — usar `AtomicU64` resolveria isso sem `unsafe`.

3. **`unsafe impl Send` com justificativa rasa.** Em `recording_manager.rs`: `unsafe impl Send for RecordingManager {}` com o comentário "contains types that we've marked as Send" — um comentário circular que não identifica os campos específicos nem explica por que a implementação é de fato segura. Em `stream.rs`, o mesmo padrão tem uma justificativa mais completa (mitigação via `spawn_blocking`), mas depende de disciplina em todos os pontos de chamada do resto do módulo, o que não é verificável isoladamente. Nenhum dos dois deve ser copiado sem reescrever a justificativa e revalidar cada campo.

4. **Suporte de áudio de sistema no Linux é heurístico, não uma implementação real.** `devices/platform/linux.rs` apenas casa a substring `"monitor"` no nome de dispositivos ALSA (convenção do PulseAudio) — não há uso de API nativa do PipeWire nem de portals de captura. Além disso, `capture/system.rs::start_system_audio_capture()` (ou equivalente) retorna erro explícito (`bail!`) para qualquer SO que não seja macOS neste caminho de código específico. Isso significa que **não existe, no código lido, uma implementação funcional e testada de captura de áudio de sistema para Linux no Meetily** — apenas uma convenção de nomenclatura que depende do usuário já ter uma fonte monitor configurada manualmente. O Helppye precisa investigar a API nativa do PipeWire do zero, como já orientado na especificação original do projeto.

5. **Divergência entre documentação interna do Meetily (`CLAUDE.md`) e código real.** Dois exemplos concretos: (a) `CLAUDE.md` descreve a janela de mixagem como "50ms", mas o código real de `pipeline.rs` define `window_ms = 600.0`; (b) `CLAUDE.md` descreve a captura de sistema no macOS como usando "ScreenCaptureKit" + "virtual audio device (BlackHole)", mas o código real (`capture/core_audio.rs`) usa a API de **Core Audio Process Tap** via `cidre`, sem ScreenCaptureKit nem BlackHole — inclusive há um comentário no próprio `devices/platform/macos.rs` esclarecendo isso ("Core Audio backend uses direct cidre API for system capture, not cpal"). Isso confirma que a documentação do Meetily não deve ser tratada como fonte de verdade — apenas o código.

6. **Requisito de macOS 14.4+ para captura de sistema, sem fallback documentado.** A API de Process Tap usada em `capture/core_audio.rs` é recente (macOS 14.4+). Não há tratamento explícito para versões anteriores do macOS nesse arquivo. O Helppye deve decidir e documentar sua própria política mínima de versão de SO, em vez de presumir suporte universal.

7. **Seleção de dispositivo por substring de nome (Windows), não por ID estável.** Em `devices/platform/windows.rs::get_windows_device()`, a correspondência de dispositivo usa `name == base_name || name.contains(base_name)` — frágil se dois dispositivos tiverem nomes parecidos, e não sobrevive a uma reconexão que troque a ordem/nome do dispositivo. O Helppye deve preferir identificadores estáveis quando a plataforma os fornecer.

8. **Ausência de testes unitários nos arquivos mais críticos.** `pipeline.rs`, `stream.rs`, `recording_manager.rs`, `recording_saver.rs` e `recording_commands.rs` não têm nenhum `#[cfg(test)]`, apesar de serem os arquivos com maior complexidade e maior risco (mixagem, threads, unsafe). Módulos periféricos (VAD, monitoramento de dispositivo, detecção de hardware) têm testes. Isso reforça que a parte mais arriscada do código é também a menos coberta — não deve ser copiada com confiança cega.

9. **Acoplamento profundo entre captura, mixagem, VAD, persistência em banco/disco e comandos Tauri.** A superfície pública de `mod.rs` reexporta mais de 40 símbolos de ~30 submódulos. Extrair "apenas a captura" exigiria cirurgia significativa — não é uma extração de crate trivial (ver seção 7, isso pesa contra a Estratégia A).

10. **Evidência de código morto e refatoração incompleta.** Arquivos como `core-old.rs`, `recording_saver_old.rs`, `recording_commands.rs.backup`, e um TODO explícito de extração incompleta (`capture/microphone.rs`) mostram que o próprio Meetily está no meio de uma reestruturação — reforça a cautela em tratar qualquer arquivo isolado como "a" implementação de referência sem verificar se ele é de fato usado a partir de `mod.rs`.

11. **Dependências git-pinadas fora do crates.io (`cpal` patch, `cidre`, `silero_rs`).** Sinalizam APIs pré-1.0/instáveis e correções ainda não publicadas oficialmente — risco de manutenção a longo prazo, mas não um bloqueador imediato.

12. **Problema de playback Bluetooth documentado (`BLUETOOTH_PLAYBACK_NOTICE.md`).** É um bug de *reprodução* (não de gravação), mas é um lembrete real de que dispositivos Bluetooth têm comportamento de codec problemático — vale considerar ao desenhar o tratamento de dispositivos Bluetooth no Helppye (o padrão de tolerância diferenciada em `device_monitor.rs` já reconhece isso parcialmente).

---

## 7. Recomendação final

Nenhuma das três estratégias (A, B, C) descreve sozinha e com precisão o que o código real justifica. A recomendação é a seguinte, com uma exceção pontual claramente delimitada:

### Estratégia padrão: **C — usar como referência, reimplementar de forma independente**

Justificativa: o módulo de áudio do Meetily é grande, fortemente acoplado (captura + mixagem + gravação + persistência + comandos Tauri em ~30 submódulos com reexportação ampla), usa canais não-limitados de forma pervasiva (violação direta do requisito central de backpressure do Helppye), mistura mic+sistema antes de qualquer lógica de turno (o oposto do que o MVP do Helppye exige), tem pelo menos um `unsafe` genuinamente não-documentado em caminho quente, e carece de testes exatamente nos arquivos mais críticos. Extrair um crate inteiro (Estratégia A) exigiria cirurgia tão extensa que equivaleria, na prática, a reescrever a maior parte do código de qualquer forma — sem o benefício de herdar uma arquitetura de canais compatível. Para os arquivos pequenos e simples (enumeração de dispositivos Windows/macOS/Linux, 32-257 linhas cada), reimplementar do zero contra os tipos e o `AudioCaptureProvider` do Helppye é pouco mais trabalhoso do que adaptar, e produz código já alinhado ao contrato de canais limitados desde o primeiro commit.

### Exceção pontual: **Estratégia B — adaptar com atribuição** para `frontend/src-tauri/src/audio/capture/core_audio.rs` (macOS, Core Audio Process Tap)

Justificativa: é o único componente do módulo cuja reimplementação do zero teria custo desproporcional ao benefício — 445 linhas de integração funcional com uma API Apple recente (macOS 14.4+), pouco documentada publicamente, incluindo uma correção de bug real já resolvida (eco/áudio duplicado) e uma nota de comportamento de permissão não-óbvia (tap retorna silêncio, não erro, quando a permissão é negada). Quando o Helppye chegar à implementação da plataforma macOS, este arquivo deve ser adaptado — não copiado literalmente — isolando-o completamente da mixagem/gravação do Meetily, reconstruindo sua interface de saída em torno de um canal limitado (`tokio::sync::mpsc::channel`), e **testado em hardware macOS 14.4+ real antes de ser confiado**, já que este ambiente de auditoria (WSL2) não permite nenhum teste de áudio. A adaptação deve ser registrada em `docs/third-party-components.md` com atribuição preservada, conforme já feito nesta sessão.

### O que não deve ser adaptado em nenhuma circunstância
Canais não-limitados; o subsistema de mixagem mic+sistema (`AudioMixerRingBuffer`/`ProfessionalAudioMixer`); o subsistema de gravação/salvamento em disco; os `unsafe impl Send` como estão escritos; a heurística de captura de sistema no Linux (não há nada funcional ali para adaptar).

### Conhecimento de referência a preservar (sem copiar código)
Os valores empíricos de ajuste de VAD (`vad.rs`) e o padrão de tolerância a desconexão diferenciada para Bluetooth (`device_monitor.rs`) são conhecimento de engenharia valioso, obtido por iteração real contra bugs de produção, e devem informar o design do Helppye nas fases 3 e 5, mesmo sendo reimplementados do zero.
