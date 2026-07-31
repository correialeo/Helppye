//! Suprime o eco da pergunta no início da resposta gerada.
//!
//! Modelos locais menores tratam o prompt como texto a continuar, não como uma tarefa a
//! executar, e às vezes começam repetindo a fala da outra pessoa — literalmente
//! ("Perfeito. Me conta um caso real..." devolvido igual) ou reformulada ("Em qual
//! situação você escolheria usar monolitos?" voltando como "Em qual situação você
//! escolheria usar micro-service?" antes da resposta de verdade). O prompt já proíbe isso
//! explicitamente; isto é a rede de segurança para quando o modelo desobedece, porque o
//! resultado visível — a sugestão sendo a própria pergunta — é pior que não sugerir nada.
//!
//! **Isto não é um detector de perguntas.** Ele nunca decide se uma fala é pergunta, nem
//! se merece resposta: essa decisão continua sendo do modelo, in-band, via `[SKIP]` (ver
//! `skip_detector.rs`). Aqui só se compara o texto **gerado** com a fala **conhecida** que
//! o originou — a mesma natureza de `strip_non_speech_annotations`: higiene de saída sobre
//! uma entrada conhecida, sem regra sobre o conteúdo da conversa.
//!
//! Custo de latência: nenhum no caso comum. O guarda só segura texto enquanto os primeiros
//! caracteres gerados coincidem com o começo da fala; uma resposta que não começa
//! repetindo a pergunta diverge no primeiro caractere e passa direto a partir dali.

/// Quantos caracteres normalizados precisam coincidir com o início da fala para o guarda
/// suspeitar de eco e começar a segurar texto. Baixo de propósito: é só o portão de
/// entrada, a decisão real vem depois, na comparação da frase inteira.
const GATE_CHARS: usize = 4;

/// Falas curtas demais ("ok", "e aí") não são guardadas: a chance de uma resposta legítima
/// começar com as mesmas poucas letras é alta e o dano de comer texto bom é maior que o de
/// deixar um eco curto passar.
const MIN_GUARDED_CHARS: usize = 12;

/// Teto de segurança para o que o guarda segura enquanto espera o fim da primeira frase,
/// em caracteres brutos. Impede que uma resposta longa sem pontuação fique represada.
const MAX_WATCH_SLACK: usize = 32;

/// Fração dos tokens da frase candidata que precisa também aparecer na fala para ela ser
/// considerada eco. Abaixo disso é resposta que por acaso reaproveita palavras da pergunta.
const ECHO_TOKEN_RATIO: f32 = 0.7;

/// Reduz a comparação ao que importa: caixa, pontuação e espaçamento variam entre a
/// transcrição e o texto do modelo sem mudar o fato de ser a mesma frase.
fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_was_space = true;
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                out.push(lower);
            }
            last_was_space = false;
        } else if !last_was_space {
            out.push(' ');
            last_was_space = true;
        }
    }
    out.trim_end().to_string()
}

fn tokens(normalized: &str) -> Vec<&str> {
    normalized.split(' ').filter(|t| !t.is_empty()).collect()
}

/// Quantos caracteres normalizados os dois textos compartilham desde o começo.
fn common_prefix_chars(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// Índice em bytes no texto **bruto** logo após o caractere que faz a forma normalizada
/// alcançar `normalized_len`. É como se recorta exatamente o trecho ecoado de dentro de um
/// chunk que já trouxe a resposta de verdade grudada nele — reproduz o mesmo estado de
/// `normalize` (alfanuméricos contam, separadores viram um espaço, sem espaço à esquerda).
fn split_after_normalized_len(raw: &str, normalized_len: usize) -> Option<usize> {
    let mut length = 0usize;
    let mut pending_space = false;
    for (index, ch) in raw.char_indices() {
        if ch.is_alphanumeric() {
            if pending_space && length > 0 {
                length += 1;
            }
            pending_space = false;
            length += ch.to_lowercase().count();
        } else {
            pending_space = true;
        }
        if length >= normalized_len {
            return Some(index + ch.len_utf8());
        }
    }
    None
}

/// A frase candidata é eco da fala se for um prefixo dela (eco truncado), se a fala for um
/// prefixo dela (eco completo seguido de mais texto na mesma frase) ou se compartilhar a
/// maior parte dos tokens (eco reformulado: uma palavra trocada no meio).
fn looks_like_echo(candidate: &str, speech: &str) -> bool {
    if candidate.is_empty() {
        return false;
    }
    if speech.starts_with(candidate) || candidate.starts_with(speech) {
        return true;
    }
    let candidate_tokens = tokens(candidate);
    let speech_tokens = tokens(speech);
    if candidate_tokens.len() < 3 || speech_tokens.is_empty() {
        return false;
    }
    let shared = candidate_tokens
        .iter()
        .filter(|t| speech_tokens.contains(t))
        .count();
    shared as f32 / candidate_tokens.len() as f32 >= ECHO_TOKEN_RATIO
}

/// O corte do eco para no último caractere alfanumérico da fala, então o que sobra começa
/// com a pontuação que fechava a pergunta ecoada (`"? "`, `". "`). Ela pertence ao eco, não
/// à resposta.
fn trim_echo_residue(rest: &str) -> String {
    rest.trim_start_matches(|c: char| !c.is_alphanumeric())
        .to_string()
}

#[derive(Debug, PartialEq, Eq)]
enum State {
    /// Ainda pode ser eco: o texto acumulado está retido.
    Watching,
    /// Decidido — tudo daqui para frente sai direto.
    Passthrough,
}

#[derive(Debug)]
pub struct EchoGuard {
    speech: String,
    buffer: String,
    state: State,
}

impl EchoGuard {
    pub fn new(utterance_text: &str) -> Self {
        let speech = normalize(utterance_text);
        let state = if speech.chars().count() < MIN_GUARDED_CHARS {
            State::Passthrough
        } else {
            State::Watching
        };
        EchoGuard {
            speech,
            buffer: String::new(),
            state,
        }
    }

    /// Recebe mais um trecho já liberado pelo `SkipDetector` e devolve o que pode ser
    /// exibido agora (string vazia = ainda retido).
    pub fn push(&mut self, text: &str) -> String {
        if self.state == State::Passthrough {
            return text.to_string();
        }
        self.buffer.push_str(text);

        let normalized = normalize(&self.buffer);
        let speech_len = self.speech.chars().count();

        // Eco literal completo: o gerado já cobre a fala inteira. Recorta exatamente o
        // trecho ecoado — o que vier depois dele é a resposta de verdade.
        if normalized.starts_with(&self.speech) {
            let index =
                split_after_normalized_len(&self.buffer, speech_len).unwrap_or(self.buffer.len());
            let rest = self.buffer[index..].to_string();
            self.state = State::Passthrough;
            self.buffer.clear();
            return trim_echo_residue(&rest);
        }

        // Eco literal em andamento: o gerado ainda é um prefixo da fala. Segura e espera.
        if self.speech.starts_with(&normalized) {
            return String::new();
        }

        // Divergiu. Se divergiu logo no começo, não é eco — o guarda se aposenta e nada
        // mais é retido nesta geração.
        if common_prefix_chars(&normalized, &self.speech) < GATE_CHARS {
            return self.release_all();
        }

        // Divergiu no meio: candidato a eco reformulado (uma palavra trocada). Segura até o
        // fim da primeira frase, onde a comparação por tokens passa a ser significativa, ou
        // até o teto de segurança.
        match self.sentence_end() {
            Some(index) => self.decide_at(index),
            None => {
                if normalized.chars().count() > speech_len + MAX_WATCH_SLACK {
                    let end = self.buffer.len();
                    self.decide_at(end)
                } else {
                    String::new()
                }
            }
        }
    }

    /// Stream terminado: decide com o que houver retido.
    pub fn finish(&mut self) -> String {
        if self.state == State::Passthrough {
            return String::new();
        }
        let end = self.buffer.len();
        self.decide_at(end)
    }

    /// Índice (em bytes) logo após o terminador da primeira frase do buffer, se houver.
    fn sentence_end(&self) -> Option<usize> {
        self.buffer
            .char_indices()
            .find(|(_, ch)| matches!(ch, '.' | '?' | '!' | '\n'))
            .map(|(index, ch)| index + ch.len_utf8())
    }

    /// Compara a frase candidata (`buffer[..end]`) com a fala e resolve: eco vira descarte,
    /// qualquer outra coisa é liberada. Em ambos os casos o guarda se aposenta — o eco, se
    /// existe, é sempre no começo.
    fn decide_at(&mut self, end: usize) -> String {
        let candidate = self.buffer[..end].to_string();
        let rest = self.buffer[end..].to_string();
        self.state = State::Passthrough;
        self.buffer.clear();

        if looks_like_echo(&normalize(&candidate), &self.speech) {
            // O trecho ecoado morre aqui; o que veio depois dele na mesma resposta é a
            // resposta de verdade e continua saindo normalmente.
            trim_echo_residue(&rest)
        } else {
            candidate + &rest
        }
    }

    fn release_all(&mut self) -> String {
        self.state = State::Passthrough;
        std::mem::take(&mut self.buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(guard: &mut EchoGuard, chunks: &[&str]) -> String {
        let mut out = String::new();
        for chunk in chunks {
            out.push_str(&guard.push(chunk));
        }
        out.push_str(&guard.finish());
        out
    }

    #[test]
    fn a_verbatim_echo_of_the_whole_question_produces_no_text() {
        let speech = "Perfeito. Me conta um caso real em que você optou por usar monolito.";
        let mut guard = EchoGuard::new(speech);
        assert_eq!(run(&mut guard, &[speech]), "");
    }

    #[test]
    fn a_reformulated_echo_is_dropped_and_the_real_answer_survives() {
        let mut guard = EchoGuard::new("Em qual situação você escolheria usar monolitos?");
        let generated = "Em qual situação você escolheria usar micro-service? \
Acho que depende do problema que estamos resolvendo.";
        assert_eq!(
            run(&mut guard, &[generated]),
            "Acho que depende do problema que estamos resolvendo."
        );
    }

    #[test]
    fn an_answer_that_does_not_start_like_the_question_passes_through_untouched() {
        let mut guard =
            EchoGuard::new("Eu queria que você me falasse diferença entre pilhas e filas.");
        let generated = "diferença está no comportamento de armazenamento: pilhas são LIFO.";
        assert_eq!(run(&mut guard, &[generated]), generated);
    }

    /// O caso que mais importa não custar caro: uma resposta normal não pode ficar retida
    /// esperando o fim de uma frase — ela sai no primeiro chunk, como antes do guarda.
    #[test]
    fn a_normal_answer_is_released_on_the_first_chunk() {
        let mut guard = EchoGuard::new("Em qual situação você escolheria usar monolitos?");
        assert_eq!(guard.push("Acho que "), "Acho que ");
        assert_eq!(guard.push("depende do time."), "depende do time.");
    }

    #[test]
    fn the_echo_is_recognized_across_chunk_boundaries() {
        let speech = "Como funciona a arquitetura MVC?";
        let mut guard = EchoGuard::new(speech);
        let out = run(
            &mut guard,
            &[
                "Como funciona",
                " a arquitetura MVC? ",
                "MVC divide a aplicação em três.",
            ],
        );
        assert_eq!(out, "MVC divide a aplicação em três.");
    }

    #[test]
    fn a_short_utterance_is_never_guarded() {
        let mut guard = EchoGuard::new("e aí?");
        assert_eq!(guard.push("e aí, tudo certo?"), "e aí, tudo certo?");
    }

    /// Sem pontuação nenhuma, o guarda não pode segurar a resposta indefinidamente.
    #[test]
    fn text_without_sentence_terminators_is_released_at_the_safety_cap() {
        let speech = "me conta um caso real de migração";
        let mut guard = EchoGuard::new(speech);
        let long = "me conta um caso real de migração que eu vivi e que foi bem complicado \
para o time inteiro naquele semestre";
        let out = run(&mut guard, &[long]);
        assert!(
            !out.is_empty(),
            "a resposta não pode ficar retida para sempre"
        );
    }

    /// Uma resposta legítima que começa com as mesmas palavras da pergunta mas segue por
    /// outro caminho não pode ser confundida com eco.
    #[test]
    fn an_answer_reusing_the_opening_words_is_not_treated_as_echo() {
        let mut guard = EchoGuard::new("Como você lidou com esse problema de escalabilidade?");
        let generated =
            "Como sempre faço nesses casos, comecei medindo antes de mexer em qualquer coisa.";
        assert_eq!(run(&mut guard, &[generated]), generated);
    }

    #[test]
    fn empty_stream_produces_nothing() {
        let mut guard = EchoGuard::new("Me conta um caso real em que isso deu errado.");
        assert_eq!(run(&mut guard, &[]), "");
    }
}
