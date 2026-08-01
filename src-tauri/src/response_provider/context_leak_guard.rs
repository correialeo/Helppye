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

    pub fn assess(&self, output: &str) -> ContextLeakAssessment {
        let output = normalize(output);
        if output.is_empty() {
            return ContextLeakAssessment {
                score: 0.0,
                strong_leak: false,
            };
        }

        let output_tokens: Vec<&str> = output.split_whitespace().collect();
        let mut score = 0.0_f32;
        let mut reproduced_sentences = 0usize;

        for reference in self.references {
            let reference = normalize(reference);
            if reference.is_empty() {
                continue;
            }

            let lcs = longest_common_substring_chars(&output, &reference);
            let lcs_ratio = lcs as f32 / output.chars().count().max(1) as f32;
            let ngram_ratio = ngram_overlap(&output_tokens, &reference, 3);
            let token_ratio = token_overlap(&output_tokens, &reference);
            score = score.max(lcs_ratio.max(ngram_ratio).max(token_ratio * 0.9));

            if output.chars().count() >= MIN_LITERAL_CHARACTERS
                && (reference.contains(&output) || output.contains(&reference))
            {
                score = 1.0;
            }

            if continues_reference(&output_tokens, &reference) {
                score = score.max(0.95);
            }

            reproduced_sentences += sentence_fragments(&output)
                .into_iter()
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
                || output.chars().count() >= MIN_LITERAL_CHARACTERS);

        ContextLeakAssessment { score, strong_leak }
    }
}

pub(crate) fn normalize(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            out.extend(ch.to_lowercase());
        } else if !out.ends_with(' ') {
            out.push(' ');
        }
    }
    out.trim().to_string()
}

fn token_overlap(output_tokens: &[&str], reference: &str) -> f32 {
    if output_tokens.is_empty() {
        return 0.0;
    }
    let reference: HashSet<&str> = reference.split_whitespace().collect();
    let shared = output_tokens
        .iter()
        .filter(|token| reference.contains(**token))
        .count();
    shared as f32 / output_tokens.len() as f32
}

fn ngram_overlap(output_tokens: &[&str], reference: &str, size: usize) -> f32 {
    if output_tokens.len() < size {
        return 0.0;
    }
    let reference_tokens: Vec<&str> = reference.split_whitespace().collect();
    let reference_ngrams: HashSet<String> = reference_tokens
        .windows(size)
        .map(|window| window.join(" "))
        .collect();
    let output_ngrams: Vec<String> = output_tokens
        .windows(size)
        .map(|window| window.join(" "))
        .collect();
    let shared = output_ngrams
        .iter()
        .filter(|gram| reference_ngrams.contains(*gram))
        .count();
    shared as f32 / output_ngrams.len().max(1) as f32
}

fn longest_common_substring_chars(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().take(1_500).collect();
    let b: Vec<char> = b.chars().take(3_000).collect();
    let mut previous = vec![0usize; b.len() + 1];
    let mut longest = 0usize;
    for left in a {
        let mut current = vec![0usize; b.len() + 1];
        for (index, right) in b.iter().enumerate() {
            if left == *right {
                current[index + 1] = previous[index] + 1;
                longest = longest.max(current[index + 1]);
            }
        }
        previous = current;
    }
    longest
}

fn continues_reference(output: &[&str], reference: &str) -> bool {
    if output.len() < 4 {
        return false;
    }
    let reference: Vec<&str> = reference.split_whitespace().collect();
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
        let refs = vec!["Vou te mandar uma pergunta relacionada a area de tecnologia".to_string()];
        let result = ContextLeakGuard::new(&refs).assess(&refs[0]);
        assert!(result.strong_leak);
        assert_eq!(result.score, 1.0);
    }

    #[test]
    fn multiple_copied_context_sentences_are_rejected() {
        let refs = vec![
            "Nao sei mais o que vou fazer a pergunta. Vou te mandar uma pergunta relacionada a tecnologia."
                .to_string(),
        ];
        let result = ContextLeakGuard::new(&refs).assess(&refs[0]);
        assert!(result.strong_leak);
    }

    #[test]
    fn shared_technical_terms_do_not_make_an_answer_a_leak() {
        let refs = vec![
            "Como garantir consistencia estrita em duas zonas de disponibilidade?".to_string(),
        ];
        let answer = "Eu usaria quorum de tres replicas, com lider sincronizando commits entre zonas e failover testado.";
        assert!(!ContextLeakGuard::new(&refs).assess(answer).strong_leak);
    }
}
