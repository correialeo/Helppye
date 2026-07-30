# Experiência de sessão ao vivo

Este documento descreve o comportamento *observável pelo usuário* durante uma sessão de
captura contínua — o que dispara a sugestão de resposta, quando ela aparece na tela, e o
que acontece quando a conversa continua. É o complemento de UX de
`docs/response-suggestion.md` (arquitetura interna, módulo por módulo) e
`CLAUDE.md` (visão geral do projeto). Para a aparência e estrutura da janela de sessão
em si (`features/session/SessionScreen.tsx` e componentes), ver `docs/design-system.md`
§Janela de sessão — este documento foca no *comportamento*, não no layout.

## Gatilho real da sugestão de resposta

A sugestão de resposta começa **automaticamente**, assim que a fala da outra pessoa
termina de forma suficiente para ser processada:

```
outra pessoa fala
        │
        ▼
VAD detecta fim da fala (silêncio sustentado, ver audio/segmentation.rs)
        │
        ▼
transcrição termina → segmento chega à Conversation Timeline
        │
        ▼
utterance permanece aberta enquanto a mesma pessoa continuar falando dentro de
same_speaker_utterance_gap_ms (1800ms por padrão)
        │
        ▼
silêncio ≥ same_speaker_utterance_gap_ms → timer dedicado finaliza a utterance sozinho
        │
        ▼
ResponseEngine inicia a geração imediatamente
        │
        ▼
streaming aparece na UI
```

**Não é preciso**, em nenhum momento deste fluxo:

- parar a captura;
- iniciar a captura de novo;
- fazer flush manual (`conversation_flush_turns_command`);
- esperar o `ConversationTurn` inteiro finalizar (ele pode continuar aberto por até
  `turn_inactivity_timeout_ms`, 20s por padrão, agrupando a conversa);
- esperar esse timeout de 20s;
- o usuário começar a falar;
- chegar outra fala/utterance depois;
- apertar nenhum atalho.

Isso é intencional, não incidental: a versão anterior deste pipeline finalizava a
utterance só *reativamente* (reavaliando o silêncio apenas quando um novo segmento de
transcrição chegava), então uma pergunta seguida de silêncio simplesmente nunca
disparava a sugestão até que *algo mais* acontecesse. O timer dedicado por utterance
(`ConversationTimeline::reschedule_utterance_timer`) existe exatamente para eliminar essa
dependência.

## Utterance vs. turno: duas decisões independentes

| Evento | Fecha a utterance? | Fecha o turno? |
|---|---|---|
| Silêncio ≥ `same_speaker_utterance_gap_ms` | Sim (`inactivity_timeout`) | Não |
| Troca de speaker (usuário ↔ outra pessoa) | Sim (`speaker_changed`) | Sim |
| Troca de fonte de áudio | Sim (`source_changed`) | Sim |
| Silêncio ≥ `turn_inactivity_timeout_ms` (20s) | — (a utterance já fechou bem antes, pelo gap) | Sim |
| Flush manual / parar captura / fim de sessão | Sim | Sim |
| Duração máxima da utterance/turno | Sim | Sim (só se o turno também estourou) |

Fechar a utterance e disparar uma sugestão são coisas diferentes: `speaker_changed` e
`source_changed` fecham a utterance mas **não** geram sugestão, porque significam que o
usuário tomou a palavra — ver `docs/response-suggestion.md`, seção "Elegibilidade".

A geração de sugestão escuta `utterance_finalized`, não `turn_finalized`. O turno acima
da utterance pode continuar aberto por até 20 segundos agrupando a conversa (útil para a
Timeline mostrar "a outra pessoa falou isso tudo seguido"), mas isso nunca atrasa a
sugestão — o contexto enviado ao LLM usa o `ConversationTurn` ainda aberto, desde que a
utterance que acabou de finalizar já esteja disponível nele.

## Continuação da fala da outra pessoa

Silêncios curtos no meio de uma frase não fecham a utterance cedo demais — só depois de
`same_speaker_utterance_gap_ms` sem nenhum segmento novo:

```
"Me conta um caso real..."
        (silêncio curto, menor que o gap)
"...em que você optou por usar monolito."
```

Aqui, a utterance só finaliza (e a geração só começa) depois do silêncio *após* a
segunda parte — o timer da primeira parte é implicitamente descartado assim que o novo
segmento chega (comparação de revisão da utterance, não um cancelamento explícito de
task).

Se a geração já tiver começado quando a continuação chega (silêncio > gap, mas a pessoa
retoma um instante depois com mais contexto real), o `ResponseEngine` cancela a geração
em andamento e inicia uma nova assim que a nova utterance finalizar — nunca deixa uma
sugestão desatualizada convivendo com uma geração baseada em contexto mais completo.

## Comportamento durante a fala do usuário

Fala do usuário (`speaker = User`, `source = Microphone`) **nunca** dispara geração — só
`speaker = OtherPerson` + `source = SystemOutput` é elegível
(`response_provider::engine::is_eligible_turn`). Especificamente:

- o usuário começar a falar não inicia uma nova geração;
- a fala do usuário entra no histórico de contexto normalmente (via
  `ConversationTimelineEvent::TurnFinalized` alimentando `ResponseEngine::push_history`);
- uma sugestão já concluída **permanece visível** enquanto o usuário fala — nada a
  apaga automaticamente;
- uma geração ainda em andamento quando o usuário começa a falar também permanece ativa
  — só é cancelada se uma nova fala *remota* (da outra pessoa) a substituir, nunca por a
  fala do usuário sozinha.

## O que a UI mostra, e quando

| Evento do backend | Estado na UI (`SuggestionState.status`) | Texto em `SuggestionPanel` |
|---|---|---|
| `started` | `preparing` | "Preparando uma sugestão..." (shimmer discreto); resposta anterior continua visível por baixo até haver algo melhor |
| primeiro `delta` com conteúdo | `streaming` | o texto chegando, com um cursor pulsante no fim |
| `completed` (texto não vazio) | `completed_with_text` | o texto final, com ações Copiar/Regenerar/Ocultar |
| `completed` (texto vazio) | `completed_empty` | "Nenhuma nova sugestão" (discreto, não ocupa o painel inteiro) |
| `skipped` | `skipped` | "Nenhuma nova sugestão"; mantém a resposta anterior visível, se houver |
| `cancelled` | `cancelled` | sem mensagem própria — é um estado de transição, a próxima geração já está a caminho |
| `error` | `error` | "Não foi possível gerar a sugestão." + [Tentar novamente] |

A UI nunca espera `completed` para começar a renderizar — o streaming aparece assim que
o primeiro delta com conteúdo chega, e a resposta concluída anterior nunca "pisca" para
um painel vazio entre uma geração e a próxima. Ver `features/session/SuggestionPanel.tsx`
e `features/session/responseSuggestionViewModel.ts` (o reducer, movido de
`src/responseSuggestionViewModel.ts` para dentro de `features/session/` na reformulação
de UI — mesma lógica, só reorganizada por pasta de domínio).

"Regenerar" (botão, ou Ctrl/Cmd+Shift+Enter — ver `docs/shortcuts.md`) dispara
manualmente uma nova geração para o turno elegível mais recente, sem esperar uma nova
utterance real. É o único gatilho de geração que não vem da Conversation Timeline —
ver `docs/frontend-architecture.md` §Um comando novo no backend, e por quê.

## Latência: o que é medido e o que se espera

Toda geração emite um evento `diagnostics` com a decomposição completa da latência (ver
`docs/response-suggestion.md`, seção "Diagnóstico em modo dev", para a lista de campos).
As duas métricas mais importantes:

- `utterance_finalized_to_request_started_ms` — o atraso de disparo em si. Meta de
  engenharia: **< 100 ms**. Se esse número estiver alto, o problema é de disparo (o que
  este trabalho corrigiu), não do provedor de LLM.
- `end_of_speech_to_first_visible_token_ms` — a métrica de UX real: do silêncio até a
  resposta aparecer na tela. Soma o atraso de disparo com a latência do provedor
  (`request_to_first_visible_token_ms`), que depende de hardware/rede/modelo e não tem
  meta fixa prometida aqui — apenas as alavancas de engenharia disponíveis para reduzi-la
  (keep-alive do Ollama, contexto menor, `think: false`, cliente HTTP reutilizado — ver
  `docs/response-suggestion.md`).

## Configuração de desenvolvimento

Diferente da versão anterior (gated só por `import.meta.env.DEV`, sempre visível em
build de desenvolvimento), o painel técnico agora é um toggle explícito e persistido —
"Modo de desenvolvedor" em Configurações (`useOnboardingStore.devMode`) — desligado por
padrão em qualquer build, inclusive `npm run dev`. Ligado, ele expõe
`DeveloperToolsScreen` (acessível por Configurações ou pelo menu ⋯ da sessão):

- Controle de `same_speaker_utterance_gap_ms` em runtime, sem rebuild, com atalhos para
  1200/1500/1800/2200ms — mesmo propósito do antigo `UtteranceGapDevControl`, agora
  dentro de `DeveloperToolsScreen` em vez de um card solto na tela principal. Valores
  muito baixos arriscam responder no meio de uma pergunta ainda incompleta — não é o
  padrão de produção.
- Turnos e utterances consolidados (IDs, revisão, segmentos, texto).
- Diagnóstico por geração, com todos os campos de `GenerationDiagnostics`, incluindo
  `finalization_reason`, `gap_ms_used`, `silence_detected_ms` e a decomposição de
  latência acima.
- "Copiar diagnóstico" — serializa turnos/utterances/diagnósticos correntes como JSON
  para a área de transferência, para colar num relato de bug.

Ver `docs/design-system.md` §Complexidade ocultada para a lista completa do que só
existe aqui.
