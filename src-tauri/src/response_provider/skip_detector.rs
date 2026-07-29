//! Detecta, com o menor atraso possível, se a resposta em streaming do provedor é o
//! marcador fixo `[SKIP]` (a fala mais recente não merece resposta) ou conteúdo real a
//! ser exibido. Evita uma segunda chamada de classificação: o mesmo prompt que gera a
//! resposta também decide se deve responder.

pub const SKIP_MARKER: &str = "[SKIP]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipDecision {
    /// Ainda não há dados suficientes para decidir; nada deve ser exibido ainda.
    Pending,
    /// Não é um skip: `flush` é texto que já pode ser exibido ao usuário.
    NotSkip { flush: String },
    /// A resposta inteira deve ser suprimida.
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Skip,
    NotSkip,
}

#[derive(Debug, Default)]
pub struct SkipDetector {
    buffer: String,
    decision: Option<Decision>,
}

impl SkipDetector {
    pub fn new() -> Self {
        SkipDetector::default()
    }

    /// Alimenta mais um delta de texto do stream. Deve ser chamado para cada chunk
    /// recebido do provedor, na ordem em que chegam.
    pub fn push(&mut self, delta: &str) -> SkipDecision {
        match self.decision {
            Some(Decision::Skip) => return SkipDecision::Pending,
            Some(Decision::NotSkip) => {
                return SkipDecision::NotSkip {
                    flush: delta.to_string(),
                }
            }
            None => {}
        }

        self.buffer.push_str(delta);

        if self.buffer == SKIP_MARKER {
            self.decision = Some(Decision::Skip);
            return SkipDecision::Skip;
        }

        if self.buffer.len() < SKIP_MARKER.len() && SKIP_MARKER.starts_with(self.buffer.as_str()) {
            return SkipDecision::Pending;
        }

        self.decision = Some(Decision::NotSkip);
        let flush = std::mem::take(&mut self.buffer);
        SkipDecision::NotSkip { flush }
    }

    /// Chamado quando o stream do provedor termina. Resolve qualquer decisão ainda
    /// pendente (ex.: resposta vazia, ou mais curta que o marcador).
    pub fn finish(&mut self) -> SkipDecision {
        match self.decision {
            Some(Decision::Skip) => SkipDecision::Skip,
            Some(Decision::NotSkip) => SkipDecision::NotSkip {
                flush: String::new(),
            },
            None => {
                self.decision = Some(Decision::NotSkip);
                SkipDecision::NotSkip {
                    flush: std::mem::take(&mut self.buffer),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_matching_first_char_decides_immediately() {
        let mut detector = SkipDetector::new();
        assert_eq!(
            detector.push("Sure, "),
            SkipDecision::NotSkip {
                flush: "Sure, ".to_string()
            }
        );
    }

    #[test]
    fn exact_marker_in_one_chunk_is_skip() {
        let mut detector = SkipDetector::new();
        assert_eq!(detector.push("[SKIP]"), SkipDecision::Skip);
    }

    #[test]
    fn marker_split_across_chunks_is_skip() {
        let mut detector = SkipDetector::new();
        assert_eq!(detector.push("["), SkipDecision::Pending);
        assert_eq!(detector.push("SKI"), SkipDecision::Pending);
        assert_eq!(detector.push("P]"), SkipDecision::Skip);
    }

    #[test]
    fn divergence_mid_marker_flushes_accumulated_buffer() {
        let mut detector = SkipDetector::new();
        assert_eq!(detector.push("["), SkipDecision::Pending);
        assert_eq!(
            detector.push("nope"),
            SkipDecision::NotSkip {
                flush: "[nope".to_string()
            }
        );
    }

    #[test]
    fn content_after_decided_not_skip_flushes_straight_through() {
        let mut detector = SkipDetector::new();
        detector.push("Sure, ");
        assert_eq!(
            detector.push("let's continue."),
            SkipDecision::NotSkip {
                flush: "let's continue.".to_string()
            }
        );
    }

    #[test]
    fn content_after_decided_skip_is_swallowed() {
        let mut detector = SkipDetector::new();
        detector.push("[SKIP]");
        assert_eq!(detector.push(" extra"), SkipDecision::Pending);
    }

    #[test]
    fn empty_stream_finishes_as_not_skip_with_empty_flush() {
        let mut detector = SkipDetector::new();
        assert_eq!(
            detector.finish(),
            SkipDecision::NotSkip {
                flush: String::new()
            }
        );
    }

    #[test]
    fn stream_ending_mid_prefix_finishes_as_not_skip() {
        let mut detector = SkipDetector::new();
        detector.push("[SKI");
        assert_eq!(
            detector.finish(),
            SkipDecision::NotSkip {
                flush: "[SKI".to_string()
            }
        );
    }

    #[test]
    fn finish_after_skip_stays_skip() {
        let mut detector = SkipDetector::new();
        detector.push("[SKIP]");
        assert_eq!(detector.finish(), SkipDecision::Skip);
    }

    #[test]
    fn finish_after_not_skip_flushes_nothing_more() {
        let mut detector = SkipDetector::new();
        detector.push("Sure, ");
        assert_eq!(
            detector.finish(),
            SkipDecision::NotSkip {
                flush: String::new()
            }
        );
    }
}
