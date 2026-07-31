# Providers de transcrição

Como a camada de speech-to-text virou um ponto de extensão, e o que exatamente um backend
novo precisa implementar.

Código: `src-tauri/src/transcription/`.

## Por que existem dois contratos

O contrato original era `transcribe(segment) -> Transcript`. Ele descreve bem um engine
batch local e descreve mal qualquer outra coisa. Um backend de streaming (OpenAI Realtime,
Gemini Live) não recebe segmentos prontos: recebe áudio contínuo, emite parciais antes do
final e tem ciclo de vida próprio (abrir, alimentar, encerrar) que precisa acompanhar a
fronteira de sessão de conversa. Encaixá-lo no molde batch significaria acumular áudio para
fingir segmentos e jogar os parciais fora — perdendo justamente a latência que motiva usar
esse tipo de backend.

Então os dois contratos coexistem, com papéis diferentes:

| Contrato | Arquivo | Papel |
| --- | --- | --- |
| `SegmentTranscriber` | `segment_transcriber.rs` | inferência batch de um `AudioSegment`. É o que o `whisper-rs` faz de fato, e o que o `model_manager` carrega/descarrega. |
| `TranscriptionProvider` | `provider.rs` | ponto de extensão da aplicação: abre sessões, tem capacidades declaradas, entra no registry. |

`WhisperLocalTranscriptionProvider` (`whisper_local.rs`) é o adaptador entre os dois: ele
implementa `TranscriptionProvider` por cima de um `Arc<dyn SegmentTranscriber>`. É por isso
que o Whisper local ganhou a interface nova sem que uma linha de `whisper_provider.rs`
mudasse de comportamento.

## Contrato

```rust
#[async_trait]
pub trait TranscriptionProvider: Send + Sync {
    fn id(&self) -> TranscriptionProviderId;
    fn capabilities(&self) -> TranscriptionCapabilities;
    async fn start_session(
        &self,
        context: TranscriptionSessionContext,
    ) -> Result<Box<dyn TranscriptionSession>, TranscriptionError>;
}

#[async_trait]
pub trait TranscriptionSession: Send {
    async fn push_audio(&mut self, chunk: AudioChunk) -> Result<(), TranscriptionError>;
    async fn finish(&mut self) -> Result<(), TranscriptionError>;
    async fn cancel(&mut self) -> Result<(), TranscriptionError>;
}
```

A sessão **não devolve** o resultado por retorno de função. Ela emite eventos pelo
`TranscriptionSessionContext`, que carrega a identidade (`session_id`,
`transcription_session_id`, `source`) e o canal de saída. É o que permite a um backend
batch emitir um `Final` por chunk e a um backend de streaming emitir dez `Partial` e um
`Final` sem que o contrato mude.

### Capacidades declaradas, não descobertas

```rust
pub struct TranscriptionCapabilities {
    pub local: bool,
    pub streaming: bool,
    pub partial_results: bool,
    pub speaker_source_preserved: bool,
    pub language_selection: bool,
    pub automatic_language_detection: bool,
    pub requires_credentials: bool,
}
```

`TranscriptionCapabilities::none()` é a base conservadora: tudo `false`, exceto
`speaker_source_preserved`, que é requisito de arquitetura e não capacidade opcional —
um backend que misture microfone e saída do sistema não é aceitável neste produto,
independentemente do que ele ofereça em troca.

A UI e o runtime **consultam** as capacidades. Ninguém assume que todo provider tem
parciais, nem que todo provider funciona offline.

## Eventos normalizados

Todo provider reporta na mesma forma (`events.rs`):

```rust
pub enum TranscriptionEvent {
    Partial(PartialTranscript),
    Final(FinalTranscript),
    SpeechStarted(SpeechBoundary),
    SpeechEnded(SpeechBoundary),
    Error(TranscriptionErrorEvent),
}
```

E todo payload carrega a identidade completa:

```
session_id                 sessão de conversa
transcription_session_id   sessão de transcrição daquela fonte
source                     Microphone | SystemOutput
provider                   quem produziu
language                   idioma reportado, quando houver
text
started_at / ended_at      relógio monotônico do processo
confidence                 quando o backend expõe
is_final
provider_event_id          identidade do evento no backend de origem
```

Sem essa identidade, um resultado atrasado é indistinguível de um resultado atual e só
poderia ser descartado no frontend — tarde demais, porque a timeline já teria virado
utterance e a geração de resposta já teria disparado.

## Registry

`registry.rs`. Guarda **instâncias**, não descritores (ao contrário do registry de resposta,
que reconstrói o provider a cada troca de configuração). Duas propriedades:

1. **Um provider previsto mas não implementado não é registrado.** Selecionar
   `OpenAiRealtime` hoje devolve `TranscriptionError::ProviderUnavailable` com o motivo, e
   não um provider que aceita áudio e nunca transcreve. Um backend que finge funcionar é
   pior que um erro: a sessão inteira parece correta e não produz uma sugestão sequer.
2. **`descriptors()` lista registrados e previstos**, com `available` e
   `unavailable_reason` reais. `Fake` nunca aparece: é infraestrutura de teste.

Estado atual:

| Provider | `id` | Situação |
| --- | --- | --- |
| Whisper local (whisper.cpp) | `whisper_local` | implementado, padrão |
| OpenAI Realtime Transcription | `openai_realtime` | contrato preparado, **não implementado** |
| Google Gemini | `google_gemini` | contrato preparado, **não implementado** |
| Compatível com a API da OpenAI | `openai_compatible` | contrato preparado, **não implementado** |
| Fake | `fake` | só em `#[cfg(test)]` |

Os três não implementados continuam assim deliberadamente: implementá-los exigiria fixar
protocolo de streaming, formato de áudio aceito e forma de autenticação a partir de
documentação oficial. Enquanto isso não for feito com a documentação na mão, o contrato
fica preparado e o registry diz a verdade — nenhum endpoint é inventado.

## Ciclo de vida e isolamento (`runtime.rs`)

Uma sessão de conversa possui **suas próprias** sessões de transcrição, uma por fonte:

```
Conversation Session A
├── microphone transcription session A
├── system-output transcription session A
└── response session A
```

O `TranscriptionRuntime` abre, roteia, encerra e — o ponto central — **descarta no
backend** todo evento que não pertence ao estado atual. Três chaves, cada uma cobrindo um
caso que as outras não cobrem:

- `session_id` — resultado de uma sessão de conversa anterior;
- `transcription_session_id` — mesma sessão de conversa, mas de uma sessão de transcrição
  já substituída (troca de provider ou de dispositivo no meio da reunião);
- `provider_event_id` — o mesmo resultado entregue duas vezes (retry de rede, reentrega de
  stream). Sem isso a fala apareceria duplicada na timeline.

A janela de dedupe é limitada (`DEDUPE_WINDOW = 512` por sessão de transcrição): uma reunião
de horas não pode acumular um conjunto sem fim, e qualquer reentrega plausível acontece em
segundos.

### Ordem do encerramento

```
bloquear chunks novos
→ invalidar a identidade de sessão
→ cancelar os providers
→ limpar buffers
```

A ordem é o que torna o isolamento real. Cancelar antes de bloquear deixaria uma janela em
que um chunk novo reabriria uma sessão recém-cancelada.

### Falha de uma fonte não derruba a outra

As duas fontes compartilham provider, runtime e fila. O microfone é o caminho que mais falha
na prática (dispositivo trocado, permissão revogada) e é o menos importante dos dois: quem
faz a pergunta é a outra pessoa, pela saída do sistema. Uma falha de microfone que levasse a
saída do sistema junto produziria uma reunião inteira sem sugestão nenhuma e sem nada na
tela explicando o porquê. Coberto por
`a_microphone_failure_never_silently_takes_system_output_down_with_it`.

## Configuração

`settings.rs` — `TranscriptionSettings` é **separado** de
`response_provider::settings::ResponseSettings`:

```rust
pub struct TranscriptionSettings {
    pub provider: TranscriptionProviderId,   // default: WhisperLocal
    pub language: LanguageCode,              // default: Fixed("pt")
    pub model: Option<String>,               // None = o provider decide
}
```

São dois eixos independentes de propósito. Transcrever localmente e gerar na nuvem é uma
combinação legítima e é o default do produto; um único campo "provedor de IA" ligaria as
duas escolhas e tornaria essa combinação inexprimível. Coberto por
`transcription_and_response_providers_are_chosen_independently`.

Comandos Tauri: `transcription_providers_command`, `transcription_settings_command`,
`transcription_set_settings_command`, `transcription_diagnostics_command`.

## Como adicionar um backend

1. Implemente `TranscriptionProvider` + `TranscriptionSession` no seu módulo.
2. Declare `capabilities()` honestamente. Se o backend não tem parciais, `partial_results:
   false` — o runtime se comporta diferente e a UI não promete o que não existe.
3. Emita eventos com a identidade completa que veio no `TranscriptionSessionContext`. Não
   invente `session_id` nem `provider_event_id`: o primeiro é a chave de descarte, o segundo
   é a chave de dedupe.
4. Recuse áudio da outra fonte (`TranscriptionError::SourceMismatch`). Uma sessão pertence a
   uma fonte.
5. Registre em `TranscriptionProviderRegistry` no boot (`lib.rs`) e remova o id de
   `PLANNED_PROVIDERS`.
6. Escreva os testes contra o contrato, não contra a implementação — os de
   `whisper_local.rs` servem de modelo (capacidades, identidade no payload, recusa de fonte
   errada, sessão fechada, falha de inferência).

## Medição

O harness de benchmark (`benchmarks/README.md`) roda o mesmo áudio contra providers
diferentes e compara latência, WER, termos técnicos perdidos, utterances resultantes e
custo estimado. É o que torna "vale a pena trocar de backend?" uma pergunta com resposta
medida em vez de impressão.
