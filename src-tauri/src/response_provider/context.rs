//! Snapshot imutavel e prompt da geracao de resposta.
//!
//! A timeline mutavel so e consultada enquanto `snapshot_generation_request` roda. A
//! partir dali, contexto, identidade e fala atual pertencem a uma
//! `ResponseGenerationRequest` owned. A task assincrona e o provider nunca voltam ao
//! historico da sessao.

use std::collections::HashSet;
use std::time::Instant;

use crate::conversation::{
    ConversationSpeaker, ConversationUtterance, SessionId, TurnId, UtteranceId,
};

use super::engine::GenerationId;
use super::provider::{ResponseMessage, ResponseRequest, ResponseRole};

pub const MAX_PREVIOUS_REMOTE_CONTEXT: usize = 2;
pub const MAX_PREVIOUS_USER_CONTEXT: usize = 1;
pub const MAX_CONTEXT_CHARACTERS: usize = 3_000;
const MAX_CURRENT_SPEECH_CHARACTERS: usize = 12_000;
const MAX_LEAK_REFERENCE_UTTERANCES: usize = 8;
const MAX_OUTPUT_TOKENS: u32 = 160;
const TEMPERATURE: f32 = 0.2;
const PREVIEW_LINE_CHARACTERS: usize = 120;
pub(crate) const CONTEXT_HEADER: &str = "[CONTEXTO ANTERIOR - SOMENTE REFERENCIA]";
pub(crate) const CURRENT_SPEECH_HEADER: &str = "[FALA ATUAL DA OUTRA PESSOA - RESPONDA A ISTO]";
pub(crate) const INSTRUCTION_HEADER: &str = "[SAIDA]";

const SYSTEM_PROMPT: &str = "[INSTRUCAO DO SISTEMA]\n\
Voce escreve exatamente a resposta que o usuario pode falar.\n\
Nao continue a transcricao.\n\
Nao repita falas anteriores.\n\
Nao copie o contexto.\n\
Nao responda como assistente.\n\
Nao explique o que esta fazendo.\n\
Responder e o padrao. Retorne [SKIP] somente quando a fala atual, considerada sozinha, \
for uma saudacao isolada, confirmacao isolada, ruido/fragmento sem sentido, um enunciado \
que ainda nao pede resposta, ou fala claramente dirigida a outra pessoa. Pontuacao da \
transcricao nao e confiavel. Retorne apenas a resposta falavel.";

const OUTPUT_INSTRUCTION: &str = "[SAIDA]\n\
Retorne apenas a resposta falavel. Use o contexto anterior somente para resolver uma \
referencia da fala atual. Nao reutilize frases do contexto.";

const REPAIR_SYSTEM_PROMPT: &str = "[INSTRUCAO DO SISTEMA]\n\
A resposta anterior foi invalida porque copiou a conversa, repetiu a pergunta ou nao \
produziu conteudo util.\n\
Responda diretamente a fala atual.\n\
Nao reutilize frases do contexto.\n\
Voce escreve exatamente a resposta que o usuario pode falar. Retorne somente a resposta \
falavel ou [SKIP] quando a fala atual, por si so, legitimamente nao pedir resposta.";

/// Snapshot completo criado no instante do trigger. Nenhum campo referencia a timeline.
#[derive(Debug, Clone)]
pub struct ResponseGenerationRequest {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub utterance_id: UtteranceId,
    pub utterance_revision: u64,
    pub generation_id: GenerationId,

    pub current_remote_utterance: String,
    pub previous_remote_context: Vec<String>,
    pub latest_user_answer: Option<String>,
    pub user_profile_context: Option<String>,

    pub created_at: Instant,
    pub speech_ended_at: Instant,
    pub transcription_completed_at: Instant,
    pub automatic: bool,

    /// Identidades exatas das utterances que entraram no bloco de contexto.
    pub context_utterance_ids: Vec<UtteranceId>,
    /// Textos que nao entram no prompt, mas precisam ser comparados com a saida para
    /// impedir copia de falas normalizadas, transcricao bruta e sugestao anterior.
    pub context_leak_references: Vec<String>,
    pub previous_suggestion: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResponseContext {
    pub request: ResponseRequest,
    pub context_utterance_ids: Vec<UtteranceId>,
    /// Nome mantido no evento existente; agora conta utterances, nao turnos inteiros.
    pub context_turn_count: usize,
    pub context_character_count: usize,
    pub sanitized_preview: String,
}

pub trait ResponseContextBuilder: Send + Sync {
    fn build(&self, snapshot: &ResponseGenerationRequest) -> ResponseContext;
    fn build_repair(&self, snapshot: &ResponseGenerationRequest) -> ResponseContext;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultResponseContextBuilder;

impl ResponseContextBuilder for DefaultResponseContextBuilder {
    fn build(&self, snapshot: &ResponseGenerationRequest) -> ResponseContext {
        build_prompt(snapshot, false)
    }

    fn build_repair(&self, snapshot: &ResponseGenerationRequest) -> ResponseContext {
        build_prompt(snapshot, true)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn snapshot_generation_request(
    session_id: SessionId,
    turn_id: TurnId,
    current: &ConversationUtterance,
    generation_id: GenerationId,
    history: &[ConversationUtterance],
    previous_suggestion: Option<String>,
    user_profile_context: Option<String>,
    created_at: Instant,
    speech_ended_at: Instant,
    automatic: bool,
) -> ResponseGenerationRequest {
    // A ordem causal primaria e a ordem de recebimento. O timestamp monotonico so
    // desempata/ordena os eventos que ja eram anteriores; nunca promovemos uma fala
    // posterior ao trigger para dentro do snapshot.
    let mut prior: Vec<&ConversationUtterance> = history
        .iter()
        .filter(|utterance| {
            utterance.id != current.id
                && utterance.finalized_at.is_some()
                && utterance.received_sequence < current.received_sequence
                && utterance.ended_at <= current.started_at
        })
        .collect();
    prior.sort_by_key(|utterance| {
        (
            utterance.ended_at,
            utterance.received_sequence,
            utterance.id.value(),
        )
    });

    let needs_context = needs_reference_context(&current.text);
    let selected_remote: Vec<&ConversationUtterance> = if needs_context {
        prior
            .iter()
            .rev()
            .copied()
            .filter(|utterance| utterance.speaker == ConversationSpeaker::OtherPerson)
            .take(MAX_PREVIOUS_REMOTE_CONTEXT)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    } else {
        Vec::new()
    };
    let selected_user = needs_context.then(|| {
        prior
            .iter()
            .rev()
            .copied()
            .filter(|utterance| utterance.speaker == ConversationSpeaker::User)
            .take(MAX_PREVIOUS_USER_CONTEXT)
            .next()
    });
    let selected_user = selected_user.flatten();

    let mut budget = MAX_CONTEXT_CHARACTERS.saturating_sub(current.text.trim().chars().count());
    let mut context_utterance_ids = Vec::new();

    let latest_user_answer = selected_user.and_then(|utterance| {
        let text = utterance.text.trim();
        let cost = "Usuario: ".chars().count() + text.chars().count();
        if text.is_empty() || cost > budget {
            None
        } else {
            budget -= cost;
            context_utterance_ids.push(utterance.id);
            Some(text.to_string())
        }
    });

    let mut previous_remote_context = Vec::new();
    // Contexto remoto antigo e removido primeiro quando o budget aperta. Iterar do mais
    // recente para o mais antigo preserva o material mais proximo da referencia.
    for utterance in selected_remote.into_iter().rev() {
        let text = utterance.text.trim();
        let cost = "Outra pessoa: ".chars().count() + text.chars().count();
        if text.is_empty() || cost > budget {
            continue;
        }
        budget -= cost;
        previous_remote_context.push(text.to_string());
        context_utterance_ids.push(utterance.id);
    }
    previous_remote_context.reverse();
    context_utterance_ids.reverse();

    let user_profile_context = user_profile_context.and_then(|profile| {
        let profile = profile.trim();
        let cost = "Perfil relevante: ".chars().count() + profile.chars().count();
        if profile.is_empty() || cost > budget {
            None
        } else {
            Some(profile.to_string())
        }
    });

    let mut leak_references = Vec::new();
    let mut seen = HashSet::new();
    for utterance in prior.iter().rev().take(MAX_LEAK_REFERENCE_UTTERANCES).rev() {
        for text in [&utterance.text, &utterance.raw_text] {
            let trimmed = text.trim();
            if !trimmed.is_empty() && seen.insert(trimmed.to_lowercase()) {
                leak_references.push(trimmed.to_string());
            }
        }
    }
    let current_raw = current.raw_text.trim();
    if !current_raw.is_empty()
        && current_raw != current.text.trim()
        && seen.insert(current_raw.to_lowercase())
    {
        leak_references.push(current_raw.to_string());
    }
    if let Some(previous) = previous_suggestion.as_deref() {
        let trimmed = previous.trim();
        if !trimmed.is_empty() && seen.insert(trimmed.to_lowercase()) {
            leak_references.push(trimmed.to_string());
        }
    }

    ResponseGenerationRequest {
        session_id,
        turn_id,
        utterance_id: current.id,
        utterance_revision: current.revision,
        generation_id,
        current_remote_utterance: current.text.trim().to_string(),
        previous_remote_context,
        latest_user_answer,
        user_profile_context,
        created_at,
        speech_ended_at,
        transcription_completed_at: current.transcription_completed_at,
        automatic,
        context_utterance_ids,
        context_leak_references: leak_references,
        previous_suggestion,
    }
}

fn needs_reference_context(text: &str) -> bool {
    // Isto nao detecta perguntas. So decide se pronomes demonstrativos exigem uma
    // antecedente; sem referencia explicita, o historico fica fora por seguranca.
    let folded = normalize_words(text);
    let tokens: HashSet<&str> = folded.split_whitespace().collect();
    [
        "isso", "isto", "aquilo", "esse", "essa", "esses", "essas", "desse", "dessa", "nesse",
        "nessa", "ele", "ela", "eles", "elas",
    ]
    .iter()
    .any(|token| tokens.contains(token))
}

fn build_prompt(snapshot: &ResponseGenerationRequest, repair: bool) -> ResponseContext {
    let current = truncate_defensively(
        snapshot.current_remote_utterance.trim(),
        MAX_CURRENT_SPEECH_CHARACTERS,
    );

    let (system, user_content, context_character_count, context_ids) = if repair {
        (
            REPAIR_SYSTEM_PROMPT,
            format!(
                "{CURRENT_SPEECH_HEADER}\n{current}\n\n{INSTRUCTION_HEADER}\nRetorne apenas a resposta falavel."
            ),
            0,
            Vec::new(),
        )
    } else {
        let mut lines = Vec::new();
        for text in &snapshot.previous_remote_context {
            lines.push(format!("Outra pessoa: {text}"));
        }
        if let Some(text) = &snapshot.latest_user_answer {
            lines.push(format!("Usuario: {text}"));
        }
        if let Some(text) = &snapshot.user_profile_context {
            lines.push(format!("Perfil relevante: {text}"));
        }
        let context = if lines.is_empty() {
            "(nenhum)".to_string()
        } else {
            lines.join("\n")
        };
        let count = context.chars().count();
        (
            SYSTEM_PROMPT,
            format!(
                "{CONTEXT_HEADER}\n{context}\n\n{CURRENT_SPEECH_HEADER}\n{current}\n\n{OUTPUT_INSTRUCTION}"
            ),
            count,
            snapshot.context_utterance_ids.clone(),
        )
    };

    let preview = user_content
        .lines()
        .map(|line| truncate_defensively(line, PREVIEW_LINE_CHARACTERS))
        .collect::<Vec<_>>()
        .join("\n");

    let context_turn_count = context_ids.len();
    ResponseContext {
        request: ResponseRequest {
            messages: vec![
                ResponseMessage {
                    role: ResponseRole::System,
                    content: system.to_string(),
                },
                ResponseMessage {
                    role: ResponseRole::User,
                    content: user_content,
                },
            ],
            max_output_tokens: MAX_OUTPUT_TOKENS,
            temperature: TEMPERATURE,
        },
        context_utterance_ids: context_ids,
        context_turn_count,
        context_character_count,
        sanitized_preview: preview,
    }
}

fn normalize_words(text: &str) -> String {
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

fn truncate_defensively(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        text.to_string()
    } else {
        text.chars().take(limit).collect()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use crate::audio::segment::{AudioTimestamp, SegmentId};
    use crate::audio::types::AudioSource;

    use super::*;

    fn utterance(
        id: u64,
        speaker: ConversationSpeaker,
        text: &str,
        start: u64,
        sequence: u64,
    ) -> ConversationUtterance {
        ConversationUtterance {
            capture_stream_id: crate::audio::types::CaptureStreamId::UNASSIGNED,
            id: UtteranceId::from_raw(id),
            speaker,
            source: match speaker {
                ConversationSpeaker::User => AudioSource::Microphone,
                ConversationSpeaker::OtherPerson => AudioSource::SystemOutput,
            },
            text: text.to_string(),
            raw_text: format!("raw {text}"),
            segments: vec![SegmentId::next()],
            received_sequence: sequence,
            started_at: AudioTimestamp(start),
            ended_at: AudioTimestamp(start + 100),
            finalized_at: Some(AudioTimestamp(start + 100)),
            revision: 1,
            transcription_completed_at: Instant::now(),
            speech_ended_at: Instant::now(),
        }
    }

    fn snapshot(
        history: &[ConversationUtterance],
        current: &ConversationUtterance,
    ) -> ResponseGenerationRequest {
        snapshot_generation_request(
            SessionId::from_value(1),
            TurnId::from_raw(9),
            current,
            GenerationId::from_raw(7),
            history,
            None,
            None,
            Instant::now(),
            Instant::now(),
            true,
        )
    }

    fn prompt(context: &ResponseContext) -> String {
        context
            .request
            .messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn current_speech_is_always_present_and_not_replaced_by_global_latest() {
        let later = utterance(3, ConversationSpeaker::User, "fala posterior", 300, 3);
        let current = utterance(
            2,
            ConversationSpeaker::OtherPerson,
            "Como voce resolveria isso?",
            200,
            2,
        );
        let request = snapshot(&[later], &current);
        let built = DefaultResponseContextBuilder.build(&request);
        assert!(prompt(&built).contains(&current.text));
        assert!(!prompt(&built).contains("fala posterior"));
    }

    #[test]
    fn context_has_at_most_two_remote_and_one_user_utterance() {
        let history = vec![
            utterance(1, ConversationSpeaker::OtherPerson, "remota um", 0, 1),
            utterance(2, ConversationSpeaker::User, "usuario um", 200, 2),
            utterance(3, ConversationSpeaker::User, "usuario dois", 400, 3),
            utterance(4, ConversationSpeaker::OtherPerson, "remota dois", 600, 4),
            utterance(5, ConversationSpeaker::OtherPerson, "remota tres", 800, 5),
        ];
        let current = utterance(
            6,
            ConversationSpeaker::OtherPerson,
            "E como voce resolveria isso?",
            1_000,
            6,
        );
        let request = snapshot(&history, &current);
        assert_eq!(request.previous_remote_context.len(), 2);
        assert_eq!(request.latest_user_answer.as_deref(), Some("usuario dois"));
        assert!(!request
            .previous_remote_context
            .iter()
            .any(|v| v == "remota um"));
    }

    #[test]
    fn independent_question_excludes_old_irrelevant_conversation() {
        let history = vec![utterance(
            1,
            ConversationSpeaker::User,
            "Vou te mandar uma pergunta relacionada a tecnologia",
            0,
            1,
        )];
        let current = utterance(
            2,
            ConversationSpeaker::OtherPerson,
            "Qual metal puro tem maior condutividade eletrica?",
            200,
            2,
        );
        let request = snapshot(&history, &current);
        assert!(request.previous_remote_context.is_empty());
        assert!(request.latest_user_answer.is_none());
        assert!(!prompt(&DefaultResponseContextBuilder.build(&request)).contains("Vou te mandar"));
    }

    #[test]
    fn raw_and_normalized_are_never_both_put_in_prompt() {
        let history = vec![utterance(
            1,
            ConversationSpeaker::OtherPerson,
            "RabbitMQ",
            0,
            1,
        )];
        let current = utterance(
            2,
            ConversationSpeaker::OtherPerson,
            "E como isso funciona?",
            200,
            2,
        );
        let request = snapshot(&history, &current);
        let text = prompt(&DefaultResponseContextBuilder.build(&request));
        assert!(text.contains("RabbitMQ"));
        assert!(!text.contains("raw RabbitMQ"));
        assert!(request
            .context_leak_references
            .iter()
            .any(|v| v == "raw RabbitMQ"));
    }

    #[test]
    fn snapshot_does_not_change_when_history_changes_after_trigger() {
        let mut history = vec![utterance(
            1,
            ConversationSpeaker::OtherPerson,
            "contexto original",
            0,
            1,
        )];
        let current = utterance(2, ConversationSpeaker::OtherPerson, "Explique isso", 200, 2);
        let request = snapshot(&history, &current);
        history.push(utterance(
            3,
            ConversationSpeaker::User,
            "fala posterior",
            400,
            3,
        ));
        let text = prompt(&DefaultResponseContextBuilder.build(&request));
        assert!(text.contains("contexto original"));
        assert!(!text.contains("fala posterior"));
    }

    #[test]
    fn repair_prompt_contains_only_current_speech_and_not_invalid_output_or_history() {
        let history = vec![utterance(
            1,
            ConversationSpeaker::User,
            "conversa inteira anterior",
            0,
            1,
        )];
        let current = utterance(2, ConversationSpeaker::OtherPerson, "Explique CAP", 200, 2);
        let request = snapshot(&history, &current);
        let text = prompt(&DefaultResponseContextBuilder.build_repair(&request));
        assert!(text.contains("Explique CAP"));
        assert!(!text.contains("conversa inteira anterior"));
    }

    #[test]
    fn previous_suggestion_is_a_guard_reference_but_never_prompt_context() {
        let current = utterance(
            2,
            ConversationSpeaker::OtherPerson,
            "Qual e a resposta atual?",
            200,
            2,
        );
        let request = snapshot_generation_request(
            SessionId::from_value(1),
            TurnId::from_raw(9),
            &current,
            GenerationId::from_raw(7),
            &[],
            Some("sugestao anterior confidencial".to_string()),
            None,
            Instant::now(),
            Instant::now(),
            true,
        );
        let text = prompt(&DefaultResponseContextBuilder.build(&request));
        assert!(!text.contains("sugestao anterior confidencial"));
        assert!(request
            .context_leak_references
            .iter()
            .any(|value| value == "sugestao anterior confidencial"));
    }

    #[test]
    fn context_character_budget_never_exceeds_three_thousand() {
        let history = vec![
            utterance(
                1,
                ConversationSpeaker::OtherPerson,
                &"contexto remoto antigo ".repeat(100),
                0,
                1,
            ),
            utterance(
                2,
                ConversationSpeaker::OtherPerson,
                &"contexto remoto recente ".repeat(100),
                300,
                2,
            ),
            utterance(
                3,
                ConversationSpeaker::User,
                &"resposta do usuario ".repeat(100),
                600,
                3,
            ),
        ];
        let current = utterance(
            4,
            ConversationSpeaker::OtherPerson,
            "Como isso se aplica?",
            900,
            4,
        );
        let request = snapshot(&history, &current);
        let built = DefaultResponseContextBuilder.build(&request);
        assert!(
            built.context_character_count + current.text.chars().count() <= MAX_CONTEXT_CHARACTERS
        );
        assert!(request.latest_user_answer.is_some());
        assert!(
            request.previous_remote_context.is_empty(),
            "a resposta recente do usuario tem prioridade quando o budget aperta"
        );
    }
}
