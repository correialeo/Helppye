# Benchmark de transcrição

Base reprodutível para rodar **o mesmo** conjunto de áudio contra providers de transcrição
diferentes e comparar latência, texto final, erros, confiança, normalizações, utterances
resultantes e custo estimado.

Código: `src-tauri/src/benchmark/` (harness) e `src-tauri/src/bin/benchmark.rs` (executável).

## O que entra no Git e o que não entra

| Arquivo | Versionado | Por quê |
| --- | --- | --- |
| `fixtures.json` | sim | texto esperado e vocabulário precisam de revisão humana |
| `audio/*.wav` | **não** | é fala gravada, muitas vezes de outra pessoa |
| `results/*` | **não** | o relatório contém a transcrição, ou seja, conteúdo de reunião |

As duas exclusões estão no `.gitignore` da raiz.

## Preparar um caso

1. Grave ou exporte um `.wav` (PCM 16-bit ou float 32-bit, mono ou estéreo, qualquer taxa —
   o harness reamostra para 16 kHz mono com o mesmo código da captura) em `benchmarks/audio/`.
2. Escreva a transcrição de referência à mão. É o denominador do WER: se ela estiver errada,
   o número está errado.
3. Liste em `technical_vocabulary` os termos que **têm** que sobreviver. Eles são medidos
   separadamente do WER de propósito — errar "RabbitMQ" muda o que o modelo de resposta
   entende, errar um artigo não.
4. Declare `source` (`microphone` ou `system_output`). Nunca é inferido do arquivo.

```json
{
  "fixtures": [
    {
      "id": "arquitetura-ddd",
      "audio": "audio/arquitetura-ddd.wav",
      "expected_transcript": "A gente usa DDD e microserviços com RabbitMQ.",
      "technical_vocabulary": ["DDD", "microserviços", "RabbitMQ"],
      "source": "system_output",
      "language": { "mode": "fixed", "tag": "pt" },
      "notes": "fala rápida, dois termos partidos pelo transcritor"
    }
  ]
}
```

## Rodar

```bash
cd src-tauri

# Whisper local, com um modelo já baixado
cargo run --bin benchmark -- \
  --manifest ../benchmarks/fixtures.json \
  --model ~/.local/share/helppye/models/ggml-base.bin \
  --out ../benchmarks/results
```

`--model` é obrigatório. O provider fake vive dentro dos testes (`#[cfg(test)]`) e devolve o
texto que lhe mandaram: rodar o harness contra ele daria WER perfeito e não diria nada sobre
transcritor nenhum.

`--usd-per-audio-minute <preço>` preenche `estimated_cost_usd`. Sem essa flag, um provider de
nuvem sai com custo vazio — o harness não consulta tabela de preço de ninguém, e um número
inventado seria pior que a ausência dele.

O runner adapta o mesmo fixture ao contrato declarado pelo provider. Providers
streaming recebem frames contínuos de 100 ms; providers batch recebem segmentos do
mesmo `Segmenter` usado pela captura, incluindo o flush de fim de arquivo. Portanto o
benchmark não força Whisper a transcrever pequenos frames artificiais e continua sem
conter casos especiais por nome de provider.

## Ler o resultado

Saem dois arquivos por execução, em `results/`: um JSON completo (inclui `raw_transcript` ao
lado de `normalized_transcript`) e um CSV para planilha.

As colunas que decidem alguma coisa:

- **`real_time_factor`** — acima de `1.0` o backend transcreve mais devagar do que a pessoa
  fala, e a defasagem cresce sem limite durante a reunião. É o corte para uso ao vivo.
- **`word_error_rate`** — pode passar de `1.0` quando o provider produz mais texto do que o
  esperado (alucinação, repetição).
- **`vocabulary_misses`** — termo técnico perdido é o erro que mais estraga a sugestão.
- **`utterances`** — quantas utterances a timeline montou. Duas utterances onde deveria haver
  uma significa pergunta partida ao meio, que é uma causa conhecida de resposta a meia
  pergunta (ver `docs/response-suggestion.md`).
- **`normalization_changes`** — quanto a camada determinística precisou consertar.

## Limites honestos

- Mede do áudio até a `ConversationUtterance`. **Não** mede geração de resposta: essa latência
  depende do provedor de LLM e do prompt, e misturar as duas num número só esconderia qual
  das duas regrediu.
- Não simula rede nem variação de carga.
- Um provider de nuvem só aparece aqui quando existir implementação real — a arquitetura está
  pronta (`docs/transcription-providers.md`), mas o harness não inventa backend.
