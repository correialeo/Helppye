# Detecção de perguntas

## Conceito

Nesta fase, o Helppye classifica como `QuestionDetection` perguntas diretas e
solicitações do entrevistador que exigem resposta do usuário. Isso inclui frases como
"fale sobre você" ou "me descreva uma situação", mesmo sem ponto de interrogação.

O detector roda apenas sobre `ConversationTurn`, nunca sobre `TranscriptSegment`
isolado. A fonte elegível é somente `speaker = OtherPerson` e
`source = SystemOutput`, para não disparar perguntas na fala do próprio usuário.

## Regras

O provider inicial é `RuleBasedQuestionDetector`. Ele usa listas centralizadas de:

- pontuação interrogativa (`?`);
- pronomes e advérbios interrogativos;
- construções dirigidas ao usuário, como "você pode" e "me explique";
- padrões comuns de entrevista técnica e comportamental;
- perguntas de sim/não dirigidas ao usuário, como "você chegou a" e "você já";
- fragmentos comportamentais acumuláveis, como "uma situação em que", "como reagiu"
  e "o que fez";
- escolhas contrastivas, como "ou só";
- marcadores negativos, como "como disse" e "ele explicou por que".

Não há LLM, correção do Whisper, reescrita semântica ou análise comportamental nesta
fase.

## Normalização

Antes da análise, o texto é convertido para lowercase, espaços duplicados são
normalizados e ruídos textuais seguros como `[inaudível]` são removidos. A pontuação
original é preservada para extração e exibição da pergunta candidata.

## Confiança

A confiança é determinística e limitada entre `0.0` e `1.0`. Os pesos iniciais são:

- termina com `?`: `+0.45`;
- começa com termo interrogativo: `+0.35`;
- contém construção interrogativa: `+0.30`;
- contém padrão de entrevista: `+0.40`;
- possui verbo dirigido ao usuário: `+0.30`;
- pergunta de sim/não dirigida ao usuário: `+0.60`;
- fragmento comportamental: `+0.24` por fragmento;
- escolha contrastiva: `+0.20`;
- erro leve de transcrição em padrão conhecido: `+0.30`;
- frase muito curta: `-0.20`;
- palavra interrogativa isolada: `-0.20`;
- marcador discursivo não interrogativo: `-0.45`;
- cláusula interrogativa embutida em frase declarativa: `-0.35`.

Thresholds iniciais:

- `question_threshold = 0.60`;
- `high_confidence_threshold = 0.85`.

Esses valores são parâmetros iniciais, não definitivos.

## Turnos Longos

Quando o turno contém explicação seguida de pergunta, o detector tenta extrair o trecho
final com sinal forte em vez de devolver o turno inteiro. Se há `?`, ele prefere a última
frase interrogativa, mas preserva a frase anterior quando ela também contém sinal forte.
Sem pontuação, ele procura o último trecho com pronomes, construções ou padrões fortes.

A janela inicial de utterances relacionadas é configurável e começa em 2 utterances
finais. Como `ConversationTurn` ainda não carrega o texto individual de cada utterance, a
associação visual usa `matched_utterance_ids` como sufixo conservador do turno.

## Debounce

`TurnEvent::Updated` e `TurnEvent::Finalized` alimentam o detector. Em updates, uma
pergunta entra como `Candidate` e só vira `Confirmed` após
`question_detection_debounce_ms = 800`, desde que o texto não tenha mudado. Um
`TurnFinalized` executa o detector novamente sobre o texto final e confirma
imediatamente se o score estiver acima do threshold. O debounce pendente fica inofensivo
porque a deduplicação guarda o fingerprint já confirmado.

Os testes usam tempo controlável no estado do processador; não há `sleep` real em teste
unitário.

## Deduplicação

A deduplicação usa `turn_id`, texto normalizado, fingerprint e status. Quando a pergunta
é ampliada durante um turno aberto, por exemplo de "qual foi seu maior desafio" para
"qual foi seu maior desafio e como você resolveu", a detecção existente é atualizada e
mantém o mesmo `QuestionDetectionId`.

## Eventos

O backend emite eventos no canal `question://detection-event`:

- `candidate`;
- `updated`;
- `confirmed`;
- `evaluated`;
- `dismissed`.

Esse canal é separado de `conversation://timeline-event` para manter responsabilidades
claras.

`evaluated` existe para observabilidade em modo de desenvolvimento, inclusive quando o
turno remoto ficou abaixo do threshold ou a fonte/speaker não é elegível. A avaliação
inclui `eligible`, `confidence`, `threshold`, `matched_signals`, `applied_penalties`,
`candidate_text`, `decision` e `matched_utterance_ids`.

## Falsos Positivos Conhecidos

- Solicitações educadas ou instruções podem ser classificadas como pergunta quando se
  parecem com prompts de entrevista.
- Frases muito curtas com verbo dirigido ao usuário podem precisar de mais contexto para
  serem descartadas.

## Falsos Negativos Conhecidos

- Perguntas muito coloquiais sem termos interrogativos podem ficar abaixo do threshold.
- Erros grandes de transcrição não são corrigidos semanticamente.
- Perguntas divididas em muitas frases longas ainda usam uma extração simples de sufixo.

## Limitações

O detector local não interpreta intenção profunda, ironia, domínio técnico específico ou
continuidade semântica entre muitos turnos. O contexto recebido existe para providers
futuros e hoje é usado de forma conservadora.

## Futuro Detector Semântico

A trait `QuestionDetector` já é assíncrona e recebe `ConversationTurn` mais contexto de
turnos anteriores. Um provider semântico futuro poderá substituir ou complementar o
detector por regras, mas a integração com LLMs fica fora desta fase.
