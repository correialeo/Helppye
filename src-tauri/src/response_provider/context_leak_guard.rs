//! Detector deterministico de copia do contexto. Nao decide qualidade semantica e nao
//! chama LLM; procura somente evidencias fortes de reproducao textual.

use std::collections::HashSet;

const MIN_LITERAL_CHARACTERS: usize = 24;
const MIN_TOKEN_SEQUENCE: usize = 6;
const STRONG_LEAK_SCORE: f32 = 0.82;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContextLeakAssessment {
    pub score: f32,
    pub strong_leak: bool,
}

pub struct ContextLeakGuard<'a> {
    references: &'a [String],
}

impl<'a> ContextLeakGuard<'a> {
    pub fn new(references: &'a [String]) -> Self {
        Self { references }
    }

    pub fn assess(&self, raw_output: &str) -> ContextLeakAssessment {
        let output = normalize(raw_output);
        if output.is_empty() {
            return ContextLeakAssessment {
                score: 0.0,
                strong_leak: false,
            };
        }

        let output_tokens: Vec<&str> = output.split_whitespace().collect();
        let output_characters = output.chars().count();
        let mut score = 0.0_f32;
        let mut reproduced_sentences = 0usize;

        for raw_reference in self.references {
            let reference = normalize(raw_reference);
            if reference.is_empty() {
                continue;
            }
            let reference_tokens: Vec<&str> = reference.split_whitespace().collect();

            // O algoritmo anterior fazia LCS por caractere em ate 1.500 x 3.000
            // posicoes para cada referencia. Isso podia executar dezenas de milhoes de
            // iteracoes depois do stream terminar e antes do primeiro texto visivel.
            // Sequencias de tokens preservam o sinal relevante (copia de fala) com um
            // limite natural dado pelo maximo de tokens da resposta.
            let common_run = longest_common_token_run(&output_tokens, &reference_tokens);
            let sequence_ratio = common_run as f32 / output_tokens.len().max(1) as f32;
            let ngram_ratio = trigram_overlap(&output_tokens, &reference_tokens);
            let token_ratio = token_overlap(&output_tokens, &reference_tokens);
            score = score.max(sequence_ratio.max(ngram_ratio).max(token_ratio * 0.9));

            if output_characters >= MIN_LITERAL_CHARACTERS
                && (reference.contains(&output) || output.contains(&reference))
            {
                score = 1.0;
            }
            if continues_reference(&output_tokens, &reference_tokens) {
                score = score.max(0.95);
            }

            reproduced_sentences += sentence_fragments(raw_output)
                .into_iter()
                .map(normalize)
                .filter(|sentence| {
                    sentence.chars().count() >= MIN_LITERAL_CHARACTERS
                        && reference.contains(sentence)
                })
                .count();
        }

        if reproduced_sentences >= 2 {
            score = 1.0;
        }
        let strong_leak = score >= STRONG_LEAK_SCORE
            && (output_tokens.len() >= MIN_TOKEN_SEQUENCE
                || output_characters >= MIN_LITERAL_CHARACTERS);

        ContextLeakAssessment { score, strong_leak }
    }
}

pub(crate) fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            out.extend(ch.to_lowercase());
        } else if !out.ends_with(' ') {
            out.push(' ');
        }
    }
    out.trim().to_string()
}

fn token_overlap(output: &[&str], reference: &[&str]) -> f32 {
    if output.is_empty() {
        return 0.0;
    }
    let reference: HashSet<&str> = reference.iter().copied().collect();
    let shared = output
        .iter()
        .filter(|token| reference.contains(**token))
        .count();
    shared as f32 / output.len() as f32
}

fn trigram_overlap(output: &[&str], reference: &[&str]) -> f32 {
    if output.len() < 3 {
        return 0.0;
    }
    let reference_ngrams: HashSet<(&str, &str, &str)> = reference
        .windows(3)
        .map(|window| (window[0], window[1], window[2]))
        .collect();
    let shared = output
        .windows(3)
        .filter(|window| reference_ngrams.contains(&(window[0], window[1], window[2])))
        .count();
    shared as f32 / (output.len() - 2) as f32
}

fn longest_common_token_run(output: &[&str], reference: &[&str]) -> usize {
    if output.is_empty() || reference.is_empty() {
        return 0;
    }
    let mut previous = vec![0usize; reference.len() + 1];
    let mut current = vec![0usize; reference.len() + 1];
    let mut longest = 0usize;

    for left in output {
        current.fill(0);
        for (index, right) in reference.iter().enumerate() {
            if left == right {
                current[index + 1] = previous[index] + 1;
                longest = longest.max(current[index + 1]);
            }
        }
        std::mem::swap(&mut previous, &mut current);
    }
    longest
}

fn continues_reference(output: &[&str], reference: &[&str]) -> bool {
    if output.len() < 4 {
        return false;
    }
    (4..=8).rev().any(|size| {
        reference.len() >= size
            && output.len() >= size
            && reference[reference.len() - size..] == output[..size]
    })
}

fn sentence_fragments(text: &str) -> Vec<&str> {
    text.split(['.', '!', '?', '\n'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_user_speech_is_a_strong_leak() {
        let refs = vec!["Vou te mandar uma pergunta relacionada a area de tecnologia".into()];
        let result = ContextLeakGuard::new(&refs).assess(&refs[0]);
        assert!(result.strong_leak);
        assert_eq!(result.score, 1.0);
    }

    #[test]
    fn multiple_copied_context_sentences_are_rejected() {
        let refs = vec![
            "Nao sei mais o que vou fazer. Vou te mandar uma pergunta relacionada a tecnologia."
                .into(),
        ];
        let result = ContextLeakGuard::new(&refs).assess(&refs[0]);
        assert!(result.strong_leak);
    }

    #[test]
    fn shared_technical_terms_do_not_make_an_answer_a_leak() {
        let refs =
            vec!["Como garantir consistencia estrita em duas zonas de disponibilidade?".into()];
        let answer = "Eu usaria quorum de tres replicas, com um lider sincronizando commits.";
        assert!(!ContextLeakGuard::new(&refs).assess(answer).strong_leak);
    }

    #[test]
    fn copied_sequence_is_detected_without_character_quadratic_work() {
        let refs = vec![
            "Primeiro eu separo o dominio depois valido os invariantes e por fim persisto".into(),
        ];
        let answer = "Eu separo o dominio depois valido os invariantes e por fim persisto";
        assert!(ContextLeakGuard::new(&refs).assess(answer).strong_leak);
    }
}
