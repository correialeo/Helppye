//! Normalizador determinístico: espaços, pontuação repetida, capitalização de frase e
//! vocabulário técnico configurável. Sem rede, sem modelo, sem estado — a mesma entrada
//! produz sempre a mesma saída, o que é o que permite testar a camada de verdade.
//!
//! A ordem das etapas importa. Espaços primeiro (para a pontuação ficar adjacente ao que
//! ela pontua), pontuação depois, vocabulário em seguida (casando sobre um texto já
//! regular) e capitalização por último — capitalizar antes do vocabulário faria a primeira
//! palavra virar `"Ddd"` e deixar de casar com o alias.

use crate::normalization::vocabulary::{fold_word, TranscriptionVocabulary};
use crate::normalization::{
    NormalizationChange, NormalizationChangeKind, TranscriptNormalizationInput,
    TranscriptNormalizationResult, TranscriptNormalizer,
};

/// Pontuação cuja repetição é sempre ruído de transcritor. `.` fica de fora desta lista e
/// tem tratamento próprio: `...` é reticência legítima, não erro.
const COLLAPSIBLE_PUNCTUATION: &[char] = &[',', ';', ':', '!', '?'];

/// Pontuação que não pode ser precedida de espaço.
const NO_SPACE_BEFORE: &[char] = &[',', '.', ';', ':', '!', '?', ')', ']', '%'];

#[derive(Default)]
pub struct DeterministicNormalizer {
    vocabulary: TranscriptionVocabulary,
}

impl DeterministicNormalizer {
    pub fn new(vocabulary: TranscriptionVocabulary) -> Self {
        DeterministicNormalizer { vocabulary }
    }

    pub fn vocabulary(&self) -> &TranscriptionVocabulary {
        &self.vocabulary
    }
}

impl TranscriptNormalizer for DeterministicNormalizer {
    fn normalize(&self, input: TranscriptNormalizationInput) -> TranscriptNormalizationResult {
        let raw_text = input.raw_text;
        let mut changes = Vec::new();

        let mut text = collapse_whitespace(&raw_text, &mut changes);
        text = collapse_repeated_punctuation(&text, &mut changes);
        text = apply_vocabulary(&text, &self.vocabulary, &mut changes);
        text = capitalize_sentences(&text, &mut changes);

        if !changes.is_empty() {
            // Sem a origem do texto, uma normalização suspeita ("por que 'micro' virou
            // 'microserviços' aqui?") é impossível de atribuir: a mesma frase vinda do
            // microfone e da saída de sistema pode ter passado por providers e idiomas
            // diferentes. Nível `trace` porque `before`/`after` são conteúdo da reunião.
            tracing::trace!(
                source = ?input.source,
                language = ?input.language,
                provider = %input.provider,
                change_count = changes.len(),
                "transcrição normalizada"
            );
        }

        TranscriptNormalizationResult {
            raw_text,
            normalized_text: text,
            normalization_changes: changes,
        }
    }
}

fn collapse_whitespace(text: &str, changes: &mut Vec<NormalizationChange>) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");

    // Remove o espaço que o transcritor às vezes deixa antes da pontuação ("assim , sim").
    let mut out = String::with_capacity(joined.len());
    for ch in joined.chars() {
        if NO_SPACE_BEFORE.contains(&ch) && out.ends_with(' ') {
            out.pop();
        }
        out.push(ch);
    }

    if out != text {
        changes.push(NormalizationChange {
            kind: NormalizationChangeKind::Whitespace,
            before: text.to_string(),
            after: out.clone(),
        });
    }
    out
}

fn collapse_repeated_punctuation(text: &str, changes: &mut Vec<NormalizationChange>) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;

    while i < chars.len() {
        let ch = chars[i];
        let mut run = 1;
        while i + run < chars.len() && chars[i + run] == ch {
            run += 1;
        }

        if run > 1 && COLLAPSIBLE_PUNCTUATION.contains(&ch) {
            let before: String = std::iter::repeat_n(ch, run).collect();
            out.push(ch);
            changes.push(NormalizationChange {
                kind: NormalizationChangeKind::RepeatedPunctuation,
                before,
                after: ch.to_string(),
            });
        } else if run > 1 && ch == '.' {
            // `...` é reticência e fica; `..` ou `....` é ruído e vira reticência.
            if run != 3 {
                let before: String = std::iter::repeat_n('.', run).collect();
                changes.push(NormalizationChange {
                    kind: NormalizationChangeKind::RepeatedPunctuation,
                    before,
                    after: "...".into(),
                });
            }
            out.push_str("...");
        } else {
            for _ in 0..run {
                out.push(ch);
            }
        }
        i += run;
    }
    out
}

enum Item {
    Word(String),
    Separator(String),
}

fn split_items(text: &str) -> Vec<Item> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut current_is_word: Option<bool> = None;

    for ch in text.chars() {
        let is_word = ch.is_alphanumeric();
        match current_is_word {
            Some(previous) if previous == is_word => current.push(ch),
            Some(previous) => {
                items.push(if previous {
                    Item::Word(std::mem::take(&mut current))
                } else {
                    Item::Separator(std::mem::take(&mut current))
                });
                current.push(ch);
                current_is_word = Some(is_word);
            }
            None => {
                current.push(ch);
                current_is_word = Some(is_word);
            }
        }
    }
    if let Some(is_word) = current_is_word {
        items.push(if is_word {
            Item::Word(current)
        } else {
            Item::Separator(current)
        });
    }
    items
}

/// Separadores que podem existir *dentro* de um termo do vocabulário. `"micro serviços"` e
/// `"micro-serviços"` são o mesmo termo partido; `"micro. Serviços"` não é — um ponto final
/// no meio significa que as duas palavras estão em frases diferentes, e juntá-las mudaria a
/// fala.
fn separator_joins_a_term(separator: &str) -> bool {
    !separator.is_empty()
        && separator
            .chars()
            .all(|c| c.is_whitespace() || c == '-' || c == '_')
}

fn apply_vocabulary(
    text: &str,
    vocabulary: &TranscriptionVocabulary,
    changes: &mut Vec<NormalizationChange>,
) -> String {
    if vocabulary.is_empty() {
        return text.to_string();
    }

    let items = split_items(text);
    let word_positions: Vec<usize> = items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| matches!(item, Item::Word(_)).then_some(i))
        .collect();
    let folded: Vec<String> = word_positions
        .iter()
        .map(|&i| match &items[i] {
            Item::Word(w) => fold_word(w),
            Item::Separator(_) => unreachable!("posição de palavra aponta para separador"),
        })
        .collect();

    // `None` = item mantido como está; `Some(s)` = item substituído por `s` (possivelmente
    // vazio, quando o item foi absorvido por um termo de várias palavras).
    let mut replacement: Vec<Option<String>> = vec![None; items.len()];

    let mut w = 0;
    while w < folded.len() {
        let Some((consumed, canonical)) = vocabulary.match_at(&folded, w) else {
            w += 1;
            continue;
        };

        let first_item = word_positions[w];
        let last_item = word_positions[w + consumed - 1];

        // Todos os separadores internos precisam ser "juntáveis", senão não é o mesmo termo.
        let internal_ok = (first_item + 1..last_item).all(|i| match &items[i] {
            Item::Separator(s) => separator_joins_a_term(s),
            Item::Word(_) => true,
        });
        if !internal_ok {
            w += 1;
            continue;
        }

        let before: String = (first_item..=last_item)
            .map(|i| match &items[i] {
                Item::Word(s) | Item::Separator(s) => s.as_str(),
            })
            .collect();

        if before != canonical {
            changes.push(NormalizationChange {
                kind: NormalizationChangeKind::Vocabulary,
                before,
                after: canonical.to_string(),
            });
        }

        replacement[first_item] = Some(canonical.to_string());
        for slot in replacement
            .iter_mut()
            .take(last_item + 1)
            .skip(first_item + 1)
        {
            *slot = Some(String::new());
        }
        w += consumed;
    }

    let mut out = String::with_capacity(text.len());
    for (i, item) in items.iter().enumerate() {
        match &replacement[i] {
            Some(value) => out.push_str(value),
            None => match item {
                Item::Word(s) | Item::Separator(s) => out.push_str(s),
            },
        }
    }
    out
}

fn capitalize_sentences(text: &str, changes: &mut Vec<NormalizationChange>) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut capitalize_next = true;

    for (index, &ch) in chars.iter().enumerate() {
        if capitalize_next && ch.is_alphabetic() {
            // `to_uppercase` pode render mais de um char (ß → SS); nenhum caso disso em
            // português, mas o iterador é o contrato correto da API.
            for upper in ch.to_uppercase() {
                out.push(upper);
            }
            capitalize_next = false;
            continue;
        }
        // Reticências não terminam frase — marcam fala que se interrompe e continua
        // ("Bom... enfim"). Tratá-las como ponto final capitalizava a palavra seguinte e
        // inventava uma fronteira de frase que o falante não fez. `...` sobrevive ao passo
        // de pontuação repetida justamente por isso, então este passo precisa reconhecê-lo.
        let part_of_ellipsis = ch == '.'
            && (chars.get(index + 1) == Some(&'.') || (index > 0 && chars[index - 1] == '.'));
        if matches!(ch, '.' | '!' | '?') && !part_of_ellipsis {
            capitalize_next = true;
        }
        out.push(ch);
    }

    if out != text {
        changes.push(NormalizationChange {
            kind: NormalizationChangeKind::Capitalization,
            before: text.to_string(),
            after: out.clone(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::types::AudioSource;
    use crate::normalization::vocabulary::VocabularyEntry;
    use crate::transcription::provider::TranscriptionProviderId;

    fn normalize(text: &str) -> TranscriptNormalizationResult {
        DeterministicNormalizer::default().normalize(TranscriptNormalizationInput {
            raw_text: text.to_string(),
            source: AudioSource::SystemOutput,
            language: Some("pt".into()),
            provider: TranscriptionProviderId::WhisperLocal,
        })
    }

    #[test]
    fn duplicate_spaces_collapse() {
        let result = normalize("olá    mundo  aqui");
        assert_eq!(result.normalized_text, "Olá mundo aqui");
        assert!(result
            .normalization_changes
            .iter()
            .any(|c| c.kind == NormalizationChangeKind::Whitespace));
    }

    #[test]
    fn space_before_punctuation_is_removed() {
        let result = normalize("então , sim .");
        assert_eq!(result.normalized_text, "Então, sim.");
    }

    #[test]
    fn repeated_punctuation_collapses_but_ellipsis_survives() {
        assert_eq!(normalize("o quê!!!").normalized_text, "O quê!");
        assert_eq!(normalize("espera,, calma").normalized_text, "Espera, calma");
        assert_eq!(normalize("bom... enfim").normalized_text, "Bom... enfim");
        assert_eq!(normalize("bom.... enfim").normalized_text, "Bom... enfim");
    }

    #[test]
    fn first_letter_of_each_sentence_is_capitalized() {
        let result = normalize("isso funciona. mas tem um porém. sério?");
        assert_eq!(
            result.normalized_text,
            "Isso funciona. Mas tem um porém. Sério?"
        );
        assert!(result
            .normalization_changes
            .iter()
            .any(|c| c.kind == NormalizationChangeKind::Capitalization));
    }

    #[test]
    fn provider_fragmented_term_is_rejoined() {
        let result = normalize("a gente usa micro serviços hoje");
        assert_eq!(result.normalized_text, "A gente usa microserviços hoje");
        let change = result
            .normalization_changes
            .iter()
            .find(|c| c.kind == NormalizationChangeKind::Vocabulary)
            .expect("mudança de vocabulário registrada");
        assert_eq!(change.before, "micro serviços");
        assert_eq!(change.after, "microserviços");
    }

    #[test]
    fn acronyms_and_product_names_get_their_canonical_form() {
        assert_eq!(
            normalize("estudei ddd e solid com docker").normalized_text,
            "Estudei DDD e SOLID com Docker"
        );
        assert_eq!(
            normalize("usamos rabbit mq e stripe").normalized_text,
            "Usamos RabbitMQ e Stripe"
        );
        assert_eq!(
            normalize("subimos no kubernetes").normalized_text,
            "Subimos no Kubernetes"
        );
        assert_eq!(
            normalize("migramos do monolito").normalized_text,
            "Migramos do monólito"
        );
        assert_eq!(
            normalize("o entity framework gera isso").normalized_text,
            "O Entity Framework gera isso"
        );
        assert_eq!(
            normalize("integrar com o bling").normalized_text,
            "Integrar com o Bling"
        );
    }

    #[test]
    fn vocabulary_never_matches_inside_a_larger_word() {
        // "ddd" dentro de outra palavra não é a sigla; casar aqui corromperia a fala.
        let result = normalize("adddendum não é sigla");
        assert_eq!(result.normalized_text, "Adddendum não é sigla");
        assert!(!result
            .normalization_changes
            .iter()
            .any(|c| c.kind == NormalizationChangeKind::Vocabulary));
    }

    #[test]
    fn a_sentence_boundary_prevents_joining_two_words_into_one_term() {
        let result = normalize("falei de micro. Serviços vieram depois");
        assert!(
            result.normalized_text.contains("micro. Serviços"),
            "{}",
            result.normalized_text
        );
    }

    #[test]
    fn raw_text_is_always_preserved() {
        let raw = "estudei    ddd ,, muito!!!";
        let result = normalize(raw);
        assert_eq!(result.raw_text, raw);
        assert_ne!(result.normalized_text, raw);
    }

    #[test]
    fn text_without_defects_produces_no_changes() {
        let result = normalize("Isso já está correto.");
        assert_eq!(result.normalized_text, "Isso já está correto.");
        assert_eq!(result.change_count(), 0);
    }

    /// A normalização trabalha sobre texto em português, e cortar ou remontar por byte em
    /// vez de por `char` produziria acento partido ao meio — o resultado seria enviado ao
    /// modelo e mostrado ao usuário como texto corrompido. Emoji e travessão entram aqui
    /// como sentinelas multi-byte: se algum passo voltar a indexar por byte, quebram antes
    /// de qualquer acento.
    #[test]
    fn accents_cedillas_and_multibyte_characters_survive_intact() {
        let raw = "então , a inflação caiu — e a manutenção também ✅";
        let result = normalize(raw);
        assert_eq!(
            result.normalized_text,
            "Então, a inflação caiu — e a manutenção também ✅"
        );
        assert_eq!(result.raw_text, raw);
        for needle in ["inflação", "manutenção", "também", "—", "✅"] {
            assert!(result.normalized_text.contains(needle), "{needle}");
        }
    }

    /// Capitalização de frase precisa usar as regras Unicode, não `to_ascii_uppercase`:
    /// uma frase começada em "é", "ó" ou "ç" continuaria minúscula em ASCII.
    #[test]
    fn a_sentence_starting_with_an_accented_letter_is_capitalized_correctly() {
        let result = normalize("é isso. ótimo. çedilha inicial");
        assert_eq!(result.normalized_text, "É isso. Ótimo. Çedilha inicial");
    }

    #[test]
    fn a_user_added_term_is_applied_like_a_seeded_one() {
        let mut vocabulary = TranscriptionVocabulary::default();
        vocabulary.add_entry(VocabularyEntry::new("Helppye", &["help pie", "help pai"]));
        let normalizer = DeterministicNormalizer::new(vocabulary);
        let result = normalizer.normalize(TranscriptNormalizationInput {
            raw_text: "o help pie roda local".into(),
            source: AudioSource::Microphone,
            language: None,
            provider: TranscriptionProviderId::WhisperLocal,
        });
        assert_eq!(result.normalized_text, "O Helppye roda local");
    }

    #[test]
    fn empty_input_stays_empty() {
        let result = normalize("   ");
        assert_eq!(result.normalized_text, "");
    }
}
