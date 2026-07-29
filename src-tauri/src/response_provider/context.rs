//! Monta o prompt enviado ao provedor a partir do histórico de turnos e do turno que
//! acabou de ter uma utterance finalizada. Contexto deliberadamente limitado — teto de
//! caracteres e de turnos — para manter o prompt pequeno e a latência baixa em vez de
//! reenviar a conversa inteira a cada geração.

use crate::conversation::{ConversationSpeaker, ConversationTurn};

use super::provider::{ResponseMessage, ResponseRequest, ResponseRole};

const MAX_HISTORY_TURNS: usize = 6;
const MAX_HISTORY_CHARS: usize = 6_000;
const MAX_OUTPUT_TOKENS: u32 = 300;

const SYSTEM_PROMPT: &str = "Você é um copiloto que ajuda o usuário durante uma reunião ao vivo. \
Você recebe o histórico recente da conversa e a fala mais recente da outra pessoa. \
Se a fala mais recente não for uma pergunta ou pedido que exija resposta do usuário, \
responda apenas com o texto exato [SKIP] e nada mais, sem explicações, sem pontuação extra. \
Caso contrário, responda de forma breve, direta e natural, como se você fosse o usuário \
respondendo à outra pessoa agora, sem se apresentar e sem repetir a pergunta antes de \
responder.";

fn speaker_label(speaker: ConversationSpeaker) -> &'static str {
    match speaker {
        ConversationSpeaker::User => "Você",
        ConversationSpeaker::OtherPerson => "Outra pessoa",
    }
}

/// `history` deve conter turnos finalizados anteriores, do mais antigo para o mais
/// recente; `current` é o turno elegível cuja utterance acabou de finalizar.
pub fn build_request(history: &[ConversationTurn], current: &ConversationTurn) -> ResponseRequest {
    let mut history_lines = Vec::new();
    let mut budget = MAX_HISTORY_CHARS;
    for turn in history.iter().rev() {
        if turn.id == current.id || turn.text.is_empty() {
            continue;
        }
        let line = format!("{}: {}", speaker_label(turn.speaker), turn.text);
        if line.len() > budget {
            break;
        }
        budget -= line.len();
        history_lines.push(line);
        if history_lines.len() >= MAX_HISTORY_TURNS {
            break;
        }
    }
    history_lines.reverse();

    let mut messages = vec![ResponseMessage {
        role: ResponseRole::System,
        content: SYSTEM_PROMPT.to_string(),
    }];

    if !history_lines.is_empty() {
        messages.push(ResponseMessage {
            role: ResponseRole::User,
            content: format!(
                "Histórico recente da conversa:\n{}",
                history_lines.join("\n")
            ),
        });
    }

    messages.push(ResponseMessage {
        role: ResponseRole::User,
        content: format!(
            "Fala mais recente de {}: {}",
            speaker_label(current.speaker),
            current.text
        ),
    });

    ResponseRequest {
        messages,
        max_output_tokens: MAX_OUTPUT_TOKENS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::segment::AudioTimestamp;
    use crate::audio::types::AudioSource;
    use crate::conversation::TurnId;

    fn turn(id: u64, speaker: ConversationSpeaker, text: &str) -> ConversationTurn {
        ConversationTurn {
            id: TurnId::from_raw(id),
            speaker,
            source: match speaker {
                ConversationSpeaker::User => AudioSource::Microphone,
                ConversationSpeaker::OtherPerson => AudioSource::SystemOutput,
            },
            text: text.to_string(),
            utterances: Vec::new(),
            started_at: AudioTimestamp(0),
            ended_at: AudioTimestamp(1000),
            finalized_at: Some(AudioTimestamp(1000)),
        }
    }

    #[test]
    fn includes_system_prompt_and_current_turn() {
        let current = turn(1, ConversationSpeaker::OtherPerson, "qual sua experiência?");
        let request = build_request(&[], &current);

        assert_eq!(request.messages[0].role, ResponseRole::System);
        let last = request.messages.last().unwrap();
        assert!(last.content.contains("qual sua experiência?"));
    }

    #[test]
    fn excludes_current_turn_from_history_section() {
        let current = turn(2, ConversationSpeaker::OtherPerson, "e sobre isso?");
        let history = vec![
            turn(1, ConversationSpeaker::User, "eu trabalhei com Rust"),
            current.clone(),
        ];
        let request = build_request(&history, &current);

        let history_message = request
            .messages
            .iter()
            .find(|m| m.content.starts_with("Histórico"))
            .unwrap();
        assert!(history_message.content.contains("eu trabalhei com Rust"));
        assert_eq!(history_message.content.matches("e sobre isso?").count(), 0);
    }

    #[test]
    fn caps_history_to_max_turns() {
        let current = turn(100, ConversationSpeaker::OtherPerson, "última fala");
        let mut history: Vec<ConversationTurn> = (0..10)
            .map(|i| turn(i, ConversationSpeaker::User, &format!("turno {i}")))
            .collect();
        history.push(current.clone());

        let request = build_request(&history, &current);
        let history_message = request
            .messages
            .iter()
            .find(|m| m.content.starts_with("Histórico"))
            .unwrap();
        let line_count = history_message.content.lines().count() - 1;
        assert_eq!(line_count, MAX_HISTORY_TURNS);
    }

    #[test]
    fn no_history_omits_history_message() {
        let current = turn(1, ConversationSpeaker::OtherPerson, "oi");
        let request = build_request(&[], &current);
        assert!(!request
            .messages
            .iter()
            .any(|m| m.content.starts_with("Histórico")));
    }
}
