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
//!
//! Duas regras foram acrescentadas depois de uma sessão ao vivo:
//!
//! - **Voz.** O texto gerado é lido em voz alta *pelo usuário*, como fala dele. O modelo
//!   escorregava para a voz de assistente ("Se quiser, posso te mostrar como ela
//!   funciona"), que é inútil — o usuário não pode ler isso numa entrevista. O prompt agora
//!   diz de quem é a voz e proíbe explicitamente oferta de serviço e pergunta de
//!   fechamento.
//! - **Enunciado que ainda não pede nada.** Uma pergunta falada costuma vir em duas partes
//!   ("Tenho uma query que respondia em menos de um segundo." … "O que pode ter
//!   acontecido?"), e o silêncio entre elas finaliza a primeira utterance. Responder à
//!   premissa produz uma resposta a meia pergunta. Ela entrou na lista fechada de `[SKIP]`
//!   como o único caso em que a dúvida se resolve pulando em vez de respondendo curto — a
//!   premissa vira contexto da fala seguinte, que é onde o pedido aparece.

use crate::conversation::{ConversationSpeaker, ConversationTurn, SessionId};

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
usuário à fala atual, em 2 a 4 frases. Você está escrevendo exatamente o que o usuário irá \
falar em uma entrevista técnica. Você é o próprio usuário falando na reunião, não um \
assistente atendendo alguém: nunca se ofereça para mostrar, explicar, buscar ou fazer algo, \
e nunca termine perguntando se a pessoa quer mais detalhes. Nunca responda com uma palavra \
isolada, só pontuação, \"sim\", \"não\", \"tchau\" ou \"?\". \
Vá direto ao conteúdo: nada de repetir ou \
reformular a pergunta, nada de comentar se ela exige resposta, nada de preâmbulo. \
Use o contexto apenas para resolver referências, e não invente nome, número, data, \
empresa ou tecnologia que não esteja nele. \
A pontuação da transcrição não é confiável: um pedido ou pergunta sem \"?\" continua \
sendo um pedido. \
Escreva apenas [SKIP] se a fala atual for somente saudação, somente uma confirmação \
isolada, um fragmento sem sentido, ou apenas um enunciado que monta contexto sem pedir \
nada ainda.";

const NO_CONTEXT_PLACEHOLDER: &str = "(nenhum — esta é a primeira fala da sessão)";

const SYSTEM_PROMPT: &str = "Você é um copiloto que ajuda o usuário durante uma reunião ao vivo. \
Você recebe o contexto recente da conversa e a fala atual da outra pessoa, e escreve a \
resposta que o usuário daria agora, em primeira pessoa. \
O texto que você escreve vai ser lido em voz alta pelo próprio usuário, como fala dele: \
você está escrevendo *como* ele, uma pessoa participando da conversa do próprio \
conhecimento — nunca como um assistente conversando com ele ou com a outra pessoa. \
Você está escrevendo exatamente o que o usuário irá falar em uma entrevista técnica. \
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
- enunciado que só monta contexto e ainda não pede nada: a pessoa está apresentando um \
fato, uma premissa ou o começo de um problema, sem nenhuma pergunta, pedido, imperativo \
ou convite a falar. O pedido costuma vir na fala seguinte; responder agora é responder a \
meia pergunta;\n\
- fala que claramente não é dirigida ao usuário.\n\
Se a fala contiver qualquer pergunta, pedido, imperativo ou convite a falar — inclusive \
logo depois de uma confirmação, como em \"Perfeito. Me conta como foi\" — responda ao \
pedido. Em qualquer dúvida entre responder e pular, responda curto — exceto quando a \
dúvida for exatamente esta: a fala não pede nada e parece ser só o começo do assunto. \
Aí espere, com [SKIP].\n\
\n\
Como responder:\n\
- primeira pessoa, tom natural de fala, 2 a 4 frases;\n\
- direto ao conteúdo, sem se apresentar e sem repetir a pergunta antes de responder;\n\
- nunca ofereça serviço nem ajuda: nada de \"se quiser, posso te mostrar\", \"quer que eu \
explique\", \"posso te ajudar com isso\". Quem fala é um participante da reunião \
respondendo, não uma ferramenta se colocando à disposição;\n\
- nunca responda apenas \"sim\", \"não\", \"tchau\", \"?\", \".\" ou qualquer pontuação;\n\
- nunca gere uma resposta vazia; quando faltar contexto, dê uma resposta curta, honesta e técnica;\n\
- nunca termine perguntando se a resposta serviu nem oferecendo continuar;\n\
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
- \"Tenho um serviço que até ontem respondia rápido.\" → [SKIP] (só monta o cenário, \
ainda não pede nada — o pedido vem depois);\n\
- \"Beleza, faz sentido.\" → [SKIP];\n\
- \"Bom dia, tudo bem?\" → [SKIP].\n\
\n\
Para pular, responda apenas com o texto exato [SKIP] e nada mais, sem explicações e sem \
pontuação extra.";

/// Tudo o que a montagem de contexto tem direito de olhar. É deliberadamente um struct
/// fechado, e não `&ResponseEngine`: o que **não** está aqui é a lista do que não pode
/// entrar no prompt — diagnósticos, IDs, sugestões anteriores, texto bruto, histórico de
/// outra sessão. Um builder novo não tem como incluir nada disso por engano, porque não
/// recebe.
///
/// `history` já vem filtrado por sessão pelo chamador
/// (`ResponseEngine::history_snapshot(session_id)`); `session_id` viaja junto só para
/// atribuição em log, nunca para o texto do prompt.
pub struct ResponseContextInput<'a> {
    pub session_id: SessionId,
    /// Turnos finalizados anteriores da sessão ativa, do mais antigo para o mais recente.
    pub history: &'a [ConversationTurn],
    /// Turno elegível cuja utterance acabou de finalizar.
    pub current_turn: &'a ConversationTurn,
    /// Texto exato da utterance atual — a fala que será respondida ou pulada. Já
    /// normalizado pela camada de normalização; o texto bruto não chega até aqui.
    pub current_utterance_text: &'a str,
}

/// Resultado da montagem: a requisição em si mais os números que os diagnósticos e os logs
/// (`context_built`, `context_turn_count`, `context_character_count`) precisam.
#[derive(Debug, Clone)]
pub struct ResponseContext {
    pub request: ResponseRequest,
    /// Quantas linhas de contexto conversacional entraram no prompt.
    pub context_turn_count: usize,
    /// Tamanho em caracteres do bloco de contexto (sem a fala atual e sem a instrução).
    pub context_character_count: usize,
    /// Versão abreviada do prompt para inspeção em modo de desenvolvedor.
    pub sanitized_preview: String,
}

/// Ponto de extensão da montagem de prompt. Existe separado do `ResponseEngine` porque as
/// duas coisas mudam por motivos diferentes: o motor cuida de sessão, cancelamento,
/// streaming e estado terminal — tudo isso é invariante de produto; o prompt é a parte que
/// se ajusta por experimentação. Trocar um não deve arriscar o outro.
pub trait ResponseContextBuilder: Send + Sync {
    fn build(&self, input: ResponseContextInput<'_>) -> ResponseContext;
}

/// A montagem descrita no cabeçalho deste módulo. Sem estado: os tetos são constantes de
/// compilação, não configuração de runtime, porque mudá-los muda a latência medida.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultResponseContextBuilder;

impl ResponseContextBuilder for DefaultResponseContextBuilder {
    fn build(&self, input: ResponseContextInput<'_>) -> ResponseContext {
        let built = build_request(
            input.history,
            input.current_turn,
            input.current_utterance_text,
        );
        tracing::trace!(
            session_id = input.session_id.value(),
            history_turns_available = input.history.len(),
            context_turn_count = built.context_turn_count,
            context_character_count = built.context_character_count,
            "contexto montado"
        );
        built
    }
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
fn build_request(
    history: &[ConversationTurn],
    current: &ConversationTurn,
    current_utterance_text: &str,
) -> ResponseContext {
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

    ResponseContext {
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

    fn prompt_text(built: &ResponseContext) -> String {
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
        assert!(system.contains("Em qualquer dúvida entre responder e pular, responda curto"));
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

    /// O texto gerado é lido em voz alta pelo usuário. Voz de assistente ("Se quiser, posso
    /// te mostrar como ela funciona") é inutilizável numa reunião — a regra tem que estar
    /// nos dois lugares, porque modelos pequenos seguem melhor o que leram por último.
    #[test]
    fn prompt_states_whose_voice_it_is_and_forbids_offering_assistance() {
        let speech = "Só que de ontem para hoje ela começou a demorar cinco segundos.";
        let current = turn(1, ConversationSpeaker::OtherPerson, speech);
        let built = build_request(&[], &current, speech);
        let system = &built.request.messages[0].content;
        let user = &built.request.messages.last().unwrap().content;

        assert!(system.contains("lido em voz alta pelo próprio usuário"));
        assert!(system.contains("nunca ofereça serviço nem ajuda"));
        assert!(system.contains("se quiser, posso te mostrar"));
        assert!(system.contains("nunca termine perguntando se a resposta serviu"));
        assert!(user.contains(
            "não um \
assistente atendendo alguém"
        ));
        assert!(user.contains("nunca termine perguntando se a pessoa quer mais detalhes"));
    }

    /// Uma pergunta falada chega partida em duas utterances quando há silêncio no meio. A
    /// primeira metade é só premissa; responder a ela é responder a meia pergunta.
    #[test]
    fn skip_policy_covers_a_statement_that_does_not_ask_for_anything_yet() {
        let speech = "Eu tenho uma query que até ontem respondia em menos de um segundo.";
        let current = turn(1, ConversationSpeaker::OtherPerson, speech);
        let built = build_request(&[], &current, speech);
        let system = &built.request.messages[0].content;
        let user = &built.request.messages.last().unwrap().content;

        assert!(system.contains("enunciado que só monta contexto e ainda não pede nada"));
        assert!(system.contains("responder agora é responder a meia pergunta"));
        // A exceção precisa ser explícita, senão ela é anulada pela regra anterior
        // ("em qualquer dúvida, responda curto").
        assert!(system.contains("Aí espere, com [SKIP]"));
        assert!(user.contains(
            "apenas um enunciado que monta contexto sem pedir \
nada ainda"
        ));
    }

    /// A metade que traz o pedido tem que chegar ao modelo com a premissa anterior como
    /// contexto — é isso que faz a resposta cobrir a pergunta inteira mesmo partida.
    #[test]
    fn the_half_that_carries_the_request_sees_the_previous_half_as_context() {
        let premise = "Eu tenho uma query que até ontem respondia em menos de um segundo.";
        let request = "Só que hoje ela demora cinco segundos. O que pode ter acontecido?";
        let current = turn(
            1,
            ConversationSpeaker::OtherPerson,
            &format!("{premise} {request}"),
        );
        let built = build_request(&[], &current, request);
        let user = &built.request.messages.last().unwrap().content;

        let context_block = user.split(CURRENT_SPEECH_HEADER).next().unwrap();
        assert!(context_block.contains(premise));
        assert!(!context_block.contains("O que pode ter acontecido?"));
        assert!(user.contains(&format!("{CURRENT_SPEECH_HEADER}\n{request}")));
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

    fn build(input: ResponseContextInput<'_>) -> ResponseContext {
        DefaultResponseContextBuilder.build(input)
    }

    fn input<'a>(
        history: &'a [ConversationTurn],
        current: &'a ConversationTurn,
        utterance: &'a str,
    ) -> ResponseContextInput<'a> {
        ResponseContextInput {
            session_id: SessionId::new(),
            history,
            current_turn: current,
            current_utterance_text: utterance,
        }
    }

    /// O builder padrão é a mesma montagem, só alcançada pelo trait. Se um dia divergirem,
    /// o motor passaria a mandar um prompt diferente do que os testes acima verificam.
    #[test]
    fn trait_builder_matches_the_direct_assembly() {
        let current = turn(2, ConversationSpeaker::OtherPerson, "e como você resolveu");
        let history = vec![turn(1, ConversationSpeaker::User, "usei fila")];
        let direct = build_request(&history, &current, "e como você resolveu");
        let via_trait = build(input(&history, &current, "e como você resolveu"));

        assert_eq!(direct.request.messages, via_trait.request.messages);
        assert_eq!(direct.context_turn_count, via_trait.context_turn_count);
        assert_eq!(
            direct.context_character_count,
            via_trait.context_character_count
        );
    }

    /// Nenhum identificador interno pode vazar para o prompt. IDs não são conversa: além de
    /// gastar token, dão ao modelo material para citar ("no turno 7 você disse...").
    #[test]
    fn prompt_never_contains_internal_identifiers() {
        let current = turn(4242, ConversationSpeaker::OtherPerson, "e daí?");
        let history = vec![turn(777, ConversationSpeaker::User, "estava tudo bem")];
        let built = build(input(&history, &current, "e daí?"));
        let prompt = prompt_text(&built);

        for needle in [
            "4242",
            "777",
            "turn_id",
            "utterance_id",
            "session_id",
            "generation_id",
            "TurnId",
        ] {
            assert!(
                !prompt.contains(needle),
                "prompt não pode carregar {needle:?}"
            );
        }
    }

    /// Diagnóstico é observabilidade, não contexto. Latência, motivo de finalização e
    /// contagem de normalizações não entram no prompt em nenhuma circunstância — e o
    /// `ResponseContextInput` nem tem por onde recebê-los.
    #[test]
    fn prompt_never_contains_diagnostics() {
        let current = turn(1, ConversationSpeaker::OtherPerson, "me explica isso");
        let built = build(input(&[], &current, "me explica isso"));
        let prompt = prompt_text(&built);

        for needle in [
            "inactivity_timeout",
            "gap_ms",
            "silence_detected",
            "latency",
            "_ms",
            "finalization_reason",
            "normalization_changes",
        ] {
            assert!(
                !prompt.contains(needle),
                "prompt não pode carregar diagnóstico {needle:?}"
            );
        }
    }

    /// O bloco de contexto respeita o teto de caracteres mesmo com histórico enorme. O teto
    /// é o que mantém o tempo de prefill previsível: sem ele, uma reunião longa faria a
    /// latência crescer com a duração da conversa.
    #[test]
    fn context_block_respects_the_character_cap() {
        let current = turn(100, ConversationSpeaker::OtherPerson, "e agora");
        let history: Vec<ConversationTurn> = (0..40)
            .map(|i| turn(i, ConversationSpeaker::User, &"palavra ".repeat(400)))
            .collect();

        let built = build(input(&history, &current, "e agora"));
        assert!(
            built.context_character_count <= MAX_HISTORY_CHARS,
            "bloco de contexto com {} caracteres",
            built.context_character_count
        );
        assert!(built.context_turn_count <= MAX_HISTORY_TURNS);
    }

    /// Exatamente uma fala atual. Utterances anteriores do mesmo turno viram contexto; a
    /// atual aparece uma única vez, sob o cabeçalho de fala atual.
    #[test]
    fn exactly_one_current_utterance_reaches_the_model() {
        let first = "Eu tenho um serviço que roda em três instâncias.";
        let second = "O que você faria para diagnosticar";
        let current = turn(
            1,
            ConversationSpeaker::OtherPerson,
            &format!("{first} {second}"),
        );
        let built = build(input(&[], &current, second));
        let user = built.request.messages.last().unwrap().content.clone();

        let speech = &user[user.find(CURRENT_SPEECH_HEADER).unwrap()..];
        assert_eq!(speech.matches(second).count(), 1);
        assert!(!speech.contains(first));
        assert_eq!(user.matches(CURRENT_SPEECH_HEADER).count(), 1);
    }

    /// Sugestões anteriores não podem voltar como diálogo. O histórico é de
    /// `ConversationTurn` — o que o **usuário falou** —, e o `ResponseContextInput` não tem
    /// campo por onde uma sugestão gerada entraria. Este teste fixa esse contrato: o que
    /// aparece no contexto é só o que foi dito.
    #[test]
    fn previously_generated_suggestions_are_not_replayed_as_dialogue() {
        let suggestion = "Na prática eu separaria a leitura da escrita e mediria antes.";
        let current = turn(2, ConversationSpeaker::OtherPerson, "e o que mais");
        let history = vec![turn(1, ConversationSpeaker::User, "a gente usa Postgres")];
        let built = build(input(&history, &current, "e o que mais"));

        let prompt = prompt_text(&built);
        assert!(!prompt.contains(suggestion));
        assert!(prompt.contains("a gente usa Postgres"));
    }

    /// Bruto e normalizado nunca convivem no prompt. O que chega aqui já é o texto
    /// normalizado; o bruto existe só em `TranscriptNormalizationResult`, para diagnóstico.
    #[test]
    fn only_normalized_text_reaches_the_prompt() {
        let raw = "a gente usa micro serviços com rabbit mq";
        let normalized = "A gente usa microserviços com RabbitMQ";
        let current = turn(2, ConversationSpeaker::OtherPerson, "e quantos são");
        let history = vec![turn(1, ConversationSpeaker::User, normalized)];
        let built = build(input(&history, &current, "e quantos são"));

        let prompt = prompt_text(&built);
        assert!(prompt.contains(normalized));
        assert!(!prompt.contains(raw));
    }

    /// Um mesmo trecho não pode entrar duas vezes: uma como turno do histórico e outra como
    /// fala precedente do turno atual. Duplicação embaralha a decisão de `[SKIP]` e gasta
    /// orçamento de contexto com repetição.
    #[test]
    fn context_never_duplicates_the_same_speech() {
        let preceding = "Perfeito.";
        let speech = "Me conta um caso real";
        let current = turn(
            2,
            ConversationSpeaker::OtherPerson,
            &format!("{preceding} {speech}"),
        );
        // O mesmo turno também presente no histórico — é o que acontece quando o turno já
        // foi empurrado para o histórico antes da geração disparar.
        let history = vec![
            turn(1, ConversationSpeaker::User, "trabalhei com filas"),
            current.clone(),
        ];
        let built = build(input(&history, &current, speech));
        let user = built.request.messages.last().unwrap().content.clone();
        let context_block =
            &user[user.find(CONTEXT_HEADER).unwrap()..user.find(CURRENT_SPEECH_HEADER).unwrap()];

        assert_eq!(context_block.matches(preceding).count(), 1);
        assert_eq!(context_block.matches(speech).count(), 0);
    }

    /// Turnos vazios não ocupam linha de contexto. Uma utterance que virou texto vazio
    /// depois da normalização não pode consumir um dos quatro turnos disponíveis.
    #[test]
    fn blank_turns_do_not_consume_context_slots() {
        let current = turn(10, ConversationSpeaker::OtherPerson, "e então");
        let history = vec![
            turn(1, ConversationSpeaker::User, "   "),
            turn(2, ConversationSpeaker::OtherPerson, ""),
            turn(3, ConversationSpeaker::User, "isso mesmo"),
        ];
        let built = build(input(&history, &current, "e então"));

        assert_eq!(built.context_turn_count, 1);
        let user = built.request.messages.last().unwrap().content.clone();
        assert!(user.contains("isso mesmo"));
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
