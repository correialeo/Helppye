# Helppye

Copiloto de reuniões em tempo real. Núcleo Tauri 2 (Rust) + frontend React/TypeScript.
Local-first por padrão: transcrição local (Whisper) e geração local (Ollama). A escolha do
transcritor e a do gerador são **eixos independentes**: o usuário pode optar
explicitamente por um provedor de nuvem para a geração (OpenAI, DeepSeek, Anthropic,
OpenRouter ou qualquer endpoint compatível com a API da OpenAI) sem que isso mexa na
transcrição — nesse caso a API key fica no keychain do SO, nunca em texto puro no disco.

## Status

Fundação de áudio e transcrição local implementada. A captura de microfone, a captura de
saída do sistema no Windows via WASAPI Loopback, o pipeline de VAD/segmentação e a
transcrição local com Whisper Base Multilíngue já existem. A Conversation Timeline
organiza transcrições em uma linha do tempo única preservando ordem, origem
(usuário/outra pessoa) e timestamps. Um timer dedicado por utterance
(`ConversationTimeline::reschedule_utterance_timer`) finaliza uma utterance só por
silêncio (`same_speaker_utterance_gap_ms`), sem esperar um novo segmento, flush manual,
parada de captura ou o timeout de inatividade do turno — a camada atual em construção é
a **sugestão de resposta em streaming** (`src-tauri/src/response_provider/`): turnos
elegíveis da outra pessoa disparam geração de uma sugestão de resposta via LLM (Ollama/
OpenAI/DeepSeek/Anthropic, escolhido pelo usuário) automaticamente assim que essa
utterance finaliza, transmitida ao frontend em streaming. Substitui a antiga detecção de
perguntas por regras, removida. Overlay de resposta dedicado (janela flutuante fora da
timeline) ainda **não** está implementado.

O pipeline hoje tem sete estágios, cada um com um contrato próprio:

```
Audio Capture → Transcription Provider → Transcript Normalization → Conversation Timeline
→ Context Builder → Response Provider → Suggestion
```

Transcrição e geração são **subsistemas plugáveis independentes**, cada um com registry,
capacidades declaradas e configuração separada (`TranscriptionSettings` /
`ResponseSettings`). Ver `docs/transcription-providers.md`,
`docs/transcript-normalization.md`, `docs/response-suggestion.md` e `docs/telemetry.md`.

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

- `src/` — frontend React/TypeScript, organizado por domínio (não um `App.tsx` único):
  `app/` (App.tsx só faz init/providers/error boundary; `router.tsx` decide qual tela
  renderizar a partir de `useOnboardingStore.screen`; `appFlow.ts` define o tipo
  `AppScreen` e a lógica pura de sequência/resumo do onboarding), `components/ui|
  layout|feedback` (primitivos visuais), `features/` (uma pasta por tela: welcome,
  profile, language, permissions, audio-setup, ai-provider, onboarding-review, ready,
  session, settings, developer-tools), `hooks/`, `services/` (wrappers tipados sobre
  `invoke`/`listen`), `stores/` (Zustand), `types/`, `utils/`. Ver
  `docs/frontend-architecture.md` para a estrutura completa e `docs/design-system.md`
  para os princípios visuais. Fluxo: boas-vindas → perfil → idioma → permissões →
  teste de áudio (que também cobre o download do modelo de transcrição, sem baixar
  nada silenciosamente) → provedor de IA → revisão → pronto → sessão (janela compacta
  focada na sugestão) — ver `docs/onboarding.md` e `docs/session-experience.md`.
  Diagnósticos técnicos (turnos, latência, eventos brutos) só aparecem atrás de "Modo
  de desenvolvedor" em Configurações, nunca na experiência normal.
- `src-tauri/` — núcleo Rust (comandos Tauri, pipeline de áudio). Ver seção "Módulo de
  áudio" abaixo.
- `docs/` — auditoria de arquitetura, notas de design, roadmap, incluindo
  `transcription-providers.md`, `transcript-normalization.md`, `response-suggestion.md`,
  `telemetry.md`, `frontend-architecture.md`, `design-system.md`, `onboarding.md` e
  `shortcuts.md`. Ler antes de tocar em captura de áudio, no frontend ou em qualquer
  coisa relacionada ao Meetily. `docs/adr/` guarda decisões de arquitetura com
  consequência de longo prazo (hoje: autenticação por assinatura ChatGPT/Codex).
- `benchmarks/` — harness de comparação entre transcritores (fixtures + instruções). O
  binário é `src-tauri/src/bin/benchmark.rs`.
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

## Transcrição (`src-tauri/src/transcription/`, `src-tauri/src/model_manager/`)

Há **dois contratos**, com papéis diferentes, e confundi-los é o erro mais fácil de
cometer aqui:

- `segment_transcriber::SegmentTranscriber` — inferência batch de um `AudioSegment`. É o
  que o whisper.cpp faz de fato e o que o `model_manager` carrega/descarrega.
  `WhisperCppProvider` (em `whisper_provider.rs`) o implementa, CPU-only.
- `provider::TranscriptionProvider` + `session::TranscriptionSession` — o ponto de
  extensão da aplicação: abre sessões por fonte, declara `TranscriptionCapabilities`,
  entra no `registry::TranscriptionProviderRegistry`.
  `whisper_local::WhisperLocalTranscriptionProvider` é o adaptador entre os dois.

O contrato batch sozinho não descreve um backend de streaming (que recebe áudio contínuo,
emite parciais e tem ciclo de vida próprio); encaixá-lo no molde batch obrigaria a
acumular áudio e jogar os parciais fora, perdendo justamente a latência que motivaria
usá-lo. `OpenAiRealtime`, `GoogleGemini` e `OpenAiCompatible` estão **representados no
registry e não implementados**, com `unavailable_reason` real — nenhum endpoint é
inventado e nenhum provider finge funcionar. Ver `docs/transcription-providers.md`.

`runtime::TranscriptionRuntime` cuida do ciclo de vida: uma sessão de transcrição por
fonte por sessão de conversa, e descarte **no backend** (nunca só no frontend) de todo
evento que não pertence ao estado atual, por três chaves — `session_id` (sessão anterior),
`transcription_session_id` (sessão de transcrição substituída) e `provider_event_id`
(reentrega/duplicata, janela `DEDUPE_WINDOW = 512`). A ordem do encerramento é parte do
isolamento: bloquear chunks novos → invalidar identidade → cancelar providers → limpar
buffers. Falha de uma fonte **não** derruba a outra: quem faz a pergunta é a outra pessoa,
pela saída do sistema, e perder isso por causa de um microfone com problema é o pior
desfecho possível.

`events::TranscriptionEvent` (`Partial`/`Final`/`SpeechStarted`/`SpeechEnded`/`Error`) é a
forma normalizada que todo provider reporta, sempre com identidade completa
(`session_id`, `transcription_session_id`, `source`, `provider`, `language`, `text`,
`started_at`, `ended_at`, `confidence`, `is_final`, `provider_event_id`).

`settings::TranscriptionSettings` (provider, `LanguageCode`, modelo) é **separado** de
`response_provider::settings::ResponseSettings`. São eixos independentes: transcrever
local e gerar na nuvem é o caso mais comum, e um único campo "provedor de IA" tornaria
essa combinação inexprimível.

`TranscriptionQueue` recebe `AudioSegment`s do pipeline de áudio em uma fila limitada e
não bloqueante; se a transcrição ficar atrasada, segmentos novos são descartados e
contabilizados, sem aplicar backpressure à captura.

O whisper anota trechos **sem fala** em vez de devolver texto vazio (`[Música]`,
`[BLANK_AUDIO]`, `[Aplausos]`, `♪`). `strip_non_speech_annotations` remove essas marcações
ainda no provider — é lá que o vocabulário de anotação de um transcritor específico é
conhecido, não na timeline. Sem isso a marcação vira segmento, abre uma utterance nova no
mesmo turno, cancela a geração de resposta em andamento da pergunta anterior e é então
classificada como `[SKIP]`: o usuário via "Nenhuma sugestão" justamente na fala que pedia
resposta. Ver `docs/response-suggestion.md`, seção "Ruído do transcritor não pode virar
fala".

`model_manager` implementa o fluxo guiado de primeiro uso: status do modelo, download
explícito, progresso, cancelamento, verificação de SHA-256, instalação atômica,
persistência da seleção e carregamento real no provider. O modelo padrão é o Whisper Base
Multilíngue; modelos personalizados podem ser selecionados por caminho local e são
validados antes de persistir. **O carregamento do modelo no provider acontece uma vez no
boot** (`lib.rs`, `.setup()`), não como efeito colateral de uma tela: o arquivo sobrevive
ao restart, o estado em memória do provider não, e amarrar essa restauração ao
`AudioSetupScreen` fazia toda transcrição falhar em silêncio a partir da segunda execução
do app.

## Normalização de transcrição (`src-tauri/src/normalization/`)

Camada entre o `Final` do provider e a Conversation Timeline. Antes dela, o único
tratamento era colapsar espaços, e o prompt recebia `"micro serviços"`, `"ddd"`,
`"rabbit mq"` — defeitos que mudam o que o modelo entende, numa conversa técnica.

Duas regras definem o escopo e explicam o que a camada **não** faz:

1. **Determinística e barata.** Nada de modelo, rede ou disco. Ela roda no caminho crítico
   entre o resultado do provider e a timeline; qualquer I/O aqui seria somado exatamente
   na métrica de UX (`speech_ended → first_visible_token`) e pago em toda fala, inclusive
   nas que terminam em `[SKIP]`.
2. **Não altera sentido.** O vocabulário é uma lista **fechada e configurável**
   (`vocabulary.rs`: DDD, SOLID, Docker, Kubernetes, microserviços, monólito, Entity
   Framework, RabbitMQ, Bling, Stripe), casada por palavra inteira, insensível a caixa e
   acento, alias mais longo primeiro. Uma correção global agressiva transformaria fala
   legítima em outra coisa — e a versão corrompida seria a única que a timeline veria.

Ordem das etapas (não é arbitrária): espaços → pontuação repetida → vocabulário →
capitalização. Capitalizar antes do vocabulário faria `"ddd"` virar `"Ddd"` e deixar de
casar.

**O texto original nunca é descartado**: `TranscriptNormalizationResult` carrega
`raw_text`, `normalized_text` e `normalization_changes` (com tipo, antes e depois), e os
três chegam ao `TranscriptSegment`. Diagnóstico usa o bruto; prompt usa o normalizado.

`TranscriptCorrectionMode` é `Disabled` / `DeterministicOnly` (**default**) / `Contextual`.
O modo contextual está contratualmente representado e **deliberadamente não ligado** ao
caminho crítico: mandar cada transcrição para um LLM "consertar" custaria uma chamada
inteira antes da geração e daria ao corretor licença para reescrever a pergunta. Sem
corretor registrado, `Contextual` se comporta como `DeterministicOnly` com `warn`, em vez
de bloquear a transcrição esperando algo que não existe. Ver
`docs/transcript-normalization.md`.

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

**Fechamento de utterance é dirigido por timer, não só por segmento novo.** A avaliação
de `same_speaker_utterance_gap_ms` descrita acima é reativa (compara o gap só quando um
segmento novo chega) e sozinha não bastava: se a pessoa parar de falar e mais nada
acontecer (nenhum segmento novo, sem flush, sem stop, sem fala do usuário), a utterance
ficava aberta indefinidamente, e com ela a sugestão de resposta nunca disparava. Por
isso, `ConversationTimeline` também mantém um timer assíncrono dedicado por utterance
(`reschedule_utterance_timer`/`fire_utterance_timeout`, usando `tokio::time::sleep`):
toda vez que um segmento novo abre ou estende a utterance aberta, o timer anterior é
implicitamente descartado (comparação de `ConversationUtterance::revision`, incrementada
a cada segmento anexado — nenhum `CancellationToken` explícito é necessário, já que o
`Mutex` do assembler garante que um timer expirado e obsoleto só encontra um estado que
não bate mais e vira no-op) e um novo timer de `same_speaker_utterance_gap_ms` é
agendado. Se ele expirar sem que a utterance tenha mudado de revisão, ela finaliza
sozinha (`UtteranceFinalizationReason::InactivityTimeout`) — **sem fechar o turno**, que
continua aberto para agrupamento conversacional até seu próprio timeout (ainda avaliado
só reativamente) ou outro evento de fechamento. `same_speaker_utterance_gap_ms` é
configurável em runtime via `ConversationTimeline::set_utterance_gap_ms` (comandos
`conversation_get_utterance_gap_ms_command`/`conversation_set_utterance_gap_ms_command`,
expostos no frontend só atrás de "Modo de desenvolvedor", em Configurações →
`DeveloperToolsScreen`) para testar valores diferentes sem rebuild.

Ao unir texto, a camada remove apenas espaços duplicados, adiciona um espaço entre
trechos e preserva a pontuação produzida pelo transcritor. Não há correção semântica,
reescrita, LLM, resumo ou troca de modelo nesta camada.

Eventos emitidos via `conversation://timeline-event`:
`utterance_started`/`utterance_updated`/`utterance_finalized` e
`turn_started`/`turn_updated`/`turn_finalized`. `utterance_finalized` carrega, além da
`ConversationUtterance` completa (que agora inclui `revision`): `finalization_reason`
(`UtteranceFinalizationReason` — `inactivity_timeout`, `speaker_changed`,
`source_changed`, `capture_stopped`, `manual_flush`, `session_ended`,
`maximum_duration`), `gap_ms_used`, `silence_detected_ms` (quando mensurável) e
`session_id`. Os eventos de fronteira `session_ended`/`session_started` marcam a troca de
sessão, nessa ordem. A Timeline também expõe `conversation_timeline_snapshot_command`
(snapshot com `turns` e `utterances`), `conversation_flush_turns_command`,
`conversation_start_session_command`, `conversation_end_session_command` e
`conversation_raw_segments_command`. Iniciar e encerrar sessão trocam o `SessionId`
(`reset_for_new_session` limpa **todas** as coleções: segmentos brutos, utterances,
turnos, aberturas por source e estado de timer) e propagam a fronteira para o
`ResponseEngine` (`begin_session`/`end_session`) — ver `docs/response-suggestion.md`,
seção "Isolamento por sessão". Um timer de utterance agendado na sessão anterior compara
`session_id` e `revision` antes de finalizar qualquer coisa, então nunca finaliza nada na
sessão nova.

Os timestamps dos segmentos são convertidos pelo `CaptureEngine` para um relógio
monotônico comum do processo antes de entrar na fila de transcrição, para que falas de
microfone e saída do sistema sejam comparáveis na mesma linha do tempo. O
`ResponseEngine` (`response_provider::engine::process_conversation_events`) consome
`ConversationTurn`, não `AudioFrame`, `AudioSegment` ou eventos brutos de transcrição —
disparado tanto pela finalização reativa quanto pela finalização via timer, pelo mesmo
caminho (`emit_conversation_events` + `process_conversation_events`, registrado como
`ConversationEventSink` em `ConversationTimeline::set_event_sink` para o caso do timer,
que não tem um chamador síncrono externo para fazer isso manualmente como os comandos
Tauri fazem).

## Sugestão de resposta (`src-tauri/src/response_provider/`)

Substitui a antiga detecção local de perguntas por regras (`question_detection.rs`,
removida). Em vez de apenas sinalizar que um turno da outra pessoa parece uma pergunta,
o `ResponseEngine` gera, via LLM e em streaming, uma sugestão real de resposta. Roda
sobre o mesmo `ConversationTurn` elegível de antes (`speaker = OtherPerson`,
`source = SystemOutput`), disparando geração em `UtteranceFinalized` — que agora dispara
de fato assim que a utterance finaliza por silêncio (via o timer dedicado descrito
acima), sem depender de flush, stop, fim de turno ou fala do usuário. Uma nova utterance
no mesmo turno cancela e substitui a geração em andamento, para nunca sugerir resposta a
uma fala que ainda não terminou; `ResponseEngine::finish_generation` roda em todo caminho
de saída (completo, skip, erro ou cancelamento) e sempre libera o slot de geração daquele
turno, para que uma geração seguinte nunca veja um estado "fantasma" de uma anterior já
encerrada. Só `inactivity_timeout`, `manual_flush` e `maximum_duration` disparam geração
(`engine::triggers_generation`): teardown (`capture_stopped`, `session_ended`) nunca
dispara, e `speaker_changed`/`source_changed` também não — numa utterance da outra pessoa
esses dois motivos significam que o microfone começou a falar, ou seja, **o usuário tomou
a palavra**. Ele já está respondendo, e gerar aí substituía token a token a sugestão que
ele estava lendo em voz alta (com a fala dele recém-entrada no contexto como `Você: ...`,
o modelo costumava devolvê-la de volta).

**Isolamento por sessão.** A unidade de isolamento é a sessão (`conversation::SessionId`,
monotônico, de propriedade da `ConversationTimeline`; o `ResponseEngine` espelha o valor).
Todo o estado mutável do motor vive num único `Mutex<SessionState>` (`session_id`, flag
`ending`, `CancellationToken` raiz, histórico, gerações ativas). `begin_session` instala
um estado inteiramente novo — token raiz novo (nunca um cancelado), histórico vazio; cada
geração roda sob um token **filho** do raiz. `end_session` é atômico, ordenado e
idempotente: marca `ending`, cancela o raiz, marca cada geração como já terminal e limpa
o histórico, tudo sob o mesmo lock. O `session_id` é validado em quatro pontos — no
gatilho, ao entrar/ler o histórico, antes de **cada** emissão de evento
(`is_publishable`), e no timer de utterance da timeline. Evento de sessão encerrada é
descartado no backend, com log `debug`, não no frontend. Provider e `reqwest::Client` são
preservados entre sessões de propósito; conteúdo conversacional, nunca.

**Montagem de contexto.** `context::ResponseContextBuilder` é um trait separado do motor,
com `DefaultResponseContextBuilder` como implementação. Os dois mudam por motivos
diferentes: o motor cuida de sessão, cancelamento e streaming (invariantes de produto), o
prompt é a parte que se ajusta por experimentação. `ResponseContextInput` é um struct
**fechado** e não `&ResponseEngine` — o que não está nele é a lista do que não pode entrar
no prompt (diagnósticos, IDs, sugestões anteriores, texto bruto, histórico de outra
sessão), e um builder novo não tem como incluir nada disso por engano porque não recebe.
Tetos: 2 utterances remotas anteriores, 1 resposta do usuário, 3000 caracteres de
contexto, utterance remota atual isolada, 160 tokens de saída, `temperature = 0.2`.

**Registry e endpoints.** `registry.rs` guarda **descritores** (não instâncias, ao
contrário do de transcrição: um provedor de geração é reconstruído a cada troca de
configuração, e guardar instâncias abriria a porta para uma geração sair com a
configuração antiga). `openai_compatible.rs` atende OpenAI, DeepSeek, LM Studio,
OpenRouter e qualquer endpoint compatível, com `CredentialMode`
(`None`/`ApiKey`/`BearerToken`) e cabeçalhos personalizados. `endpoint.rs` é o único lugar
onde uma string vira destino aceito: só `http`/`https`, credencial embutida na URL
rejeitada, seis cabeçalhos reservados não sobrescrevíveis, `REQUEST_TIMEOUT = 60s`,
`CONNECT_TIMEOUT = 10s`, `MAX_REDIRECTS = 2` (redirect ilimitado reenviaria
`Authorization` para o último host da cadeia), e `sanitized()` — esquema, host e porta —
como **única** forma de um endpoint aparecer em log ou erro. `EndpointClassification`
(`Loopback`/`PrivateNetwork`/`PublicInternet`) faz parte do retorno da validação porque o
usuário precisa saber quando a conversa deixa a máquina. Sobre SSRF: o app não recebe URL
de terceiros, recebe de quem está na frente dele — o objetivo não é impedir a escolha de
um destino, é impedir que um destino escolhido por engano passe despercebido. Ver
`docs/response-suggestion.md`, seção "Endpoints configuráveis".

O provedor por assinatura ChatGPT/Codex aparece no registry como **indisponível, com o
motivo real**: não há suporte oficial documentado para aplicações de terceiros. Ver
`docs/adr/chatgpt-codex-subscription-auth.md` — nenhuma autenticação foi implementada, e
"funciona em outro app" não é prova de suporte oficial.

O usuário escolhe o provedor de LLM: Ollama local (padrão) ou um provedor de nuvem
(OpenAI, DeepSeek, Anthropic). A mesma chamada que gera a resposta também decide se deve
responder, via um marcador `[SKIP]` no início do stream quando a fala não exige resposta
— sem uma segunda chamada de classificação e sem detector por regex. O prompt separa
fisicamente `CONTEXTO RECENTE:` / `FALA ATUAL DA OUTRA PESSOA:` / `INSTRUÇÃO:` para que a
decisão seja sobre a fala atual, não sobre o turno inteiro. O `SYSTEM_PROMPT` declara a
política: responder é o padrão, `[SKIP]` tem uma lista fechada de casos, a pontuação da
transcrição não conta na decisão (o transcritor quase nunca produz "?", então "Me conta um
caso real..." é um pedido sem interrogação), confirmação seguida de pedido se responde, e
em qualquer dúvida responde-se curto. Duas regras vieram de uma sessão ao vivo: o prompt
diz **de quem é a voz** (o texto vai ser lido em voz alta pelo próprio usuário, então nada
de oferta de serviço tipo "se quiser, posso te mostrar" nem pergunta de fechamento), e
acrescenta à lista fechada de `[SKIP]` o **enunciado que ainda não pede nada** — uma
pergunta falada costuma chegar partida em duas utterances, e responder só à premissa
produz resposta a meia pergunta; a premissa vira contexto da fala seguinte, onde o pedido
aparece. Contra alucinação, o mesmo prompt proíbe fabricar
específicos ausentes do contexto (nome, número, data, empresa, tecnologia) e pede resposta
de 2 a 4 frases — ver `docs/response-suggestion.md`, seção "Estrutura do prompt e política
de `[SKIP]`". Depois do `SkipDetector` e antes de qualquer `Delta`, o `EchoGuard`
(`echo_guard.rs`) descarta o eco da própria fala quando o modelo começa repetindo a
pergunta em vez de respondê-la — comparação com a fala conhecida que originou a geração,
não detecção de pergunta; a decisão de responder continua sendo do modelo, via `[SKIP]`.
API keys de provedores de nuvem ficam no keychain do SO via crate `keyring`,
nunca em texto puro no disco.
Eventos emitidos via `response://suggestion-event`: `started`, `delta`, `completed`,
`skipped`, `cancelled` e `error`, todos carregando `session_id`, `turn_id`,
`utterance_id` e `generation_id` — o `generation_id` para o frontend descartar eventos de
uma geração já superada, o `utterance_id` porque a sugestão pertence a uma **fala**, não
ao turno: um turno pode conter várias perguntas, e indexando por turno a resposta à
segunda sobrescrevia a resposta à primeira. A janela de sessão é um feed cronológico com
uma entrada por fala elegível (`features/session/SuggestionFeed.tsx`), crescendo para
baixo; nada já exibido é substituído no lugar. Há também `diagnostics` (ver abaixo). Ver
`docs/response-suggestion.md` para a arquitetura completa, módulo por módulo, e
`docs/session-experience.md` para o comportamento fim a fim durante uma sessão ao vivo.

**Latência.** Contexto deliberadamente pequeno (2 utterances remotas anteriores,
1 resposta do usuário, `maximum_context_characters` = 3000, `maximum_response_tokens` = 160,
`temperature` = 0.2, ver `context.rs`). O provider Ollama reutiliza uma única instância
de `reqwest::Client` (reconstruída só quando a configuração muda, nunca por chamada),
envia `keep_alive` configurável (`ResponseProviderConfig::ollama_keep_alive`, default
`10m`) para o Ollama não descarregar o modelo entre chamadas, e desliga o modo de
raciocínio estendido (`"think": false`) para modelos híbridos como o Qwen3, em vez de
depender de parsing de tags de pensamento. Cada geração emite um evento `diagnostics`
(`GenerationDiagnostics`) com o contexto do disparo (`finalization_reason`,
`gap_ms_used`, `silence_detected_ms`) e latências medidas com relógio monotônico
(`Instant`, não epoch): `utterance_finalized_to_request_started_ms` (meta de engenharia:
< 100 ms — mede diretamente o atraso de disparo),
`request_to_first_http_chunk_ms`/`request_to_first_visible_token_ms` (o segundo distingue
o primeiro chunk HTTP bruto do primeiro texto que o `SkipDetector` de fato libera) e
`end_of_speech_to_first_visible_token_ms` (métrica principal de UX: silêncio → resposta
visível).

## Telemetria de pipeline (`src-tauri/src/telemetry/`)

`GenerationDiagnostics` cobre um trecho; a fila de transcrição cobre outro; nenhuma das
duas responde "onde foram os 4 segundos". `PipelineTrace` cobre a fala inteira, com 12
marcos (`speech_started` … `generation_completed`) correlacionados pelos ids que já
existem (`SegmentId` → `UtteranceId` → `GenerationId`) e 5 latências derivadas — cada
trecho apontando para um culpado diferente (transcritor, gap configurado, disparo, LLM,
total sentido pelo usuário).

Duas decisões: **relógio monotônico** (`Instant`; epoch produziria durações negativas
durante uma reunião longa, que é quando a medição importa) e **conteúdo não é telemetria**
(`ContentPolicy::Redacted` é o padrão; `Developer` grava no máximo 160 caracteres
sanitizados). `Milestone` é um índice denso para que um trace seja
`[Option<Duration>; 12]`, sem alocação no caminho crítico. Latência ausente é `None`, nunca
`0` — uma fala pulada não tem `generation_started → first_visible_token`.

O recorder é singleton de processo por uma razão concreta: os marcos vêm de três
subsistemas construídos em pontos diferentes do `setup()` que não se conhecem, e injetar
o recorder nos três os acoplaria por algo que só existe para observabilidade. **Testes
nunca usam `recorder()`**; constroem `TelemetryRecorder::new()`. Limites:
`MAX_LIVE_TRACES = 64`, `MAX_COMPLETED_TRACES = 256`. Ver `docs/telemetry.md`.

## Benchmark de transcrição (`benchmarks/`, `src-tauri/src/bin/benchmark.rs`)

Roda o mesmo áudio contra transcritores diferentes e compara latência, WER, termos
técnicos perdidos, utterances resultantes e custo estimado — para que "vale a pena trocar
de backend?" tenha resposta medida em vez de impressão. `--model` é obrigatório: o provider
fake vive em `#[cfg(test)]` e devolve o texto que lhe mandaram, então rodar o harness
contra ele daria WER perfeito e não diria nada sobre transcritor nenhum.

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
