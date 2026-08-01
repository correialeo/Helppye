import { buildSessionExchanges } from "./sessionTimelineViewModel";
import type { ConversationTurn, ConversationUtterance } from "../../types/conversation";

function assert(condition: boolean, message: string): void {
  if (condition) return;
  throw new Error(`assertion failed: ${message}`);
}

function run(name: string, fn: () => void): void {
  fn();
  console.log(`ok: ${name}`);
}

/**
 * Ao contrário do helper de `sessionTimelineViewModel.test.ts`, este monta `speaker` e
 * `source` de forma **independente**, de propósito: o que estes testes verificam é que o
 * frontend não reconstrói um a partir do outro, nem a partir do texto. Ele repete o que o
 * backend classificou; classificar duas vezes é o começo de classificar diferente.
 */
function utterance(
  id: number,
  speaker: ConversationUtterance["speaker"],
  source: ConversationUtterance["source"],
  text: string,
): ConversationUtterance {
  return {
    id,
    speaker,
    source,
    text,
    segments: [id],
    received_sequence: id,
    started_at: id * 1000,
    ended_at: id * 1000 + 100,
    finalized_at: id * 1000 + 100,
    revision: 1,
  };
}

function turn(
  id: number,
  speaker: ConversationTurn["speaker"],
  source: ConversationTurn["source"],
  utterances: number[],
): ConversationTurn {
  return {
    id,
    speaker,
    source,
    text: String(id),
    utterances,
    started_at: id * 1000,
    ended_at: id * 1000 + 100,
    finalized_at: null,
  };
}

run("the feed classifies by the backend speaker and source, never by the text", () => {
  // Texto de pergunta, mas o backend disse que é o usuário falando ao microfone. O
  // frontend obedece: quem decidiu foi a captura, não o conteúdo.
  const utterances = [
    utterance(1, "user", "microphone", "Em qual situação você escolheria usar monolitos?"),
    utterance(2, "other_person", "system_output", "ok"),
  ];
  const turns = [turn(1, "user", "microphone", [1]), turn(2, "other_person", "system_output", [2])];

  const exchanges = buildSessionExchanges(turns, utterances, {});

  assert(exchanges.length === 1, "só a fala classificada como da outra pessoa vira exchange");
  assert(exchanges[0]!.utteranceId === 2, "a fala do microfone não entra no feed");
});

run("a divergent speaker/source pair is not silently repaired by the frontend", () => {
  // Combinação impossível de o backend produzir hoje (`speaker_for_source` garante o par).
  // Se ela chegar mesmo assim, o frontend não pode "consertar" para o lado que parece certo:
  // ele exige os dois campos coerentes, e na dúvida a fala não vira sugestão.
  const utterances = [utterance(1, "other_person", "microphone", "e como você faria isso?")];
  const turns = [turn(1, "other_person", "microphone", [1])];

  const exchanges = buildSessionExchanges(turns, utterances, {});

  assert(exchanges.length === 0, "par divergente não é promovido a fala da outra pessoa");
});

run("a remote utterance keeps its turn even when the turn came from another source", () => {
  // Regressão do defeito relatado: enquanto o backend deixava utterances da saída de
  // sistema penduradas num turno do microfone, o frontend mostrava `OTHERS` na fala e o
  // turno errado no diagnóstico. Hoje isso não é montável no backend; aqui fica registrado
  // que o frontend também não inventa um turno para compensar.
  const utterances = [utterance(1, "other_person", "system_output", "me conta um caso real")];
  const turns = [turn(9, "other_person", "system_output", [1])];

  const exchanges = buildSessionExchanges(turns, utterances, {});

  assert(exchanges.length === 1, "a fala remota aparece");
  assert(exchanges[0]!.turnId === 9, "o turno vem do backend, não de uma inferência local");
});
