use serde::Serialize;

use super::context_leak_guard::{normalize, ContextLeakGuard};
use super::skip_detector::SKIP_MARKER;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionValidationFailure {
    Empty,
    PunctuationOnly,
    TooShort,
    EchoOfQuestion,
    ContextLeak,
    AssistantVoice,
    InvalidSkip,
}

impl SuggestionValidationFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::PunctuationOnly => "punctuation_only",
            Self::TooShort => "too_short",
            Self::EchoOfQuestion => "echo_of_question",
            Self::ContextLeak => "context_leak",
            Self::AssistantVoice => "assistant_voice",
            Self::InvalidSkip => "invalid_skip",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidatedSuggestion {
    Skip,
    Text(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct SuggestionValidation {
    pub result: Result<ValidatedSuggestion, SuggestionValidationFailure>,
    pub context_leak_score: f32,
}

pub fn validate_suggestion(
    output: &str,
    current_question: &str,
    leak_references: &[String],
) -> SuggestionValidation {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return failure(SuggestionValidationFailure::Empty, 0.0);
    }
    if trimmed.eq_ignore_ascii_case(SKIP_MARKER) {
        return success(ValidatedSuggestion::Skip, 0.0);
    }
    if trimmed.to_ascii_uppercase().contains(SKIP_MARKER) {
        return failure(SuggestionValidationFailure::InvalidSkip, 0.0);
    }
    if trimmed.chars().all(|ch| !ch.is_alphanumeric()) {
        return failure(SuggestionValidationFailure::PunctuationOnly, 0.0);
    }

    let normalized = normalize(trimmed);
    let generic = [
        "sim", "nao", "ok", "okay", "certo", "talvez", "depende", "tchau", "ate logo", "obrigado",
        "valeu",
    ];
    let alphanumeric = normalized
        .chars()
        .filter(|character| character.is_alphanumeric())
        .count();
    if alphanumeric < 3 || generic.contains(&normalized.as_str()) {
        return failure(SuggestionValidationFailure::TooShort, 0.0);
    }

    let assistant_voice = [
        "como assistente",
        "posso te ajudar",
        "se quiser posso",
        "posso explicar",
        "posso mostrar",
    ];
    if assistant_voice
        .iter()
        .any(|phrase| normalized.contains(phrase))
    {
        return failure(SuggestionValidationFailure::AssistantVoice, 0.0);
    }

    let question_refs = vec![current_question.to_string()];
    let question_echo = ContextLeakGuard::new(&question_refs).assess(trimmed);
    if normalize(current_question) == normalized || question_echo.strong_leak {
        return failure(
            SuggestionValidationFailure::EchoOfQuestion,
            question_echo.score,
        );
    }

    let leak = ContextLeakGuard::new(leak_references).assess(trimmed);
    if leak.strong_leak {
        return failure(SuggestionValidationFailure::ContextLeak, leak.score);
    }

    success(ValidatedSuggestion::Text(trimmed.to_string()), leak.score)
}

fn failure(reason: SuggestionValidationFailure, score: f32) -> SuggestionValidation {
    SuggestionValidation {
        result: Err(reason),
        context_leak_score: score,
    }
}

fn success(value: ValidatedSuggestion, score: f32) -> SuggestionValidation {
    SuggestionValidation {
        result: Ok(value),
        context_leak_score: score,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validate(output: &str) -> Result<ValidatedSuggestion, SuggestionValidationFailure> {
        validate_suggestion(
            output,
            "Qual metal puro tem maior condutividade eletrica?",
            &["Vou te mandar uma pergunta relacionada a tecnologia".to_string()],
        )
        .result
    }

    #[test]
    fn punctuation_and_empty_are_rejected() {
        assert_eq!(
            validate("."),
            Err(SuggestionValidationFailure::PunctuationOnly)
        );
        assert_eq!(
            validate("?"),
            Err(SuggestionValidationFailure::PunctuationOnly)
        );
        assert_eq!(validate("   "), Err(SuggestionValidationFailure::Empty));
    }

    #[test]
    fn farewell_and_question_echo_are_rejected() {
        assert_eq!(
            validate("Tchau!"),
            Err(SuggestionValidationFailure::TooShort)
        );
        assert_eq!(
            validate("Qual metal puro tem maior condutividade eletrica?"),
            Err(SuggestionValidationFailure::EchoOfQuestion)
        );
    }

    #[test]
    fn copied_user_speech_is_rejected() {
        assert_eq!(
            validate("Vou te mandar uma pergunta relacionada a tecnologia"),
            Err(SuggestionValidationFailure::ContextLeak)
        );
    }

    #[test]
    fn short_technical_answer_and_skip_are_valid() {
        assert!(matches!(
            validate("A prata."),
            Ok(ValidatedSuggestion::Text(_))
        ));
        assert_eq!(validate("  [SKIP]\n"), Ok(ValidatedSuggestion::Skip));
        assert_eq!(validate(""), Err(SuggestionValidationFailure::Empty));
    }
}
