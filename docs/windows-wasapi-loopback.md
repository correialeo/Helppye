# Captura de áudio de sistema no Windows via WASAPI loopback

**Escopo:** `src-tauri/src/audio/platform/windows/` — implementação de
`AudioCaptureProvider` para `AudioSource::SystemOutput` no Windows, usando captura
loopback em modo compartilhado da WASAPI (`windows` crate 0.62.2).

**Ambiente de desenvolvimento/validação:** WSL2/Linux, sem hardware de áudio Windows
real disponível. Esta seção declara explicitamente o que foi verificado neste sandbox
e o que ainda depende de confirmação manual em Windows real — ver seção 7.

---

## 1. Visão geral

WASAPI não tem uma API de "captura de saída" dedicada: em vez disso, qualquer
dispositivo de **render** (saída) pode ser aberto em modo de captura com a flag
`AUDCLNT_STREAMFLAGS_LOOPBACK`, que faz o `IAudioClient` entregar, via
`IAudioCaptureClient`, o mesmo áudio que está sendo tocado nesse dispositivo. É esse
mecanismo — não uma extensão especial de driver — que este módulo usa.

Fluxo por captura, do lado Windows:

1. `IMMDeviceEnumerator` resolve o dispositivo de render alvo (por ID estável ou o
   padrão do sistema) — `devices.rs`.
2. `IMMDevice::Activate` cria um `IAudioClient` para esse dispositivo.
3. `IAudioClient::GetMixFormat` retorna o formato interno do mecanismo de áudio
   (quase sempre `WAVEFORMATEXTENSIBLE` float 32-bit) — parseado em `format.rs`.
4. `IAudioClient::Initialize` com `AUDCLNT_SHAREMODE_SHARED` e
   `AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK`.
5. Um evento Win32 (`CreateEventW`) é registrado via `SetEventHandle`; a thread de
   captura dorme em `WaitForSingleObject` em vez de fazer polling.
6. `IAudioCaptureClient::GetBuffer`/`ReleaseBuffer` entregam pacotes PCM intercalados,
   convertidos para `f32`, downmixados para mono e reamostrados para a taxa alvo
   (16 kHz) antes de virar `AudioFrame`s enviados pelo canal `mpsc` limitado.

Todo esse ciclo roda em uma única thread OS dedicada
(`std::thread::Builder::spawn`, nome `helppye-loopback-capture`), não na runtime
async do Tokio — chamadas COM/WASAPI são bloqueantes e a apartment COM é afim à
thread que a inicializou (seção 2).

## 2. Modelo de apartment COM: MTA, não STA

`com::ComGuard` chama `CoInitializeEx(None, COINIT_MULTITHREADED)` em vez de
`COINIT_APARTMENTTHREADED`. STA exige uma bomba de mensagens Win32 (`GetMessage`/
`DispatchMessage`) na thread para despachar chamadas COM originadas de outras
apartments; a thread de captura aqui não tem — nem precisa de — um message pump,
então MTA é o modelo correto e mais simples. `ComGuard` deliberadamente não é
`Send`/`Sync` (`PhantomData<*mut ()>`), refletindo que uma apartment COM é afim à
thread que a inicializou; é criado e destruído (via `Drop`) inteiramente dentro da
mesma função de setup, na mesma thread que depois roda o loop de captura.

A enumeração de dispositivos (`devices::list_output_devices`, usada pelo comando
Tauri de listagem) roda em `tokio::task::spawn_blocking` com seu próprio
`ComGuard` — instância separada, mesma thread, sem cruzar apartments.

## 3. Detecção de desconexão de dispositivo

Em vez de implementar um callback COM completo (`IMMNotificationClient`) para
observar eventos de dispositivo, este MVP detecta desconexão/invalidação
observando o HRESULT `AUDCLNT_E_DEVICE_INVALIDATED` retornado por qualquer chamada
WASAPI dentro do loop de captura (`GetNextPacketSize`, `GetBuffer`, `ReleaseBuffer`)
— ver `capture::handle_stream_error`. Esse código é o retorno documentado da WASAPI
quando o endpoint é desconectado, desabilitado, ou tem seu formato alterado
enquanto um cliente o mantém aberto. Ao detectar isso, o loop emite
`AudioCaptureEvent::DeviceDisconnected` e cancela a captura; qualquer outro erro
WASAPI vira um `AudioCaptureEvent::Error` genérico, também cancelando.

**Trade-off assumido:** essa abordagem não notifica sobre uma *troca* de
dispositivo padrão enquanto a captura roda em um device específico (não-padrão) que
continua válido, nem sobre um novo dispositivo padrão aparecer — apenas sobre o
dispositivo atualmente aberto parar de responder. Implementar
`IMMNotificationClient` para cobrir esses casos foi considerado fora do escopo
desta primeira versão.

## 4. Conversão de formato e resample

`sample_convert.rs` é código Rust puro, sem dependência da crate `windows` —
testável em qualquer plataforma (ver seção 7) — que converte um buffer PCM
intercalado bruto para `f32` em `[-1.0, 1.0]`, suportando:

- `F32` (contêiner IEEE float 32-bit) — cópia direta via `f32::from_le_bytes`.
- `IntPcm { container_bytes }` — inteiro assinado little-endian de 2, 3 ou 4 bytes,
  com sign-extension manual para o caso de 24-bit empacotado em 3 bytes.

**Suposição documentada e não verificável neste sandbox:** para contêineres mais
largos que a profundidade de bits real (ex.: 24-bit dentro de um contêiner de 32
bits), assume-se justificação à esquerda (*left-justified*) — a convenção mais
comum em drivers WASAPI. Um driver incomum que faça justificação à direita sem
reescalar produziria áudio mais baixo do que o esperado, e não há como detectar
isso apenas a partir do formato relatado por `GetMixFormat`. Na prática, o formato
de mixagem em modo compartilhado é quase sempre float 32-bit desde o Windows Vista
— este caminho inteiro de PCM inteiro é um fallback defensivo, não o caso comum.

`format.rs` faz o parsing de `WAVEFORMATEX`/`WAVEFORMATEXTENSIBLE` (struct
`#[repr(packed)]`) para determinar canais, taxa de amostragem nativa e o
`SampleContainer` a usar; todo acesso a campo de struct empacotada é copiado para
uma variável local antes de comparar, para evitar referência desalinhada (erro
`E0793`).

Após a conversão para `f32`, `resampler::downmix_to_mono` reduz para 1 canal e
`resampler::resample_linear` reamostra da taxa nativa do dispositivo para a taxa
alvo configurada (16 kHz) — ambas as funções já existiam e são compartilhadas com o
caminho de captura de microfone.

## 5. Backpressure

Igual ao caminho de microfone: o loop de captura acumula amostras reamostradas até
completar um `AudioFrame` (tamanho definido por `CaptureConfig::frame_len_samples`)
e então faz `sender.try_send(...)` em um `mpsc::Sender` limitado. Se o consumidor
estiver atrasado e o canal estiver cheio, o frame mais recente é descartado
(*drop-oldest* por não bloquear o produtor) e um contador `dropped_frames` é
incrementado, logando um aviso a cada 50 descartes em vez de a cada um.

## 6. Módulos

| Arquivo | Responsabilidade |
|---|---|
| `mod.rs` | `SystemAudioProvider`, implementação de `AudioCaptureProvider`: `list_devices` via `spawn_blocking`, `start` via thread dedicada + handshake `oneshot` de prontidão |
| `com.rs` | `ComGuard` — RAII para `CoInitializeEx`/`CoUninitialize` em MTA |
| `devices.rs` | Enumeração de dispositivos de render ativos e resolução por ID estável (`IMMDevice::GetId`), não por substring de nome |
| `format.rs` | Parsing de `WAVEFORMATEX(TENSIBLE)` para `WaveFormat` (canais, taxa, `SampleContainer`) |
| `capture.rs` | Thread de captura: setup do `IAudioClient`/`IAudioCaptureClient`, loop orientado a evento, conversão/downmix/resample, emissão de eventos |

Identificação de dispositivo por ID estável (`IMMDevice::GetId`) foi uma escolha
deliberada em vez de correspondência por substring de nome — a auditoria do Meetily
(`docs/meetily-audio-audit.md`, seção 6, achado 7) sinalizou esse padrão como frágil
no código de referência daquele projeto.

## 7. O que foi verificado neste ambiente vs. o que ainda precisa de Windows real

Este sandbox é Linux/WSL2, sem WASAPI, sem dispositivo de áudio Windows e sem
toolchain de link para `x86_64-pc-windows-gnu` (apenas front-end de compilação).
Nenhuma captura de áudio real foi executada. O que foi efetivamente verificado:

**Verificado neste sandbox:**
- `cargo fmt --check`, `cargo check --target x86_64-unknown-linux-gnu`,
  `cargo clippy --target x86_64-unknown-linux-gnu -- -D warnings` (mod. dead-code
  esperado, ver abaixo) e `cargo test --target x86_64-unknown-linux-gnu` (20 testes,
  todos passando) na crate real — cobre `sample_convert.rs` e todo o código
  independente de plataforma.
- `cargo check --target x86_64-pc-windows-gnu` e
  `cargo clippy --target x86_64-pc-windows-gnu -- -D warnings` **limpos** (sem
  avisos além de um import não usado específico da crate de validação isolada,
  não presente na crate real) contra a API real da crate `windows` 0.62.2, rodados
  em uma crate isolada que espelha `src/audio/platform/windows/*` e os módulos
  compartilhados de áudio, para contornar uma falha pré-existente e não relacionada
  do `tauri-build` ao fazer cross-compile para Windows. Isso valida tipos,
  assinaturas, feature flags do crate `windows` e borrow-checking do código
  Windows-específico, mas **não** substitui rodar/linkar o binário real em Windows.
- `npm run typecheck`, `npm run lint`, `npm run build` — limpos.
- Testes unitários de `sample_convert.rs` (6 casos: passagem direta de f32,
  min/max de i16, i24 empacotado positivo/negativo com sign-extension, min/max de
  i32, split de buffer por stride) — hardware-independentes, rodam em qualquer
  plataforma.

**Nota sobre `clippy -D warnings` no target Linux:** falha com 11 avisos
`dead_code` — funções/variantes usadas apenas por código Windows-`cfg`-gated (ex.:
`SampleContainer`, `AudioSource::SystemOutput`, `AudioCaptureEvent::DeviceDisconnected`)
parecem não utilizadas quando compiladas apenas para Linux. Isso é uma
característica pré-existente e inerente de desenvolver esta base de código
multiplataforma a partir de um sandbox Linux (o mesmo já ocorria antes desta
implementação, com `target_channels`, `AudioCaptureError::Cancelled`, `rms_dbfs` e
o método `stop` do trait) — não foi suprimido com `#[allow(dead_code)]` para não
mascarar dead-code genuíno futuro; será resolvido naturalmente quando a
implementação macOS/Linux também existir e consumir esses itens.

**Ainda precisa de confirmação manual em Windows real (não fabricado aqui):**
- Captura de loopback funcionando de fato contra um dispositivo de saída real
  (áudio audível chegando como `AudioFrame`s corretos).
- Teste real com Discord, Google Meet ou qualquer app tocando áudio.
- Estabilidade em sessão longa (ex. 20 minutos) — throughput, memory/handle leaks,
  comportamento do event handle sob carga.
- Comportamento da UI (medidor de nível, throttle de atualização) com dados reais
  do painel "System output".
- Ciclos de start/stop repetidos (reentrância do `AudioState`, liberação correta
  de `IAudioClient`/`IAudioCaptureClient`/`ComGuard`/`EventHandle`).
- Desconexão/reconexão real de dispositivo (ex. desplugar um monitor com áudio
  HDMI, desabilitar um dispositivo pelo painel de som) para confirmar que
  `AUDCLNT_E_DEVICE_INVALIDATED` de fato ocorre e é tratado como esperado.
- A suposição de justificação à esquerda para PCM inteiro em contêiner mais largo
  (seção 4), já que não há hardware/driver disponível aqui que produza esse
  formato para testar.

Nenhum desses itens foi marcado como testado ou executado neste trabalho — todos
requerem confirmação manual em uma máquina Windows real.
