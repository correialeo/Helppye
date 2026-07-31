//! Normalização de transcrição: a camada entre o provider e a Conversation Timeline.
//!
//! Antes daqui, o único tratamento de texto era colapsar espaços (`normalize_segment_text`)
//! dentro da timeline. Isso deixava passar para o prompt exatamente os defeitos que o
//! transcritor produz e que mudam o que o modelo entende: `"micro serviços"` em vez de
//! `"microserviços"`, `"ddd"` em vez de `"DDD"`, `"rabbit mq"` em vez de `"RabbitMQ"`,
//! pontuação repetida, frase começando em minúscula.
//!
//! Duas regras definem o escopo desta camada e explicam o que ela **não** faz:
//!
//! 1. **Determinística e barata.** Nada aqui chama modelo, rede ou disco. Ela roda no
//!    caminho crítico entre o resultado final do provider e a timeline; qualquer coisa
//!    dependente de I/O aqui somaria latência exatamente onde a métrica de UX é medida
//!    (silêncio → resposta visível).
//! 2. **Não altera sentido.** O vocabulário é uma lista **fechada e configurável** de
//!    termos técnicos e nomes próprios conhecidos, casada por palavra inteira. Uma lista
//!    global agressiva ("corrigir tudo que parece errado") transformaria fala legítima em
//!    outra coisa — e a fala corrompida seria a única versão que a timeline veria.
//!
//! O texto original **nunca** é descartado: `TranscriptNormalizationResult` carrega
//! `raw_text`, `normalized_text` e a lista de mudanças aplicadas. Diagnóstico usa o bruto;
//! prompt usa o normalizado.

pub mod correction;
pub mod deterministic;
pub mod vocabulary;

use serde::{Deserialize, Serialize};

use crate::audio::types::AudioSource;
use crate::transcription::provider::TranscriptionProviderId;

pub use correction::{ContextualCorrectionInput, ContextualCorrector, TranscriptCorrectionMode};
pub use deterministic::DeterministicNormalizer;
pub use vocabulary::{TranscriptionVocabulary, VocabularyEntry};

#[derive(Debug, Clone)]
pub struct TranscriptNormalizationInput {
    pub raw_text: String,
    pub source: AudioSource,
    pub language: Option<String>,
    pub provider: TranscriptionProviderId,
}

/// Que tipo de mudança foi aplicada. Existe para tornar a normalização **auditável**: em
/// modo de desenvolvedor dá para ver exatamente o que a camada mexeu, e uma mudança
/// inesperada aparece como um item na lista em vez de como um texto misteriosamente
/// diferente.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NormalizationChangeKind {
    /// Espaços duplicados/espaço antes de pontuação.
    Whitespace,
    /// Pontuação repetida (`,,`, `!!!`).
    RepeatedPunctuation,
    /// Primeira letra da frase em maiúscula.
    Capitalization,
    /// Termo do vocabulário configurado, incluindo palavra que o transcritor partiu em
    /// duas (`"micro serviços"` → `"microserviços"`).
    Vocabulary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NormalizationChange {
    pub kind: NormalizationChangeKind,
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TranscriptNormalizationResult {
    pub raw_text: String,
    pub normalized_text: String,
    pub normalization_changes: Vec<NormalizationChange>,
}

impl TranscriptNormalizationResult {
    /// Passagem sem alteração — usada quando o modo é `Disabled`.
    pub fn unchanged(raw_text: String) -> Self {
        TranscriptNormalizationResult {
            normalized_text: raw_text.clone(),
            raw_text,
            normalization_changes: Vec::new(),
        }
    }

    pub fn change_count(&self) -> usize {
        self.normalization_changes.len()
    }
}

pub trait TranscriptNormalizer: Send + Sync {
    fn normalize(&self, input: TranscriptNormalizationInput) -> TranscriptNormalizationResult;
}
