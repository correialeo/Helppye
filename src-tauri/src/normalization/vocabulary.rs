//! Vocabulário técnico configurável usado pela normalização determinística.
//!
//! Cada entrada é `canonical` + `aliases`: a forma correta e as formas que o transcritor
//! costuma produzir no lugar dela. O casamento é por **palavra inteira** (ou sequência
//! inteira de palavras), insensível a maiúsculas e a acentos — `"monolito"` casa
//! `"monólito"`, `"Micro Serviços"` casa `"microserviços"` — e nunca dentro de outra
//! palavra: sem isso, `"ddd"` casaria dentro de `"adddendum"` e a correção viraria
//! corrupção.
//!
//! A lista de sementes é curta de propósito. Ela cobre os termos que aparecem de fato nas
//! conversas-alvo e cujo erro de transcrição muda o que o modelo entende. Ampliar isto sem
//! critério é o caminho mais rápido para a camada começar a alterar sentido — ver
//! `docs/transcript-normalization.md`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VocabularyEntry {
    pub canonical: String,
    #[serde(default)]
    pub aliases: Vec<String>,
}

impl VocabularyEntry {
    pub fn new(canonical: impl Into<String>, aliases: &[&str]) -> Self {
        VocabularyEntry {
            canonical: canonical.into(),
            aliases: aliases.iter().map(|a| (*a).to_string()).collect(),
        }
    }
}

/// Sementes. Só termos técnicos, nomes de produto e siglas — nada de palavra comum, cuja
/// "correção" mudaria o que a pessoa disse.
fn seed_entries() -> Vec<VocabularyEntry> {
    vec![
        VocabularyEntry::new("DDD", &["ddd", "d d d", "de de de", "domain driven design"]),
        VocabularyEntry::new("SOLID", &["solid"]),
        VocabularyEntry::new("Docker", &["docker", "doker"]),
        VocabularyEntry::new("Kubernetes", &["kubernetes", "kubernets", "kubernetis"]),
        VocabularyEntry::new(
            "microserviços",
            &["micro serviços", "micro-serviços", "microservicos"],
        ),
        VocabularyEntry::new(
            "microservices",
            &["micro service", "micro services", "micro-services"],
        ),
        VocabularyEntry::new("monólito", &["monolito", "mono lito"]),
        VocabularyEntry::new("Entity Framework", &["entity framework", "entity framwork"]),
        VocabularyEntry::new("RabbitMQ", &["rabbitmq", "rabbit mq", "rabbit m q"]),
        VocabularyEntry::new("Bling", &["bling"]),
        VocabularyEntry::new("Stripe", &["stripe"]),
    ]
}

/// Índice pronto para casamento: cada alias (e o próprio canônico) vira uma sequência de
/// tokens dobrados (minúsculas, sem acento), apontando para a forma canônica.
#[derive(Debug, Clone)]
pub struct TranscriptionVocabulary {
    entries: Vec<VocabularyEntry>,
    /// `(tokens dobrados do alias, forma canônica)`, ordenado do alias mais longo para o
    /// mais curto: casar primeiro a sequência maior evita que `"micro"` sozinho consuma o
    /// começo de `"micro serviços"`.
    index: Vec<(Vec<String>, String)>,
}

impl Default for TranscriptionVocabulary {
    fn default() -> Self {
        TranscriptionVocabulary::from_entries(seed_entries())
    }
}

impl TranscriptionVocabulary {
    pub fn empty() -> Self {
        TranscriptionVocabulary {
            entries: Vec::new(),
            index: Vec::new(),
        }
    }

    pub fn from_entries(entries: Vec<VocabularyEntry>) -> Self {
        let mut index: Vec<(Vec<String>, String)> = Vec::new();
        for entry in &entries {
            let mut forms: Vec<&str> = vec![entry.canonical.as_str()];
            forms.extend(entry.aliases.iter().map(String::as_str));
            for form in forms {
                let tokens = fold_tokens(form);
                if tokens.is_empty() {
                    continue;
                }
                index.push((tokens, entry.canonical.clone()));
            }
        }
        // Mais longo primeiro; empate resolvido por ordem estável para o resultado ser
        // reproduzível entre execuções.
        index.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        TranscriptionVocabulary { entries, index }
    }

    /// Acrescenta um termo definido pelo usuário. Reconstrói o índice: a lista é pequena
    /// (dezenas de entradas) e a alternativa — manter o índice parcialmente ordenado —
    /// abriria a porta para o alias mais longo deixar de vencer.
    pub fn add_entry(&mut self, entry: VocabularyEntry) {
        let mut entries = std::mem::take(&mut self.entries);
        entries.retain(|e| !e.canonical.eq_ignore_ascii_case(&entry.canonical));
        entries.push(entry);
        *self = TranscriptionVocabulary::from_entries(entries);
    }

    pub fn entries(&self) -> &[VocabularyEntry] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Procura, começando em `tokens[start]`, o alias mais longo que casa. Devolve
    /// `(quantidade de tokens consumidos, forma canônica)`.
    pub fn match_at(&self, tokens: &[String], start: usize) -> Option<(usize, &str)> {
        for (alias_tokens, canonical) in &self.index {
            let len = alias_tokens.len();
            if start + len > tokens.len() {
                continue;
            }
            if tokens[start..start + len] == alias_tokens[..] {
                return Some((len, canonical.as_str()));
            }
        }
        None
    }
}

/// Minúsculas + remoção de acento, para casar `"monólito"` com `"monolito"` sem depender de
/// uma crate de normalização Unicode. Cobre o conjunto que aparece em português; qualquer
/// caractere fora dele passa inalterado, o que no pior caso significa não casar um alias —
/// nunca corromper texto.
pub fn fold_char(c: char) -> char {
    match c {
        'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
        'ú' | 'ù' | 'û' | 'ü' => 'u',
        'ç' => 'c',
        'ñ' => 'n',
        _ => c,
    }
}

pub fn fold_word(word: &str) -> String {
    word.chars()
        .flat_map(|c| c.to_lowercase())
        .map(fold_char)
        .collect()
}

fn fold_tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(fold_word)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_alias_wins() {
        let vocabulary = TranscriptionVocabulary::default();
        let tokens = fold_tokens("usei micro serviços aqui");
        let (consumed, canonical) = vocabulary.match_at(&tokens, 1).unwrap();
        assert_eq!(consumed, 2);
        assert_eq!(canonical, "microserviços");
    }

    #[test]
    fn accent_folding_matches_the_unaccented_transcription() {
        let vocabulary = TranscriptionVocabulary::default();
        let tokens = fold_tokens("um monolito grande");
        let (consumed, canonical) = vocabulary.match_at(&tokens, 1).unwrap();
        assert_eq!(consumed, 1);
        assert_eq!(canonical, "monólito");
    }

    #[test]
    fn user_entries_replace_a_previous_entry_with_the_same_canonical() {
        let mut vocabulary = TranscriptionVocabulary::empty();
        vocabulary.add_entry(VocabularyEntry::new("Helppye", &["help pie"]));
        vocabulary.add_entry(VocabularyEntry::new("Helppye", &["help pie", "helpai"]));
        assert_eq!(vocabulary.entries().len(), 1);
        let tokens = fold_tokens("o helpai roda local");
        let (_, canonical) = vocabulary.match_at(&tokens, 1).unwrap();
        assert_eq!(canonical, "Helppye");
    }

    #[test]
    fn empty_vocabulary_never_matches() {
        let vocabulary = TranscriptionVocabulary::empty();
        assert!(vocabulary.is_empty());
        let tokens = fold_tokens("docker e kubernetes");
        assert!(vocabulary.match_at(&tokens, 0).is_none());
    }
}
