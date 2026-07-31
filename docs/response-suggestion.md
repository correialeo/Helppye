# Sugestão de resposta em streaming (`src-tauri/src/response_provider/`)

Substitui a antiga detecção local de perguntas por regras (`question_detection.rs`,
removida). Em vez de apenas sinalizar que um turno da outra pessoa parece uma pergunta,
o pipeline atual gera, via LLM e em streaming, uma sugestão real de resposta para o
usuário — mantendo a filosofia local-first como padrão, mas permitindo que o usuário
escolha explicitamente um provedor de nuvem quando quiser.

## Visão geral do fluxo

```
Conversation Timeline (turns/utterances) — dona do session_id
        │  silêncio ≥ same_speaker_utterance_gap_ms → timer dedicado finaliza a
        │  utterance sozinho (sem esperar novo segmento/flush/stop/turno fechar)
        │  UtteranceFinalized (turno elegível, com session_id)
        ▼
process_conversation_events → GenerationTrigger (session_id, utterance_id,
        │  utterance_revision, finalization_reason, gap_ms_used, silence_detected_ms,
        │  utterance_finalized_at)
        ▼
ResponseEngine::trigger_generation
        │  rejeita gatilho de sessão não-ativa/encerrando (generation_rejected_wrong_session)
        │  cancela geração anterior do mesmo turno, se houver (token filho do token raiz)
        ▼
context::build_request (histórico **da sessão ativa** + fala atual isolada)
        │  history_snapshot(session_id) → None se a sessão já mudou ⇒ aborta
        ▼
ResponseProvider ativo (Ollama | OpenAI | DeepSeek | Anthropic)
        │  stream de ResponseChunk::Delta/Done
        ▼
SkipDetector (decide [SKIP] vs. conteúdo real, sem segunda chamada ao LLM)
        │
        ▼
publish_stream_event / publish_terminal_event
        │  revalida sessão + geração corrente antes de **cada** emissão
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
`silence_detected_ms` (quando mensurável) e `session_id`. O motivo esperado no dia a dia
é `inactivity_timeout` — silêncio após a fala, detectado pelo timer dedicado da utterance.
`speaker_changed`/`source_changed` também finalizam a utterance, mas deliberadamente
**não** disparam geração (ver "Elegibilidade" abaixo).

`same_speaker_utterance_gap_ms` (default 1800ms) é configurável em runtime, sem rebuild,
via `conversation_get_utterance_gap_ms_command`/`conversation_set_utterance_gap_ms_command`
— exposto no frontend só em modo dev (`UtteranceGapDevControl` em `App.tsx`, com atalhos
para 1200/1500/1800/2200ms) para calibrar o trade-off entre latência e responder cedo
demais (no meio de uma pergunta ainda incompleta).

## Elegibilidade

Igual à extinta detecção de perguntas: só turnos com `speaker = OtherPerson` e
`source = SystemOutput` disparam geração (`engine::is_eligible_turn`). O usuário nunca
recebe uma "sugestão de resposta" para a própria fala.

Além do turno ser elegível, o **motivo de finalização** da utterance precisa representar
fim de fala, não desmontagem de estado (`engine::triggers_generation`):

| `UtteranceFinalizationReason`                              | Dispara geração? |
| ---------------------------------------------------------- | ---------------- |
| `inactivity_timeout`, `manual_flush`, `maximum_duration`   | sim              |
| `speaker_changed`, `source_changed`                        | **não**          |
| `capture_stopped`, `session_ended`                         | **não**          |

**`speaker_changed`/`source_changed` — o usuário tomou a palavra.** Numa utterance da
outra pessoa, esses dois motivos só podem significar que o microfone começou a produzir
fala. O usuário já está respondendo; uma sugestão nesse instante chega tarde por
definição. Pior, o efeito era ativamente destrutivo: enquanto ele lia a sugestão em voz
alta, a própria leitura entrava pelo microfone, finalizava a utterance da outra pessoa por
troca de speaker e disparava uma geração nova — que ia substituindo, token a token,
exatamente a resposta que estava sendo lida. E como a fala dele acabara de entrar no
contexto como `Você: ...`, o modelo com frequência devolvia a fala dele de volta. O
disparo legítimo é o silêncio, que o timer dedicado da utterance já cobre.

Parar a captura e encerrar a sessão finalizam a utterance aberta — é correto que
finalizem, mas essas finalizações são consequência do teardown, não de alguém ter
terminado de falar. Antes desse gate, encerrar a sessão A disparava uma geração *pela
própria finalização de encerramento*, cujo resultado chegava depois — parte visível do
sintoma "a resposta da sessão anterior reaparece".

## Isolamento por sessão

A unidade de isolamento é a **sessão**, e ela é identificada por um `SessionId`
monotônico (`conversation::SessionId`, `AtomicU64`). A `ConversationTimeline` é a dona
desse ID; o `ResponseEngine` espelha o valor e o usa como chave de validade de tudo que
faz. Nenhum estado conversacional atravessa a fronteira.

### O que cada camada zera

- **`ConversationTimeline` / `ConversationAssembler`** — `reset_for_new_session` aloca um
  `SessionId` novo e limpa *todas* as coleções: segmentos brutos, utterances, turnos,
  utterance/turno abertos por source e o estado de timer. `start_session` e `end_session`
  devolvem um `SessionTransition { previous_session_id, session_id, events }`, e os
  eventos de fronteira (`session_ended`, `session_started`) vão para o frontend na mesma
  ordem em que ocorreram.
- **`ResponseEngine`** — todo o estado mutável de sessão vive num único
  `Mutex<SessionState>`: `session_id`, flag `ending`, `CancellationToken` raiz, `history`
  (deque de turnos finalizados) e `generations` (mapa `TurnId → GenerationHandle`).
  `begin_session` substitui o `SessionState` inteiro por um novo — token raiz novo,
  histórico vazio, mapa de gerações vazio.

**Provider e `reqwest::Client` são deliberadamente preservados** entre sessões:
reaproveitar o pool de conexões HTTP é correto e é o que mantém a latência da primeira
geração baixa. O que nunca é reaproveitado é conteúdo conversacional.

### Identidade de uma geração

Toda geração carrega um `GenerationContext`:

```rust
pub struct GenerationContext {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub utterance_id: UtteranceId,
    pub utterance_revision: u64,
    pub generation_id: GenerationId,
}
```

Esses campos aparecem nos logs estruturados e — exceto `utterance_revision` — em todos os
eventos públicos de `response://suggestion-event`, para que o frontend consiga descartar
qualquer coisa que ainda escape.

### Os quatro pontos de validação

O `session_id` é comparado com a sessão ativa em quatro lugares distintos, e falhar em
qualquer um deles é sempre um descarte silencioso com log `debug` — nunca um erro exibido
ao usuário:

1. **No gatilho** (`trigger_generation`): sessão diferente ou `ending = true` ⇒
   `generation_rejected_wrong_session`, nada é iniciado.
2. **No histórico** (`push_history` / `history_snapshot`): um turno finalizado que chegue
   atrasado, de uma sessão encerrada, não entra no histórico; e uma geração cuja sessão
   mudou entre o gatilho e a montagem do prompt recebe `None` de `history_snapshot` e
   aborta antes de chamar o provedor. **É este o ponto que corrige a contaminação de
   prompt na origem** — não existe snapshot "global" de histórico em lugar nenhum.
3. **Antes de cada emissão** (`is_publishable` → `publish_stream_event` /
   `publish_terminal_event`): `started`, cada `delta` e o estado terminal só são emitidos
   se a sessão ainda for a ativa, não estiver encerrando, o token raiz não estiver
   cancelado e o `generation_id` ainda for o corrente daquele turno. Um `delta` que chegue
   depois do encerramento é descartado no **backend**, com `event_emitted =
   "discarded_stale"` no diagnóstico.
4. **No timer da utterance** (`ConversationTimeline::fire_utterance_timeout`): o timer
   compara `session_id` *e* `ConversationUtterance::revision` antes de finalizar. Um timer
   agendado na sessão A que expire depois de a sessão B começar encontra um estado que não
   bate mais e vira no-op.

### Cancelamento hierárquico

Um `CancellationToken` raiz por sessão; cada geração roda sob um `child_token()` dele.
Cancelar o raiz cancela toda geração em voo de uma vez, sem precisar percorrer o mapa
para cancelar uma a uma (o mapa ainda é percorrido, mas para *marcar* estado terminal, não
para propagar o cancelamento). Um token cancelado **nunca é reutilizado**: `begin_session`
cria um `SessionState` novo com um token novo, e não há caminho que "descancele" um token
existente.

### Encerramento atômico e ordenado

`ResponseEngine::end_session` roda inteiro sob o mesmo lock, nesta ordem:

1. valida que `session_id` é a sessão ativa (senão, no-op logado);
2. valida que ela ainda não está encerrando (senão, no-op logado — **idempotência**);
3. `ending = true` — a partir daqui nenhum gatilho novo passa;
4. cancela o token raiz;
5. drena `generations`, fazendo `terminal_emitted.swap(true)` em cada handle antes de
   cancelar seu token filho — assim, quando a task acordar do `select!`, ela verá que o
   estado terminal já foi "consumido" e não publicará `cancelled`/`completed`/`error` de
   uma sessão que não existe mais;
6. limpa `history` e loga `session_state_cleared` com quantos turnos foram apagados.

Como os passos 3–6 acontecem sob o mesmo `Mutex`, não existe janela em que uma geração
consiga ler um estado meio-encerrado.

`conversation_end_session_command` **não** realimenta suas próprias finalizações em
`process_conversation_events`: ele emite os eventos de timeline para o frontend e chama
`ResponseEngine::end_session`. Antes, o comando passava suas finalizações pelo mesmo
caminho de um `UtteranceFinalized` normal, o que fazia o encerramento gerar trabalho novo
em vez de encerrar o existente.

### Estado terminal único

`GenerationHandle` carrega um `Arc<AtomicBool>` (`terminal_emitted`). Todo caminho de
saída — `Completed`, `Skipped`, `Cancelled`, `Error`, `Superseded`, `SessionEnded` —
passa por `publish_terminal_event`, que faz `swap(true, SeqCst)` e só emite se o valor
anterior era `false`. Isso é proteção explícita contra dupla finalização: antes, uma
geração superada por uma nova utterance podia emitir `cancelled` duas vezes (uma pelo
`trigger_generation` que a substituiu, outra pela própria task ao acordar cancelada).

`finish_generation` continua rodando em **todo** caminho de saída, emite `Diagnostics`
apenas se a sessão ainda estiver ativa, e sempre libera o slot daquele turno via
`clear_if_current` — que só remove a entrada se o `generation_id` guardado ainda for o
desta geração, para uma geração encerrada tarde não apagar o slot de sua substituta.

## Estrutura do prompt e política de `[SKIP]`

A decisão de responder é sobre a **fala atual**, não sobre o turno inteiro nem sobre o
contexto. O prompt reflete isso fisicamente (`context.rs`):

```
CONTEXTO RECENTE:
Outra pessoa: ...
Você: ...

FALA ATUAL DA OUTRA PESSOA:
Me dá um exemplo disso.

INSTRUÇÃO: Escreva agora, em primeira pessoa, a resposta do usuário à fala atual, em 2 a
4 frases. Você é o próprio usuário falando na reunião, não um assistente atendendo
alguém: nunca se ofereça para mostrar, explicar, buscar ou fazer algo, e nunca termine
perguntando se a pessoa quer mais detalhes. Vá direto ao conteúdo: nada de repetir ou
reformular a pergunta, nada de comentar se ela exige resposta, nada de preâmbulo. Use o
contexto apenas para resolver referências, e não invente nome, número, data, empresa ou
tecnologia que não esteja nele. A pontuação da transcrição não é confiável: um pedido ou
pergunta sem "?" continua sendo um pedido. Escreva apenas [SKIP] se a fala atual for
somente saudação, somente uma confirmação isolada, um fragmento sem sentido, ou apenas um
enunciado que monta contexto sem pedir nada ainda.
```

A instrução é a **última coisa que o modelo lê antes de gerar**, e é a que mais pesa na
saída. A versão anterior pedia só uma decisão ("Decida exclusivamente se a fala atual
exige resposta"), o que contradizia o `SYSTEM_PROMPT` — que já mandava responder — e
fazia modelos locais menores devolverem a análise da fala, ou a própria pergunta
reformulada, em vez de uma resposta. A tarefa pedida é escrever a resposta; `[SKIP]` é a
exceção, não o objetivo.

Sem contexto (primeira fala da sessão), o bloco recebe
`(nenhum — esta é a primeira fala da sessão)` em vez de sumir — a estrutura do prompt é
sempre a mesma, o que evita que a ausência de contexto mude o formato que o modelo vê.

O texto que o mesmo interlocutor já havia dito **neste turno**, antes da utterance atual,
vai para o contexto (`preceding_text_in_turn`), nunca duplicado dentro da fala atual. Se
a utterance atual não for encontrada como sufixo do texto do turno (caso patológico de
normalização), o prefixo vira vazio em vez de reenviar o turno inteiro — duplicar a fala
atual dentro do contexto embaralharia justamente a decisão de `[SKIP]`.

O `SYSTEM_PROMPT` declara a política em vez de deixá-la implícita:

- **Responder é o padrão.** `[SKIP]` tem uma **lista fechada** de casos — saudação
  isolada; confirmação/reação isolada sem nada depois; fragmento truncado, ruído ou fala
  sem sentido; enunciado que só monta contexto e ainda não pede nada; fala que claramente
  não é dirigida ao usuário — e nenhum outro.
- **De quem é a voz.** O texto gerado vai ser lido em voz alta *pelo usuário*, como fala
  dele. O prompt diz isso explicitamente e proíbe oferta de serviço ("se quiser, posso te
  mostrar", "quer que eu explique") e pergunta de fechamento — voz de assistente é
  inutilizável numa reunião, porque o usuário não tem o que fazer com ela.
- **A pontuação da transcrição não conta.** O transcritor quase nunca produz "?", então
  decidir por pontuação é decidir por um sinal que não existe: "me conta como foi",
  "explica melhor" e "e como você resolveu isso" são pedidos escritos sem interrogação.
- Fala que começa com confirmação/saudação mas contém pergunta ou pedido ⇒ responder ao
  pedido (**o caso citado explicitamente**, com exemplo, porque é o que falhava na
  prática: "Perfeito. Me conta um caso real..." voltava como `[SKIP]`).
- **Em qualquer dúvida, responder curto em vez de `[SKIP]`** — com uma exceção nomeada,
  abaixo.
- **Enunciado que ainda não pede nada ⇒ esperar.** Uma pergunta falada costuma chegar
  partida em duas utterances, porque a pessoa respira no meio dela e o silêncio finaliza
  a primeira: "Eu tenho uma query que até ontem respondia em menos de um segundo." …
  "Só que hoje ela demora cinco segundos. O que pode ter acontecido?". A primeira metade é
  só a premissa — não há pergunta, pedido, imperativo nem convite a falar. Responder a ela
  produz uma resposta a meia pergunta (e o modelo, forçado a responder, continua a frase
  da outra pessoa em primeira pessoa, como se fosse o usuário narrando o problema dela).
  Esse é o **único** caso em que a dúvida se resolve pulando, e o prompt marca a exceção
  explicitamente para ela não ser anulada pela regra anterior. A premissa não se perde:
  ela vira `CONTEXTO RECENTE:` da fala seguinte (`preceding_text_in_turn`), que é onde o
  pedido aparece — então a resposta cobre a pergunta inteira. O raio de ação real do
  problema também é ajustável em runtime: `same_speaker_utterance_gap_ms` (Configurações →
  Modo de desenvolvedor) controla quanto silêncio parte uma fala em duas.
- Exemplos curtos de calibração no fim do prompt de sistema — dois que devem responder
  (um imperativo depois de confirmação, uma pergunta sem "?") e dois que devem pular.
  Modelos locais pequenos seguem exemplo melhor do que seguem política declarada.

Contra alucinação, o mesmo prompt separa **responder** de **inventar**: a resposta
continua obrigatória, mas não pode fabricar o específico que o modelo não tem como saber
— nomes de empresa, clientes, produtos, datas, números, métricas ou tecnologias que não
apareceram no contexto. Quando o pedido exige um detalhe pessoal ausente ("me conta um
caso real em que você..."), a política é responder pelo raciocínio e pela estrutura da
experiência, deixando o específico em aberto para o usuário completar em voz alta, em vez
de fabricar um caso — e, sem base nenhuma, dar a resposta mais curta e honesta possível.
O teto de saída (`MAX_OUTPUT_TOKENS` = 160) e a instrução de 2 a 4 frases também são
anti-alucinação, não só latência: resposta longa é onde o detalhe inventado aparece.

Nada disso é verificável por teste automatizado além da estrutura: os testes de
`context.rs` provam que cada fala chega isolada sob `FALA ATUAL DA OUTRA PESSOA:` e que a
política, os exemplos e as regras anti-invenção estão no prompt — **não** que um modelo
específico decida certo. Isso só se observa rodando um provedor real (ver a seção de
validação no fim deste documento).

Continua valendo a restrição de arquitetura: **nenhum detector por regex e nenhuma segunda
chamada de classificação**. A mesma chamada que gera a resposta decide, in-band, via o
marcador `[SKIP]` no início do stream. O `SkipDetector` ganhou apenas robustez de
parsing (`classify`): tolera espaço em branco à esquerda, marcador em caixa baixa e
`[SKIP]` seguido de `\n` — antes, um `"[SKIP]\n"` num único chunk não era reconhecido
como skip e vazava o marcador literal para a tela.

### Ruído do transcritor não pode virar fala

O whisper não devolve texto vazio para trechos sem fala: ele **anota** o trecho —
`[Música]`, `[BLANK_AUDIO]`, `[Aplausos]`, `♪`. Como qualquer outro texto, essas marcações
viravam segmento, e um segmento abre utterance. O efeito na sugestão era duplo e
silencioso: a marcação que chega logo depois de uma pergunta real (a) abre uma utterance
nova no mesmo turno, o que **cancela a geração já em andamento** para a pergunta, e (b) é
então corretamente classificada como `[SKIP]` — o usuário via "Nenhuma sugestão"
exatamente na fala que mais precisava de resposta.

A filtragem mora em `transcription::whisper_provider::strip_non_speech_annotations`, que é
onde o vocabulário de anotação é conhecido — não na timeline, que não deve saber quais
marcações um transcritor específico inventa. Regra conservadora: colchetes e notas musicais
são removidos em qualquer posição do texto (o whisper nunca envolve fala real em
colchetes); `(...)` só é descartado quando é o segmento inteiro, porque parênteses
aparecem em fala transcrita de verdade. Se o que sobra é vazio, `TranscriptSegment::
from_transcript` já devolve `None` e nada entra na timeline.

## Logs estruturados

Nomes fixos, para poder filtrar por evento em vez de por texto livre (nenhum deles
registra API key, prompt completo ou credencial):

| Log                                 | Onde                        | Quando                                            |
| ----------------------------------- | --------------------------- | ------------------------------------------------- |
| `session_started`                   | `begin_session` (engine) + `start_session` (timeline) | sessão nova instalada (com `previous_session_id`) |
| `session_ending`                    | `end_session` (engine + timeline) | início do teardown (nº de gerações e turnos)    |
| `session_state_cleared`             | `end_session` (engine) + `reset_for_new_session` (assembler) | histórico/coleções apagados, com quantidades |
| `session_ended`                     | `ConversationTimeline::end_session` | fronteira fechada na timeline                 |
| `generation_trigger_received`       | `process_conversation_events`   | `UtteranceFinalized` elegível chegou              |
| `generation_rejected_wrong_session` | `trigger_generation`            | gatilho de sessão não-ativa/encerrando            |
| `generation_cancelled_session_end`  | `end_session`                   | geração em voo cancelada pelo encerramento        |
| `generation_event_discarded_stale`  | `publish_*` / `run_generation`  | evento suprimido por sessão/geração obsoleta      |
| `context_built`                     | `run_generation`                | com `context_turn_count`/`context_character_count` |
| `skip_detected`                     | `run_generation`                | `SkipDetector` decidiu `Skip`                     |
| `terminal_state`                    | `publish_terminal_event`        | estado terminal único daquela geração             |

O prompt sanitizado (`BuiltContext::sanitized_preview`, linhas truncadas em 120
caracteres) não vai para o log: ele viaja só no evento `Diagnostics`, exibido apenas em
`DeveloperToolsScreen`.

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

- `session_id`, `turn_id`, `utterance_id`, `generation_id`, `provider`, `model` —
  identificação completa da geração (ver "Isolamento por sessão"): permite conferir, na
  própria UI de diagnóstico, a que sessão pertence cada resposta exibida.
- `prompt_preview`, `context_turn_count`, `context_character_count` — o prompt
  sanitizado que de fato foi enviado (linhas truncadas em 120 caracteres) e o tamanho do
  bloco de contexto. É a forma de inspecionar visualmente que nenhum texto de uma sessão
  anterior entrou no prompt, sem depender só dos testes.
- `request_started` — epoch ms de quando a chamada ao provedor começou.
- `http_status` — código HTTP da resposta bem-sucedida, `None` se a conexão falhou antes
  disso (hipótese "a requisição nem chega ao provedor").
- `first_chunk_received` — epoch ms do primeiro chunk do stream, `None` se nenhum chegou.
- `raw_prefix` — até ~80 caracteres brutos recebidos do provedor, **antes** de qualquer
  filtragem do `SkipDetector` — permite confirmar se o modelo de fato respondeu `[SKIP]`
  literalmente, em vez de supor.
- `skip_detected` — se o `SkipDetector` decidiu `Skip`.
- `echo_suppressed_characters` — quantos caracteres o `EchoGuard` descartou por serem
  repetição da própria fala. Sem esse número, uma resposta que era só eco (portanto
  integralmente suprimida) chega à UI idêntica a uma resposta vazia do modelo.
- `cancel_reason` — hoje só existe uma causa possível: `"new_utterance"` (uma nova
  utterance no mesmo turno substituiu esta geração).
- `latency_ms` — duração total da geração, do início da chamada ao provedor até o
  evento final.
- `final_text_length` — tamanho (em caracteres) do texto final acumulado.
- `event_emitted` — o estado final, como string: `"skipped"`, `"error"`, `"cancelled"`,
  `"completed_empty"`, `"completed_with_text"` ou
  `"discarded_stale"`. `completed_empty` e `completed_with_text` derivam de `Completed`
  conforme o texto final estar vazio (ou só espaço em branco) ou não — este é justamente o
  par de estados que antes colapsava, na UI, na mesma mensagem que `skipped`.
  `discarded_stale` significa que a geração terminou mas nada foi publicado porque a
  sessão já havia mudado.
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
- **`context.rs`** — monta o `ResponseRequest` a partir do histórico de turnos **da sessão
  ativa** e da utterance que acabou de finalizar, devolvendo um `BuiltContext`
  (`request`, `context_turn_count`, `context_character_count`, `sanitized_preview`). Teto
  de 4 turnos e 5000 caracteres de histórico (`MAX_HISTORY_TURNS`, `MAX_HISTORY_CHARS`),
  160 tokens de saída (`MAX_OUTPUT_TOKENS`), `temperature = 0.2` (`TEMPERATURE`) —
  contexto e saída limitados de propósito para manter prompt pequeno e latência baixa em
  vez de reenviar a conversa inteira a cada geração; baixa `temperature` favorece uma
  resposta direta e previsível em vez de variedade criativa, o que também ajuda a manter a
  latência estável entre chamadas. O módulo não conhece "a conversa": ele só vê a fatia
  que o `ResponseEngine` entregou, o que torna a contaminação entre sessões impossível de
  se originar aqui. Ver "Estrutura do prompt e política de `[SKIP]`" acima para o formato
  em três blocos e o texto do `SYSTEM_PROMPT`.
- **`skip_detector.rs`** — `SkipDetector` consome os deltas do stream incrementalmente e
  decide, com o menor atraso possível, se o texto acumulado começa com `[SKIP]` (suprime a
  resposta inteira) ou diverge dele (libera o texto acumulado como conteúdo real, mais
  qualquer delta seguinte, direto). Máquina de estados simples: `Pending` enquanto o buffer
  ainda é um prefixo do marcador, decide `Skip`/`NotSkip` assim que diverge ou completa o
  marcador. `classify` normaliza espaço em branco à esquerda e caixa, e trata `[SKIP]`
  seguido de qualquer coisa (tipicamente `\n`) como skip — nenhum regex, nenhuma segunda
  chamada ao LLM.
- **`echo_guard.rs`** — `EchoGuard`, segundo filtro do stream, depois do `SkipDetector` e
  antes de qualquer `Delta`. Modelos locais menores tratam o prompt como texto a continuar
  e às vezes começam **repetindo a fala** em vez de respondê-la: numa sessão real a
  primeira sugestão exibida foi a própria pergunta, palavra por palavra, e em outra a
  pergunta voltou reformulada ("...usar monolitos?" → "...usar micro-service?") antes da
  resposta de verdade. O prompt já proíbe isso; o guarda é a rede para quando o modelo
  desobedece, porque devolver a pergunta é pior que não sugerir nada. Ele compara o texto
  **gerado** com a fala **conhecida** que originou a geração (normalizando caixa,
  pontuação e espaçamento) e, se o começo for eco — prefixo em qualquer direção, ou ≥70%
  de tokens em comum na primeira frase — descarta só esse trecho e deixa passar o que vem
  depois. **Não é um detector de perguntas:** ele nunca decide se uma fala merece resposta,
  essa decisão continua sendo do modelo, in-band, via `[SKIP]`. É a mesma natureza de
  `strip_non_speech_annotations` — higiene de saída sobre uma entrada conhecida.
  Custo de latência no caso comum: nenhum. Uma resposta que não começa repetindo a pergunta
  diverge no primeiro caractere e passa direto a partir dali; só um começo que coincide com
  a fala fica retido, e ainda assim com teto (fim da primeira frase ou 32 caracteres além
  do tamanho da fala). Falas com menos de 12 caracteres normalizados nunca são guardadas.
  O quanto foi suprimido aparece em `GenerationDiagnostics::echo_suppressed_characters`,
  senão um eco integralmente descartado chegaria à UI indistinguível de uma resposta vazia.
- **`engine.rs`** — `ResponseEngine`: mantém o provedor ativo, a configuração e um único
  `Mutex<SessionState>` com o `session_id` ativo, a flag `ending`, o `CancellationToken`
  raiz, o histórico rolante de até 20 turnos finalizados (`MAX_HISTORY_TURNS`) e o mapa de
  gerações em andamento por `TurnId`. `begin_session`/`end_session` são a fronteira
  descrita em "Isolamento por sessão". `trigger_generation` valida a sessão, cria um
  `generation_id` monotônico e um token **filho** do token raiz; se já havia uma geração
  para aquele turno, ela é marcada como terminal (`Superseded`) e cancelada antes de
  iniciar a nova. `process_conversation_events` é o ponto de entrada chamado a cada lote de
  eventos da timeline: acumula turnos finalizados no histórico da sessão que os produziu e
  dispara geração em `UtteranceFinalized` de turnos elegíveis cujo motivo de finalização
  represente fim de fala, montando um `GenerationTrigger` (sessão, utterance, revisão,
  instante de finalização com relógio monotônico, motivo, gap configurado, silêncio
  observado). `run_generation` monta um `GenerationDiagnostics` por geração — incluindo as
  métricas de latência derivadas do `GenerationTrigger` e dos instantes capturados durante
  o streaming (primeiro chunk HTTP, primeiro texto visível) — publica cada evento via
  `publish_stream_event`/`publish_terminal_event` (que revalidam sessão e geração corrente)
  e fecha em `finish_generation`, chamado em **todo** caminho de saída (skip, erro,
  cancelamento, supersessão, encerramento de sessão ou conclusão natural) para garantir que
  nenhum retorno adiantado esqueça de liberar o slot de `generations` daquele turno — é
  esse fechamento incondicional que garante que uma geração seguinte para o mesmo turno
  nunca veja um estado "fantasma" de uma anterior já encerrada (coberto por teste, ver
  "Testes" abaixo). Genérico sobre `R: tauri::Runtime` em vez de `AppHandle` fixo
  (`= AppHandle<Wry>`) só para poder ser exercitado com `tauri::test::mock_app` sem uma
  janela/webview real.
- **`events.rs`** — evento `response://suggestion-event`
  (`ResponseSuggestionEvent::{Started, Delta, Completed, Skipped, Cancelled, Error,
  Diagnostics}`), cada variante carregando `session_id` + `turn_id` + `generation_id`. O
  `generation_id` deixa o frontend descartar eventos de uma geração já superada; o
  `session_id` é redundância defensiva e material de diagnóstico — o descarte de eventos de
  sessão encerrada acontece no backend, antes da emissão, não no frontend. `Diagnostics`
  carrega um `GenerationDiagnostics` completo (ver acima) e é emitido ao final de toda
  geração cuja sessão ainda esteja ativa, além do evento "de negócio" correspondente
  (`Skipped`/`Error`/`Cancelled`/`Completed`).
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

`features/session/responseSuggestionViewModel.ts` (movido de `src/` para dentro de
`features/session/` na reformulação de UI documentada em `docs/frontend-architecture.md`
— mesma lógica, só reorganizada por domínio) reduz os eventos de
`response://suggestion-event` num `Record<UtteranceId, SuggestionState>` via
`applyResponseSuggestionEvent`. Segue a mesma semântica de supersessão do backend:
eventos que não sejam `started` só são aplicados se o `generation_id` do evento ainda
casar com o `generationId` armazenado para aquela fala — eventos de uma geração já
cancelada são descartados silenciosamente. `SuggestionStatus` tem sete valores:
`preparing`, `streaming`, `completed_with_text`, `completed_empty`, `skipped`,
`cancelled`, `error` — o evento `completed` do backend vira `completed_with_text` ou
`completed_empty` conforme o texto final (`.trim()`) estar vazio ou não.

**A chave é a utterance, não o turno — e é por isso que todo evento público carrega
`utterance_id`.** Um `ConversationTurn` agrupa tudo que a outra pessoa falou enquanto
manteve a palavra (até `turn_inactivity_timeout_ms`, 20s por padrão) e pode conter
várias perguntas seguidas. Indexando por turno, a resposta à segunda pergunta
sobrescrevia, no mesmo lugar, a resposta à primeira — que o usuário podia ainda estar
lendo. A utterance é a unidade que de fato corresponde a uma sugestão (uma pergunta, uma
resposta), então cada uma tem seu próprio registro e nada é substituído. O `turn_id`
continua no estado, mas só como o argumento de `regenerate_suggestion_command`, que é
por turno.

A tela é um **feed cronológico**, não um slot único: `features/session/SuggestionFeed.tsx`
empilha um `features/session/ExchangeItem.tsx` por fala elegível da outra pessoa (a fala,
secundária, e logo abaixo a sua sugestão — o elemento com maior destaque tipográfico da
janela, ver `docs/design-system.md` §Janela de sessão). O que é novo entra **embaixo**;
nada acima é apagado ou trocado. O auto-scroll só acompanha o fim quando o usuário já
está no fim — se ele rolou para cima para reler uma resposta anterior, uma fala nova não
arranca a tela dele. Isso substitui o antigo `SuggestionPanel`/`TranscriptPeek` (painel
único + última fala) e, com ele, o mecanismo de `previousText`, que existia só para
disfarçar a sobrescrita: com uma entrada por fala, a resposta anterior continua
literalmente na tela, não numa cópia de fallback.

**Fronteira de sessão no frontend (fiação mínima, sem redesign).** Iniciar uma sessão em
`app/router.tsx` agora chama `startConversationSession()`
(`conversation_start_session_command`) depois de parar qualquer captura anterior e antes
de iniciar a nova — é o que abre a fronteira no backend em vez de deixar a sessão anterior
seguir ativa. `useConversationTimeline` e `useResponseSuggestions` escutam
`session_started`/`session_ended` e zeram, respectivamente, turnos/utterances e
sugestões/diagnósticos. Isso é limpeza de UI, **não** é o mecanismo de isolamento: mesmo
sem essas linhas, o backend não constrói prompt, não executa geração e não emite evento de
uma sessão encerrada. Elas existem para a tela não continuar exibindo o que já foi
descartado na origem.

`applyResponseSuggestionDiagnostics` reduz os eventos `diagnostics` separadamente, num
`Record<TurnId, ResponseSuggestionDiagnostics>` só usado pelas ferramentas de
desenvolvedor (`features/developer-tools/DeveloperToolsScreen.tsx`) — não afeta
`SuggestionState`, e não aparece na experiência normal (ver
`docs/design-system.md` §Complexidade ocultada). Escolher provedor/modelo/URL base/
`ollama_keep_alive` e gerenciar a API key acontece em
`features/ai-provider/{OllamaPanel,CloudProviderPanel}.tsx` (onboarding) e
`features/settings/SettingsScreen.tsx` (reentrante, mesmos componentes). O controle de
`same_speaker_utterance_gap_ms` em runtime (atalhos para 1200/1500/1800/2200ms) mudou de
um card sempre visível em modo dev (`import.meta.env.DEV`) para dentro de
`DeveloperToolsScreen`, atrás do toggle explícito "Modo de desenvolvedor" em
Configurações.

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

  Os testes de fronteira de sessão vivem no mesmo arquivo: encerrar a sessão limpa
  **todas** as coleções (`ending_a_session_clears_every_conversational_collection`);
  encerrar duas vezes é idempotente; `session_ended` e `session_started` aparecem nessa
  ordem; eventos finalizados carregam a sessão que os produziu; utterances finalizadas
  pelo encerramento são marcadas como `session_ended`; um timer agendado na sessão A não
  finaliza nada depois de a sessão B começar
  (`a_timer_from_the_previous_session_cannot_finalize_anything_in_the_new_one`) e a sessão
  nova tem seu próprio timer funcionando (`the_new_session_has_its_own_working_timer`).
- **`response_provider/context.rs`** — inspeciona o prompt montado, não só a API:
  `prompt_contains_only_supplied_history` prova que texto de uma sessão anterior (isto é,
  turnos não passados em `history`) não aparece em lugar nenhum do prompt;
  `separates_context_from_current_speech` e `excludes_current_turn_from_context_section`
  verificam os três blocos e que a fala atual não se duplica dentro do contexto;
  `required_utterances_reach_the_model_isolated_as_current_speech` cobre as falas que
  precisam obrigatoriamente chegar isoladas ao modelo (pergunta direta, pedido de exemplo,
  pedido de explicação, imperativo, desafio de entrevista) e o contraste com um
  `"Perfeito."` isolado; `system_prompt_states_skip_policy` fixa a política de `[SKIP]` no
  texto do prompt.
- **`response_provider/skip_detector.rs`** — além dos casos originais (marcador completo em
  um chunk, partido entre chunks, divergência no meio, stream vazio), cobre `[SKIP]` com
  `\n` no fim, com espaço em branco à esquerda, em caixa baixa, e espaço em branco isolado
  ainda pendente.
- **`response_provider/echo_guard.rs`** — eco literal da fala inteira (nada sai), eco
  reformulado seguido da resposta (só a resposta sai), eco reconhecido atravessando
  fronteira de chunk, resposta normal liberada já no primeiro chunk (a garantia de que o
  guarda não custa latência), resposta que reaproveita as palavras iniciais da pergunta mas
  segue por outro caminho (não é eco), fala curta demais para ser guardada, e texto sem
  pontuação nenhuma liberado no teto de segurança.
- **`response_provider/engine.rs`** — `ResponseEngine::for_test` injeta um
  `ResponseProvider` fake (`FakeProvider`) sem tocar o `config_store` real. Ele registra
  **todo** `ResponseRequest` recebido (`prompts()`, `request_count()`), o que permite
  afirmar tanto o que foi enviado quanto o que *não* foi enviado, e tem modos
  `RepliesWith`/`Hangs`/`FailsRequest`/`FailsMidStream`/`Scripted` (este último dirigido
  por um canal, para controlar o timing do stream a partir do teste). Os testes usam
  `tauri::test::mock_app()` (por isso
  `trigger_generation`/`run_generation`/`finish_generation`/`process_conversation_events`
  são genéricas sobre `R: tauri::Runtime` em vez de fixas em `AppHandle<Wry>`) e um
  listener (`app.listen_any`) capturando os eventos emitidos. Cobrem, por grupo:
  - **Isolamento** — sessão nova começa com contexto vazio; gatilho de sessão anterior é
    rejeitado; histórico de outra sessão é recusado; `history_snapshot` devolve `None`
    para sessão obsoleta; encerrar a sessão suprime `delta` e estado terminal da geração
    em voo; uma geração da sessão anterior não bloqueia o mesmo turno na sessão nova; o
    prompt classifica a utterance atual, não o turno inteiro.
  - **Ciclo de vida** — `end_session` cancela o token da geração ativa; sessão nova nunca
    herda token cancelado; cada token de geração é filho do token da sessão; encerrar duas
    vezes é no-op; gatilho durante o encerramento é rejeitado; finalizações de
    `capture_stopped`/`session_ended` nunca disparam geração.
  - **`[SKIP]` e estados terminais** — marcador termina como `skipped` e libera o estado;
    texto real termina como `completed_with_text`; erro no meio do stream encerra a
    geração exatamente uma vez; geração superada emite exatamente **um** evento terminal.
  - **Concorrência** — gerações de turnos diferentes coexistem; nova utterance no mesmo
    turno cancela a anterior; estado é liberado após conclusão natural (nenhum
    cancelamento "fantasma" depois); `end_of_speech_to_first_visible_token_ms` e os demais
    campos de latência são de fato calculados a partir do `GenerationTrigger`.
  - **Roteiro A/B/C** — `sessions_a_b_and_c_script` automatiza o cenário de validação
    manual: sessão A responde uma pergunta e faz `[SKIP]` numa confirmação; a fronteira
    para B não emite nenhum evento de sugestão e o histórico volta a zero; o prompt de B
    contém a pergunta de B e **não** contém nem `"monolito"` nem `"Perfeito."` de A; em C,
    uma geração lenta é interrompida pelo encerramento e o delta que chega depois não
    produz `delta`, `completed`, `error` nem `skipped` na sessão seguinte.
- **`response_provider/ollama.rs`** — um servidor HTTP/1.1 mínimo, escrito à mão sobre
  `tokio::net::TcpListener` (sem crate de mock), captura o corpo bruto da requisição para
  confirmar que `keep_alive`, `options.num_predict`, `options.temperature` e
  `think: false` realmente vão no JSON enviado ao Ollama — e que `keep_alive` fica
  totalmente ausente (não `null`) quando não configurado.
- **`src/features/session/responseSuggestionViewModel.test.ts`** — cobre o chaveamento
  por `utterance_id` (inclusive o caso central: duas perguntas no **mesmo turno**
  produzem duas entradas coexistentes, nenhuma sobrescrita), o descarte de eventos sem
  `utterance_id`, a supersessão por `generation_id` e a distinção
  `completed_with_text`/`completed_empty`.

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
  `cargo test --target x86_64-unknown-linux-gnu` (179 testes, todos passando,
  repetidamente, incluindo em paralelo — cobre parsing de streaming NDJSON/SSE
  (`net.rs`), montagem de contexto/prompt (`context.rs`), a máquina de estados do
  `SkipDetector` (marcador completo em um chunk, partido entre chunks, divergência no
  meio do marcador, stream vazio), carregamento/salvamento/corrupção de
  `config_store.rs`, mapeamento de erros do `keyring` em `secrets.rs`, o timer dedicado
  da utterance com relógio de teste pausado/avançado (`conversation.rs`), o disparo e
  ciclo de vida da geração com um provider fake e `tauri::test::mock_app`
  (`engine.rs`), e o corpo exato da requisição HTTP enviada ao Ollama
  (`ollama.rs`) — ver "Testes" acima para a lista completa por arquivo.
- **Isolamento entre sessões, por teste e não por inspeção da UI:** com um provider fake
  que registra todo prompt recebido, está verificado neste sandbox que, depois de
  `end_session`, o backend (a) não monta prompt com conteúdo da sessão anterior
  (`prompt_contains_only_supplied_history`, `sessions_a_b_and_c_script`), (b) não inicia
  geração a partir de gatilho de sessão encerrada
  (`a_trigger_from_a_previous_session_is_rejected`,
  `a_trigger_while_the_session_is_ending_is_rejected`) e (c) não emite `started`/`delta`/
  terminal de uma geração cuja sessão acabou
  (`ending_a_session_suppresses_deltas_and_terminal_events_of_the_generation_in_flight`,
  fim do roteiro A/B/C). O roteiro A/B/C foi executado **automatizado com provider fake**,
  não contra um Ollama real.
- `npm run typecheck`, `npm run lint`, `npm run build` — limpos, incluindo os reducers
  `applyResponseSuggestionEvent`/`applyResponseSuggestionDiagnostics` e seus testes
  manuais (`responseSuggestionViewModel.test.ts`, rodados via `npx tsx`), cobrindo a
  distinção `completed_with_text`/`completed_empty`, o chaveamento por `utterance_id`
  (duas perguntas no mesmo turno coexistindo) e o preenchimento de
  `ResponseSuggestionDiagnostics` a partir do evento bruto.

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
  empiricamente contra transcrições reais). Os testes provam que a fala atual chega ao
  modelo isolada, com a política de `[SKIP]` explícita no `SYSTEM_PROMPT` e que o parser
  de `[SKIP]` é robusto — **não** provam que um LLM real deixará de responder `[SKIP]` a
  uma pergunta legítima. Essa parte do §6 só pode ser confirmada rodando contra um modelo
  de verdade; se ainda ocorrer, o próximo passo é ler `raw_prefix` e `prompt_preview` no
  painel de diagnóstico antes de mudar código.
- O roteiro manual A/B/C contra um provedor real (Ollama ou nuvem), com áudio de verdade:
  aqui ele existe apenas como teste automatizado com provider fake. Windows/WASAPI e
  keychain real também não foram exercitados — este ambiente é WSL2/Linux.
- Comportamento do cancelamento/substituição de geração (turno que continua sendo falado
  enquanto uma sugestão já está sendo gerada) sob timing real de fala humana, em vez de
  timers/eventos sintéticos de teste.
- Teste manual da UI (`ResponseProviderSettings`, painel de sugestão em streaming) em um
  app Tauri rodando de fato — este ambiente é código-only, sem sessão gráfica para abrir
  o app.
