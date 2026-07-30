//! Monta o prompt enviado ao provedor a partir do histórico de turnos e da utterance que
//! acabou de finalizar. Contexto deliberadamente limitado — teto de caracteres e de
//! turnos — para manter o prompt pequeno e a latência baixa em vez de reenviar a conversa
//! inteira a cada geração.
//!
//! Duas invariantes de domínio moram aqui:
//!
//! 1. **O prompt é sempre montado a partir de um histórico de uma única sessão.** Esta
//!    camada não conhece "a conversa"; ela recebe exatamente os turnos que o
//!    `ResponseEngine` guardou para a sessão *ativa* (`history_snapshot(session_id)`).
//!    Não existe snapshot global aqui, e nenhum turno chega por outro caminho.
//! 2. **A decisão de responder ou usar `[SKIP]` é sobre a fala atual, não sobre o
//!    contexto.** Por isso o prompt separa fisicamente `CONTEXTO RECENTE:` de
//!    `FALA ATUAL DA OUTRA PESSOA:` e instrui o modelo a usar o contexto apenas para
//!    resolver referências ("isso", "esse problema"). Sem essa separação, um "Perfeito."
//!    isolado e um "Perfeito. Me conta um caso real..." se pareciam demais no prompt.
//!
//! A separação sozinha não bastou: modelos locais menores continuavam devolvendo `[SKIP]`
//! para pedidos claros no imperativo. Duas causas conhecidas, ambas endereçadas no
//! `SYSTEM_PROMPT`: a transcrição raramente produz "?" (então "Me conta um caso real"
//! chega sem nenhuma marca sintática de pergunta), e a política de skip era descrita por
//! adjetivos ("claramente não pedir resposta") em vez de uma lista fechada. Hoje o prompt
//! diz explicitamente que a pontuação não é confiável, enumera os únicos casos de skip e
//! traz exemplos curtos — inclusive o padrão exato que falhava, confirmação seguida de
//! pedido.

use crate::conversation::{ConversationSpeaker, ConversationTurn};

use super::provider::{ResponseMessage, ResponseRequest, ResponseRole};

// Teto de contexto e de saída deliberadamente pequeno (ver `docs/response-suggestion.md`,
// seção de latência): menos tokens de entrada e saída significam menos tempo de
// prefill/decode no provedor, sem depender de nenhuma otimização de infraestrutura.
const MAX_HISTORY_TURNS: usize = 4;
const MAX_HISTORY_CHARS: usize = 5_000;
const MAX_OUTPUT_TOKENS: u32 = 160;
/// Baixa de propósito: sugestão de resposta em reunião ao vivo quer a resposta mais
/// provável e direta, não variedade criativa — determinismo também ajuda a manter a
/// latência estável entre chamadas.
const TEMPERATURE: f32 = 0.2;

/// Quanto de cada linha aparece no `sanitized_preview` (diagnóstico em modo dev). O
/// preview existe para conferir *estrutura* e *origem* do contexto, não para reler a
/// conversa inteira na UI.
const PREVIEW_LINE_CHARS: usize = 120;

pub const CONTEXT_HEADER: &str = "CONTEXTO RECENTE:";
pub const CURRENT_SPEECH_HEADER: &str = "FALA ATUAL DA OUTRA PESSOA:";
pub const INSTRUCTION_HEADER: &str = "INSTRUÇÃO:";

/// A instrução é a última coisa que o modelo lê antes de gerar, e é a que mais pesa. Ela
/// pedia só uma *decisão* ("Decida exclusivamente se a fala atual exige resposta"), o que
/// contradizia o `SYSTEM_PROMPT` e fazia modelos locais menores devolverem a análise da
/// fala — ou a própria pergunta reformulada — em vez da resposta. A tarefa é escrever a
/// resposta; `[SKIP]` é a exceção, não o objetivo.
///
/// Ela repete, em forma curta, as três regras do `SYSTEM_PROMPT` que mais falhavam na
/// prática (pontuação não confiável, lista fechada de `[SKIP]`, proibição de inventar
/// específicos) porque um modelo pequeno tende a seguir o que leu por último — não é
/// redundância acidental.
const INSTRUCTION: &str = "INSTRUÇÃO: Escreva agora, em primeira pessoa, a resposta do \
usuário à fala atual, em 2 a 4 frases. Vá direto ao conteúdo: nada de repetir ou \
reformular a pergunta, nada de comentar se ela exige resposta, nada de preâmbulo. \
Use o contexto apenas para resolver referências, e não invente nome, número, data, \
empresa ou tecnologia que não esteja nele. \
A pontuação da transcrição não é confiável: um pedido ou pergunta sem \"?\" continua \
sendo um pedido. \
Escreva apenas [SKIP] se a fala atual for somente saudação, somente uma confirmação \
isolada, ou um fragmento sem sentido.";

const NO_CONTEXT_PLACEHOLDER: &str = "(nenhum — esta é a primeira fala da sessão)";

const SYSTEM_PROMPT: &str = "Você é um copiloto que ajuda o usuário durante uma reunião ao vivo. \
Você recebe o contexto recente da conversa e a fala atual da outra pessoa, e escreve a \
resposta que o usuário daria agora, em primeira pessoa. \
Sua decisão é sempre sobre a fala atual: o contexto serve apenas para entender referências \
como \"isso\", \"esse problema\" ou \"essa decisão\".\n\
\n\
O texto vem de transcrição automática de fala: a pontuação é pouco confiável e o ponto de \
interrogação quase sempre falta. Nunca decida pela pontuação, decida pelo conteúdo. \
\"Me conta como foi\", \"explica melhor\", \"e como você resolveu isso\" são pedidos, mesmo \
escritos sem \"?\".\n\
\n\
Responder é o padrão. Use [SKIP] apenas nestes casos, e em nenhum outro:\n\
- saudação isolada;\n\
- confirmação ou reação isolada, sem nada depois (\"perfeito\", \"entendi\", \"faz sentido\");\n\
- fragmento truncado, ruído ou fala sem sentido;\n\
- fala que claramente não é dirigida ao usuário.\n\
Se a fala contiver qualquer pergunta, pedido, imperativo ou convite a falar — inclusive \
logo depois de uma confirmação, como em \"Perfeito. Me conta como foi\" — responda ao \
pedido. Em qualquer dúvida, responda curto em vez de usar [SKIP].\n\
\n\
Como responder:\n\
- primeira pessoa, tom natural de fala, 2 a 4 frases;\n\
- direto ao conteúdo, sem se apresentar e sem repetir a pergunta antes de responder;\n\
- não invente fatos específicos que não estejam no contexto: nomes de empresa, clientes, \
produtos, datas, números, métricas ou tecnologias;\n\
- se o pedido exigir um detalhe pessoal que você não tem, responda pelo raciocínio e pela \
estrutura da experiência, deixando o específico em aberto para o usuário completar, em vez \
de fabricar um caso;\n\
- se não houver base para responder, dê a resposta mais curta e honesta possível — nunca \
preencha com detalhe inventado.\n\
\n\
Exemplos, só para calibrar a decisão (não copie este formato na resposta):\n\
- \"Perfeito. Me conta um caso em que isso deu errado\" → responder (é um pedido, mesmo \
vindo depois de uma confirmação);\n\
- \"E quais foram as limitações disso\" → responder (é pergunta, mesmo sem \"?\");\n\
- \"Beleza, faz sentido.\" → [SKIP];\n\
- \"Bom dia, tudo bem?\" → [SKIP].\n\
\n\
Para pular, responda apenas com o texto exato [SKIP] e nada mais, sem explicações e sem \
pontuação extra.";

/// Resultado de `build_request`: a requisição em si mais os números que os diagnósticos e
/// os logs (`context_built`, `context_turn_count`, `context_character_count`) precisam.
#[derive(Debug, Clone)]
pub struct BuiltContext {
    pub request: ResponseRequest,
    /// Quantas linhas de contexto conversacional entraram no prompt.
    pub context_turn_count: usize,
    /// Tamanho em caracteres do bloco de contexto (sem a fala atual e sem a instrução).
    pub context_character_count: usize,
    /// Versão abreviada do prompt para inspeção em modo de desenvolvedor.
    pub sanitized_preview: String,
}

fn speaker_label(speaker: ConversationSpeaker) -> &'static str {
    match speaker {
        ConversationSpeaker::User => "Você",
        ConversationSpeaker::OtherPerson => "Outra pessoa",
    }
}

fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let head: String = text.chars().take(limit).collect();
    format!("{head}…")
}

/// O que o mesmo interlocutor já havia dito *neste turno* antes da utterance atual. O
/// turno em andamento pode conter várias utterances; a atual vai para
/// `FALA ATUAL DA OUTRA PESSOA:` e o que veio antes dela é contexto, não a fala a
/// classificar.
fn preceding_text_in_turn(turn: &ConversationTurn, current_utterance_text: &str) -> String {
    let current = current_utterance_text.trim();
    let full = turn.text.trim();
    if current.is_empty() {
        return full.to_string();
    }
    if let Some(prefix) = full.strip_suffix(current) {
        return prefix.trim().to_string();
    }
    match full.rfind(current) {
        Some(index) => full[..index].trim().to_string(),
        // A junção de texto da timeline normaliza espaços, então a utterance atual
        // normalmente é sufixo exato do turno. Se não for, é mais seguro não mandar o
        // turno inteiro (duplicaria a fala atual dentro do contexto e embaralharia a
        // decisão de `[SKIP]`) do que tentar adivinhar onde ela começa.
        None => String::new(),
    }
}

/// `history` deve conter turnos finalizados anteriores **da sessão ativa**, do mais antigo
/// para o mais recente; `current` é o turno elegível cuja utterance acabou de finalizar; e
/// `current_utterance_text` é o texto exato dessa utterance — a fala que será classificada
/// e respondida.
pub fn build_request(
    history: &[ConversationTurn],
    current: &ConversationTurn,
    current_utterance_text: &str,
) -> BuiltContext {
    let mut context_lines = Vec::new();
    let mut budget = MAX_HISTORY_CHARS;

    let preceding = preceding_text_in_turn(current, current_utterance_text);
    if !preceding.is_empty() {
        let line = format!("{}: {}", speaker_label(current.speaker), preceding);
        budget = budget.saturating_sub(line.len());
        context_lines.push(line);
    }

    let mut history_turns = 0;
    for turn in history.iter().rev() {
        if turn.id == current.id || turn.text.trim().is_empty() {
            continue;
        }
        let line = format!("{}: {}", speaker_label(turn.speaker), turn.text.trim());
        if line.len() > budget {
            break;
        }
        budget -= line.len();
        context_lines.push(line);
        history_turns += 1;
        if history_turns >= MAX_HISTORY_TURNS {
            break;
        }
    }
    // `context_lines` foi montado do mais recente para o mais antigo (a fala anterior do
    // turno atual primeiro); o modelo lê melhor em ordem cronológica.
    context_lines.reverse();

    let context_block = if context_lines.is_empty() {
        NO_CONTEXT_PLACEHOLDER.to_string()
    } else {
        context_lines.join("\n")
    };
    let current_speech = current_utterance_text.trim();

    let user_content = format!(
        "{CONTEXT_HEADER}\n{context_block}\n\n{CURRENT_SPEECH_HEADER}\n{current_speech}\n\n{INSTRUCTION}"
    );

    let preview_context = if context_lines.is_empty() {
        NO_CONTEXT_PLACEHOLDER.to_string()
    } else {
        context_lines
            .iter()
            .map(|line| truncate_chars(line, PREVIEW_LINE_CHARS))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let sanitized_preview = format!(
        "{CONTEXT_HEADER}\n{preview_context}\n\n{CURRENT_SPEECH_HEADER}\n{}\n\n{INSTRUCTION_HEADER} (fixa)",
        truncate_chars(current_speech, PREVIEW_LINE_CHARS)
    );

    let request = ResponseRequest {
        messages: vec![
            ResponseMessage {
                role: ResponseRole::System,
                content: SYSTEM_PROMPT.to_string(),
            },
            ResponseMessage {
                role: ResponseRole::User,
                content: user_content,
            },
        ],
        max_output_tokens: MAX_OUTPUT_TOKENS,
        temperature: TEMPERATURE,
    };

    BuiltContext {
        request,
        context_turn_count: context_lines.len(),
        context_character_count: context_block.chars().count(),
        sanitized_preview,
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

    fn prompt_text(built: &BuiltContext) -> String {
        built
            .request
            .messages
            .iter()
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn includes_system_prompt_and_current_utterance() {
        let current = turn(1, ConversationSpeaker::OtherPerson, "qual sua experiência?");
        let built = build_request(&[], &current, "qual sua experiência?");

        assert_eq!(built.request.messages[0].role, ResponseRole::System);
        let last = built.request.messages.last().unwrap();
        assert!(last.content.contains(CURRENT_SPEECH_HEADER));
        assert!(last.content.contains("qual sua experiência?"));
    }

    #[test]
    fn separates_context_from_current_speech() {
        let current = turn(
            2,
            ConversationSpeaker::OtherPerson,
            "Perfeito. Me conta um caso real em que você optou por usar monolito.",
        );
        let history = vec![turn(1, ConversationSpeaker::User, "eu trabalhei com Rust")];
        let built = build_request(
            &history,
            &current,
            "Me conta um caso real em que você optou por usar monolito.",
        );

        let user = built.request.messages.last().unwrap().content.clone();
        let context_start = user.find(CONTEXT_HEADER).unwrap();
        let speech_start = user.find(CURRENT_SPEECH_HEADER).unwrap();
        let instruction_start = user.find(INSTRUCTION_HEADER).unwrap();
        assert!(context_start < speech_start && speech_start < instruction_start);

        let context_block = &user[context_start..speech_start];
        assert!(context_block.contains("eu trabalhei com Rust"));
        assert!(context_block.contains("Perfeito."));
        // A fala a classificar não pode aparecer duplicada dentro do contexto.
        assert!(!context_block.contains("Me conta um caso real"));

        let speech_block = &user[speech_start..instruction_start];
        assert!(speech_block.contains("Me conta um caso real em que você optou por usar monolito."));
    }

    #[test]
    fn excludes_current_turn_from_context_section() {
        let current = turn(2, ConversationSpeaker::OtherPerson, "e sobre isso?");
        let history = vec![
            turn(1, ConversationSpeaker::User, "eu trabalhei com Rust"),
            current.clone(),
        ];
        let built = build_request(&history, &current, "e sobre isso?");

        let user = built.request.messages.last().unwrap().content.clone();
        let context_block =
            &user[user.find(CONTEXT_HEADER).unwrap()..user.find(CURRENT_SPEECH_HEADER).unwrap()];
        assert!(context_block.contains("eu trabalhei com Rust"));
        assert_eq!(context_block.matches("e sobre isso?").count(), 0);
    }

    #[test]
    fn caps_history_to_max_turns() {
        let current = turn(100, ConversationSpeaker::OtherPerson, "última fala");
        let mut history: Vec<ConversationTurn> = (0..10)
            .map(|i| turn(i, ConversationSpeaker::User, &format!("turno {i}")))
            .collect();
        history.push(current.clone());

        let built = build_request(&history, &current, "última fala");
        assert_eq!(built.context_turn_count, MAX_HISTORY_TURNS);
        let user = built.request.messages.last().unwrap().content.clone();
        let context_block =
            &user[user.find(CONTEXT_HEADER).unwrap()..user.find(CURRENT_SPEECH_HEADER).unwrap()];
        assert_eq!(context_block.trim().lines().count() - 1, MAX_HISTORY_TURNS);
        assert!(built.context_character_count <= MAX_HISTORY_CHARS);
    }

    #[test]
    fn empty_history_reports_no_context() {
        let current = turn(1, ConversationSpeaker::OtherPerson, "oi");
        let built = build_request(&[], &current, "oi");
        assert_eq!(built.context_turn_count, 0);
        let user = built.request.messages.last().unwrap().content.clone();
        assert!(user.contains(NO_CONTEXT_PLACEHOLDER));
    }

    /// Prova direta do requisito de isolamento: dado um histórico que contém apenas turnos
    /// da sessão ativa, nenhum texto de uma sessão anterior pode aparecer no prompt. Quem
    /// garante que `history` só tem turnos da sessão ativa é
    /// `ResponseEngine::history_snapshot(session_id)` — testado em `engine.rs`.
    #[test]
    fn prompt_contains_only_supplied_history() {
        let current = turn(
            9,
            ConversationSpeaker::OtherPerson,
            "e agora, o que faríamos?",
        );
        let history = vec![turn(
            8,
            ConversationSpeaker::User,
            "estamos começando do zero",
        )];
        let built = build_request(&history, &current, "e agora, o que faríamos?");

        let prompt = prompt_text(&built);
        assert!(prompt.contains("estamos começando do zero"));
        assert!(!prompt.contains("monolito"));
        assert!(!prompt.contains("sessão anterior"));
    }

    #[test]
    fn system_prompt_states_skip_policy() {
        let current = turn(1, ConversationSpeaker::OtherPerson, "pode explicar melhor?");
        let built = build_request(&[], &current, "pode explicar melhor?");
        let system = &built.request.messages[0].content;
        assert!(system.contains("Responder é o padrão"));
        assert!(system.contains("Em qualquer dúvida, responda curto"));
        assert!(system.contains("[SKIP]"));
    }

    /// A transcrição quase nunca produz "?", então a decisão não pode depender de
    /// pontuação — foi assim que "Me conta um caso real em que você optou por usar
    /// monolito." virou `[SKIP]` na prática. Tanto o prompt de sistema quanto a instrução
    /// final precisam dizer isso, porque um modelo pequeno segue melhor o que leu por
    /// último.
    #[test]
    fn prompt_tells_the_model_that_transcription_punctuation_is_unreliable() {
        let current = turn(1, ConversationSpeaker::OtherPerson, "me conta como foi");
        let built = build_request(&[], &current, "me conta como foi");
        let system = &built.request.messages[0].content;
        let user = &built.request.messages.last().unwrap().content;

        assert!(system.contains("a pontuação é pouco confiável"));
        assert!(system.contains("Nunca decida pela pontuação"));
        assert!(user.contains("A pontuação da transcrição não é confiável"));
    }

    /// O caso que o usuário reportou: confirmação seguida de pedido, tudo numa fala só.
    /// A política precisa citar esse padrão explicitamente, não deixá-lo implícito numa
    /// regra genérica de "em dúvida, responda".
    #[test]
    fn skip_policy_is_a_closed_list_and_covers_confirmation_followed_by_a_request() {
        let speech = "Perfeito. Me conta um caso real em que você optou por usar monolito.";
        let current = turn(1, ConversationSpeaker::OtherPerson, speech);
        let system = build_request(&[], &current, speech).request.messages[0]
            .content
            .clone();

        assert!(system.contains("Use [SKIP] apenas nestes casos, e em nenhum outro"));
        assert!(system.contains("logo depois de uma confirmação"));
        assert!(system.contains("saudação isolada"));
        assert!(system.contains("fragmento truncado"));
    }

    /// Segunda metade do pedido do usuário: menos alucinação. A regra é sobre *específicos*
    /// (nome, número, data, empresa), não sobre responder menos — a resposta continua
    /// obrigatória, só não pode fabricar o detalhe que o modelo não tem como saber.
    #[test]
    fn prompt_forbids_inventing_specifics_absent_from_the_context() {
        let speech = "Me conta um caso real de migração que você tocou.";
        let current = turn(1, ConversationSpeaker::OtherPerson, speech);
        let built = build_request(&[], &current, speech);
        let system = &built.request.messages[0].content;
        let user = &built.request.messages.last().unwrap().content;

        assert!(system.contains("não invente fatos específicos que não estejam no contexto"));
        assert!(system.contains("nunca preencha com detalhe inventado"));
        assert!(user.contains("não invente nome, número, data, empresa ou tecnologia"));
    }

    /// As falas que o produto **precisa** responder e a que precisa pular, do requisito.
    /// O que este teste prova é estrutural e determinístico: cada uma dessas falas chega
    /// ao modelo isolada, sob `FALA ATUAL DA OUTRA PESSOA:`, com a política de `[SKIP]`
    /// explícita no prompt de sistema — não que um modelo específico decida corretamente,
    /// o que só pode ser observado rodando um provedor real (ver
    /// `docs/response-suggestion.md`, seção de validação).
    #[test]
    fn required_utterances_reach_the_model_isolated_as_current_speech() {
        let cases = [
            "Me conta um caso real em que você optou por usar monolito.",
            "Quais foram as limitações dessa decisão?",
            "Como você lidou com esse problema?",
            "Pode explicar melhor?",
            "Em qual situação você usaria microserviços?",
            "Perfeito.",
        ];
        let history = vec![turn(
            1,
            ConversationSpeaker::User,
            "contexto anterior qualquer",
        )];

        for (index, text) in cases.iter().enumerate() {
            let current = turn((index + 2) as u64, ConversationSpeaker::OtherPerson, text);
            let built = build_request(&history, &current, text);
            let user = built.request.messages.last().unwrap().content.clone();
            let speech = &user
                [user.find(CURRENT_SPEECH_HEADER).unwrap()..user.find(INSTRUCTION_HEADER).unwrap()];
            assert!(
                speech.contains(text),
                "{text:?} deve chegar como fala atual, não diluída no contexto"
            );
            // A instrução final tem que pedir a resposta, não uma classificação: pedir
            // "decida se exige resposta" fazia o modelo devolver a análise/pergunta.
            assert!(user.contains("Escreva agora, em primeira pessoa, a resposta do usuário"));
            assert!(!user.contains("Decida exclusivamente"));
        }
    }

    #[test]
    fn preview_is_truncated_and_keeps_structure() {
        let long = "a".repeat(PREVIEW_LINE_CHARS * 3);
        let current = turn(2, ConversationSpeaker::OtherPerson, &long);
        let history = vec![turn(1, ConversationSpeaker::User, &long)];
        let built = build_request(&history, &current, &long);

        assert!(built.sanitized_preview.contains(CONTEXT_HEADER));
        assert!(built.sanitized_preview.contains(CURRENT_SPEECH_HEADER));
        assert!(built.sanitized_preview.contains('…'));
        assert!(built.sanitized_preview.chars().count() < prompt_text(&built).chars().count());
    }
}
