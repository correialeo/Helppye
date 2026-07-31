# Normalização de transcrição

A camada entre o resultado final do provider de transcrição e a Conversation Timeline.

Código: `src-tauri/src/normalization/`.

## O problema

Antes desta camada, o único tratamento de texto era `normalize_segment_text` dentro da
timeline — colapso de espaços duplicados. Tudo o mais que o transcritor produz seguia
direto para o prompt:

| O transcritor entrega | O que o modelo entende |
| --- | --- |
| `"micro serviços"` | duas palavras comuns, não a arquitetura |
| `"ddd"` | sigla de telefone, não Domain-Driven Design |
| `"rabbit mq"` | nada em particular |
| `"kubernets"` | erro de digitação sem referente |
| `"e aí,, funciona!!!"` | ruído de pontuação no meio da pergunta |
| `"me conta um caso real"` | frase começando em minúscula, sem sinal de início |

Em uma conversa técnica isso não é cosmético: `"micro serviços"` e `"microserviços"` levam
a respostas diferentes, e a resposta errada é a única que o usuário vai ler em voz alta.

## Escopo — e o que a camada não faz

Duas regras definem tudo:

**1. Determinística e barata.** Nada aqui chama modelo, rede ou disco. Ela roda no caminho
crítico entre o `Final` do provider e a timeline. Qualquer I/O aqui seria somado exatamente
na métrica que o produto otimiza (`end_of_speech_to_first_visible_token_ms`) e pago em
**toda** fala, inclusive nas que terminariam em `[SKIP]`.

**2. Não altera sentido.** O vocabulário é uma lista **fechada e configurável** de termos
técnicos e nomes próprios, casada por palavra inteira. Uma correção global agressiva
("consertar tudo que parece errado") transformaria fala legítima em outra coisa — e a fala
corrompida seria a única versão que a timeline veria.

Portanto: sem reescrita semântica, sem resumo, sem LLM, sem tradução, sem diarização, sem
inferência de intenção.

## O texto original nunca é descartado

```rust
pub struct TranscriptNormalizationResult {
    pub raw_text: String,
    pub normalized_text: String,
    pub normalization_changes: Vec<NormalizationChange>,
}
```

`TranscriptSegment` carrega os três campos até a timeline. Diagnóstico e modo de
desenvolvedor mostram o bruto; o prompt usa o normalizado. Sem guardar o bruto, uma
normalização suspeita ("por que 'micro' virou 'microserviços' aqui?") seria impossível de
investigar depois do fato — a única evidência teria sido sobrescrita.

Cada mudança é registrada com tipo, antes e depois:

```rust
pub enum NormalizationChangeKind {
    Whitespace,
    RepeatedPunctuation,
    Capitalization,
    Vocabulary,
}
```

É o que torna a camada **auditável**: uma alteração inesperada aparece como um item na
lista, e não como um texto misteriosamente diferente.

## Pipeline determinístico (`deterministic.rs`)

A ordem das etapas importa e não é arbitrária:

```
1. espaços        colapsa duplicados; remove espaço antes de , . ! ? ; :
2. pontuação      ,, → ,   !!! → !   .... → ...   (`...` legítimo é preservado)
3. vocabulário    casamento por palavra inteira sobre texto já regular
4. capitalização  primeira letra de cada frase
```

Espaços primeiro porque a pontuação precisa estar adjacente à palavra que pontua.
Capitalização **por último** porque capitalizar antes do vocabulário faria `"ddd"` virar
`"Ddd"` e deixar de casar com o alias.

### Casamento do vocabulário

- **Por palavra inteira.** Sem isso, `"ddd"` casaria dentro de `"adddendum"` e a correção
  viraria corrupção.
- **Insensível a caixa e a acento** (`fold_word`): `"monolito"` casa `"monólito"`,
  `"Micro Serviços"` casa `"microserviços"`.
- **Alias mais longo primeiro.** O índice é ordenado por comprimento decrescente para que
  `"micro"` sozinho não consuma o começo de `"micro serviços"`.
- **Separadores internos limitados.** `"micro serviços"` e `"micro-serviços"` são o mesmo
  termo partido; `"micro. Serviços"` não é — ponto no meio significa frases diferentes, e
  juntar as duas alteraria a fala.

### Vocabulário semente

Curto de propósito (`vocabulary.rs`): apenas termos técnicos, nomes de produto e siglas —
nada de palavra comum, cuja "correção" mudaria o que a pessoa disse.

```
DDD · SOLID · Docker · Kubernetes · microserviços · microservices · monólito ·
Entity Framework · RabbitMQ · Bling · Stripe
```

Ampliar esse critério é o caminho mais rápido para a camada começar a alterar sentido. A
regra para adicionar uma entrada: o termo aparece nas conversas-alvo **e** o erro de
transcrição muda o que o modelo entende. `"funcionalidade"` transcrito como
`"funcionalidades"` não qualifica; `"ddd"` qualifica.

O usuário adiciona entradas em runtime via `transcription_add_vocabulary_entry_command`
(e lê a lista com `transcription_vocabulary_command`). Adicionar reconstrói o
`DeterministicNormalizer` e o instala no runtime — a lista não é recompilada no binário.

## Modos (`correction.rs`)

```rust
pub enum TranscriptCorrectionMode {
    Disabled,          // texto exatamente como o provider entregou
    DeterministicOnly, // DEFAULT
    Contextual,        // determinístico + corretor contextual, se houver um registrado
}
```

`Contextual` está **contratualmente representado e deliberadamente não ligado** ao caminho
crítico nesta versão. A tentação óbvia é mandar cada transcrição para um LLM "consertar"
antes da timeline. Isso custaria uma chamada de modelo inteira entre o fim da fala e a
geração da resposta — somada na mesma métrica de UX, e paga em toda fala. Pior: o corretor
teria licença para reescrever a pergunta, e a resposta passaria a ser sobre o texto
reescrito, não sobre o que a pessoa perguntou.

Sem corretor registrado, `Contextual` se comporta como `DeterministicOnly` (com log
`warn`), em vez de bloquear a transcrição esperando algo que não existe. Quando um corretor
existir, ele recebe a saída determinística (nunca o bruto) e contexto **só da sessão
atual** (`ContextualCorrectionInput`), e precisa ser chamado fora do caminho que bloqueia a
entrada do segmento na timeline.

Comandos: `transcription_correction_mode_command`,
`transcription_set_correction_mode_command`.

## O que é preservado

A normalização mexe em **texto**, e só em texto. Atravessam a camada intactos:

- `source` (`Microphone` / `SystemOutput`) e o speaker derivado dele;
- `started_at` / `ended_at` no relógio monotônico do processo;
- `session_id` — a normalização é stateless, então não há nada que possa vazar entre
  sessões;
- Unicode: acentos, cedilhas, travessões e emoji sobrevivem, e a capitalização funciona
  sobre letra acentuada (`"é isso"` → `"É isso"`, `"çedilha"` → `"Çedilha"`).

Texto vazio ou só espaço passa sem alteração e sem pânico.

## Telemetria

Cada normalização registra o marco `NormalizationCompleted` e o
`normalization_change_count` (ver a documentação de telemetria). Uma contagem que dispara
de repente é o primeiro sinal de que uma entrada de vocabulário nova está casando mais do
que devia.

Log de `before`/`after` é nível `trace`, porque isso é conteúdo de reunião.
