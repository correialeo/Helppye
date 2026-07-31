//! Correção contextual **opcional** — a extensão prevista depois da normalização
//! determinística, e deliberadamente **não** ligada ao caminho crítico nesta entrega.
//!
//! A tentação óbvia é mandar cada transcrição para um LLM "consertar" antes de ir para a
//! timeline. Isso custaria uma chamada de modelo inteira entre o fim da fala e o início da
//! geração da resposta — somada exatamente na métrica que o produto otimiza (silêncio →
//! resposta visível) e paga em **toda** fala, inclusive nas que resultariam em `[SKIP]`.
//! Pior: o corretor teria licença para reescrever a pergunta, e a resposta passaria a ser
//! sobre o texto reescrito.
//!
//! Por isso o modo default é `DeterministicOnly`, e `Contextual` só faz efeito quando um
//! `ContextualCorrector` estiver registrado — coisa que nenhuma implementação faz hoje. Sem
//! corretor registrado, `Contextual` se comporta como `DeterministicOnly` (com aviso), em
//! vez de bloquear a transcrição esperando algo que não existe.

use serde::{Deserialize, Serialize};

use crate::normalization::TranscriptNormalizationResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptCorrectionMode {
    /// Nenhuma normalização: a timeline recebe o texto exatamente como o provider entregou.
    Disabled,
    /// Só o normalizador determinístico. **Default.**
    #[default]
    DeterministicOnly,
    /// Determinístico seguido de correção contextual, quando houver corretor registrado.
    Contextual,
}

impl TranscriptCorrectionMode {
    pub fn applies_deterministic(self) -> bool {
        matches!(
            self,
            TranscriptCorrectionMode::DeterministicOnly | TranscriptCorrectionMode::Contextual
        )
    }

    pub fn applies_contextual(self) -> bool {
        matches!(self, TranscriptCorrectionMode::Contextual)
    }
}

#[derive(Debug, Clone)]
pub struct ContextualCorrectionInput {
    /// Saída do normalizador determinístico — o corretor trabalha sobre ela, nunca sobre o
    /// bruto, para não desfazer o que já está resolvido de forma barata.
    pub deterministic: TranscriptNormalizationResult,
    /// Falas recentes da mesma sessão, como contexto. Nunca de outra sessão.
    pub recent_context: Vec<String>,
}

/// Contrato do corretor contextual. Assíncrono porque qualquer implementação plausível
/// envolve I/O; por isso mesmo, quem o chamar precisa fazê-lo **fora** do caminho que
/// bloqueia a entrada do segmento na timeline.
#[async_trait::async_trait]
pub trait ContextualCorrector: Send + Sync {
    async fn correct(&self, input: ContextualCorrectionInput) -> TranscriptNormalizationResult;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_mode_is_deterministic_only() {
        assert_eq!(
            TranscriptCorrectionMode::default(),
            TranscriptCorrectionMode::DeterministicOnly
        );
    }

    #[test]
    fn disabled_applies_nothing() {
        let mode = TranscriptCorrectionMode::Disabled;
        assert!(!mode.applies_deterministic());
        assert!(!mode.applies_contextual());
    }

    #[test]
    fn contextual_implies_deterministic_first() {
        let mode = TranscriptCorrectionMode::Contextual;
        assert!(mode.applies_deterministic());
        assert!(mode.applies_contextual());
    }
}
