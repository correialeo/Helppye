//! Testes de integridade de origem da timeline.
//!
//! O que estes testes protegem não é uma função: é uma afirmação sobre o dado. **Um texto
//! nunca troca de fonte ou de speaker entre captura, transcrição, timeline e geração.** O
//! defeito que os motivou não estava no roteamento — cada `TranscriptSegment` tinha a fonte
//! certa — e sim na montagem do turno: uma utterance da saída de sistema era anexada a um
//! turno do microfone já aberto, e o turno é o que decide elegibilidade de geração.
//!
//! Ficam num arquivo próprio, incluído por `conversation.rs` via `#[path]`, pelo mesmo
//! motivo de `engine_critical_tests.rs`: são submódulo do `conversation` (logo enxergam o
//! `ConversationAssembler`, privado) sem inchar o módulo que já tem 2 mil linhas.

use std::time::Instant;

use super::*;
use crate::audio::segment::SegmentId;
use crate::audio::types::CaptureStreamId;
use crate::integrity::{diagnose_cross_source, CrossSourceConfig, CrossSourceDiagnosis};
use crate::normalization::TranscriptNormalizationResult;
use crate::response_provider::engine::is_eligible_turn;
use crate::transcription::envelope::TranscriptionResultEnvelope;
use crate::transcription::events::{FinalTranscript, ProviderEventId, TranscriptPayload};
use crate::transcription::provider::TranscriptionProviderId;
use crate::transcription::session::TranscriptionSessionId;

fn assembler() -> ConversationAssembler {
    ConversationAssembler::new(ConversationAssemblerConfig::default())
}

/// Segmento no formato do caminho de produção: identidade vinda de um envelope, texto vindo
/// do transcritor. Devolve `(segmento, envelope)` porque vários testes precisam comparar os
/// dois.
fn produced(
    source: AudioSource,
    stream: CaptureStreamId,
    sequence: u64,
    text: &str,
    start: u64,
    end: u64,
) -> (TranscriptSegment, TranscriptionResultEnvelope) {
    let envelope = TranscriptionResultEnvelope {
        session_id: SessionId::from_value(1),
        segment_id: SegmentId::next(),
        source,
        capture_stream_id: stream,
        sequence_number: sequence,
    };
    let transcript = final_transcript(source, text, start, end);
    let normalization = TranscriptNormalizationResult::unchanged(text.to_string());
    let segment = TranscriptSegment::from_normalized(
        &envelope,
        &transcript,
        &normalization,
        sequence,
        Instant::now(),
    )
    .expect("origem consistente")
    .expect("texto não vazio");
    (segment, envelope)
}

fn final_transcript(source: AudioSource, text: &str, start: u64, end: u64) -> FinalTranscript {
    FinalTranscript(TranscriptPayload {
        session_id: SessionId::from_value(1),
        transcription_session_id: TranscriptionSessionId::next(),
        source,
        provider: TranscriptionProviderId::WhisperLocal,
        language: Some("pt".into()),
        text: text.to_string(),
        started_at: AudioTimestamp(start),
        ended_at: AudioTimestamp(end),
        confidence: Some(0.9),
        is_final: true,
        provider_event_id: ProviderEventId::new("test-event"),
        segment_id: None,
        processing_time_ms: Some(12),
    })
}

fn segment_of(
    source: AudioSource,
    stream: CaptureStreamId,
    sequence: u64,
    text: &str,
    start: u64,
    end: u64,
) -> TranscriptSegment {
    produced(source, stream, sequence, text, start, end).0
}

/// Speaker e fonte do `TurnStarted`, quando houve um. É a forma que o frontend recebe: ele
/// não recalcula nada, lê estes dois campos.
fn turn_started_origin(
    events: &[ConversationTimelineEvent],
) -> Option<(ConversationSpeaker, AudioSource)> {
    events.iter().find_map(|event| match event {
        ConversationTimelineEvent::TurnStarted {
            speaker, source, ..
        } => Some((*speaker, *source)),
        _ => None,
    })
}

/// `ConversationTurn::utterances` guarda ids, não as utterances; resolve contra o snapshot.
fn utterances_of<'a>(
    snapshot: &'a ConversationTimelineSnapshot,
    turn: &ConversationTurn,
) -> Vec<&'a ConversationUtterance> {
    turn.utterances
        .iter()
        .filter_map(|id| snapshot.utterances.iter().find(|u| u.id == *id))
        .collect()
}

fn finalized_turns(events: &[ConversationTimelineEvent]) -> Vec<&ConversationTurn> {
    events
        .iter()
        .filter_map(|event| match event {
            ConversationTimelineEvent::TurnFinalized { turn, .. } => Some(turn),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 1–4. A origem sobrevive ao pipeline, e o speaker vem dela.
// ---------------------------------------------------------------------------

#[test]
fn a_microphone_segment_is_still_microphone_at_the_end_of_the_pipeline() {
    let stream = CaptureStreamId::next();
    let (segment, envelope) = produced(
        AudioSource::Microphone,
        stream,
        3,
        "eu acho que sim",
        0,
        1_000,
    );

    assert_eq!(segment.source, AudioSource::Microphone);
    assert_eq!(segment.source, envelope.source);
    assert_eq!(segment.capture_stream_id, stream);
    assert_eq!(segment.sequence_number, 3);

    let mut assembler = assembler();
    let events = assembler.ingest_segment(segment);
    assert_eq!(
        turn_started_origin(&events),
        Some((ConversationSpeaker::User, AudioSource::Microphone))
    );
}

#[test]
fn a_system_output_segment_is_still_system_output_at_the_end_of_the_pipeline() {
    let stream = CaptureStreamId::next();
    let (segment, envelope) = produced(
        AudioSource::SystemOutput,
        stream,
        1,
        "em qual situação você usaria monolito?",
        0,
        1_500,
    );

    assert_eq!(segment.source, AudioSource::SystemOutput);
    assert_eq!(segment.source, envelope.source);

    let mut assembler = assembler();
    let events = assembler.ingest_segment(segment);
    assert_eq!(
        turn_started_origin(&events),
        Some((ConversationSpeaker::OtherPerson, AudioSource::SystemOutput))
    );
}

#[test]
fn microphone_always_derives_the_user_speaker() {
    assert_eq!(
        speaker_for_source(AudioSource::Microphone),
        ConversationSpeaker::User
    );
    assert_eq!(
        ConversationSpeaker::from(AudioSource::Microphone),
        ConversationSpeaker::User
    );
}

#[test]
fn system_output_always_derives_the_other_person_speaker() {
    assert_eq!(
        speaker_for_source(AudioSource::SystemOutput),
        ConversationSpeaker::OtherPerson
    );
    assert_eq!(
        ConversationSpeaker::from(AudioSource::SystemOutput),
        ConversationSpeaker::OtherPerson
    );
}

// ---------------------------------------------------------------------------
// 11–13. O turno recusa o que não é dele.
// ---------------------------------------------------------------------------

#[test]
fn an_open_turn_rejects_a_segment_from_a_different_source() {
    let mut assembler = assembler();
    let mic_stream = CaptureStreamId::next();
    let system_stream = CaptureStreamId::next();

    assembler.ingest_segment(segment_of(
        AudioSource::Microphone,
        mic_stream,
        1,
        "deixa eu ver",
        0,
        800,
    ));
    let events = assembler.ingest_segment(segment_of(
        AudioSource::SystemOutput,
        system_stream,
        1,
        "e como você faria isso?",
        900,
        2_000,
    ));

    let finalized = finalized_turns(&events);
    assert_eq!(finalized.len(), 1, "o turno do microfone foi fechado");
    assert_eq!(finalized[0].source, AudioSource::Microphone);

    let open = assembler.open_turn.as_ref().expect("um turno novo abriu");
    assert_eq!(open.source, AudioSource::SystemOutput);
    assert_eq!(open.speaker, ConversationSpeaker::OtherPerson);
    assert_eq!(open.capture_stream_id, system_stream);
}

#[test]
fn an_open_turn_rejects_a_segment_from_a_different_speaker() {
    let mut assembler = assembler();
    assembler.ingest_segment(segment_of(
        AudioSource::SystemOutput,
        CaptureStreamId::next(),
        1,
        "me conta um caso real",
        0,
        1_200,
    ));
    let events = assembler.ingest_segment(segment_of(
        AudioSource::Microphone,
        CaptureStreamId::next(),
        1,
        "claro, teve um projeto",
        1_300,
        2_400,
    ));

    let finalized = finalized_turns(&events);
    assert_eq!(finalized.len(), 1);
    assert_eq!(finalized[0].speaker, ConversationSpeaker::OtherPerson);
    assert_eq!(
        assembler.open_turn.as_ref().unwrap().speaker,
        ConversationSpeaker::User
    );
}

#[test]
fn an_open_turn_rejects_another_capture_stream_of_the_same_source() {
    let mut assembler = assembler();
    let first = CaptureStreamId::next();
    let second = CaptureStreamId::next();

    assembler.ingest_segment(segment_of(
        AudioSource::Microphone,
        first,
        1,
        "testando o microfone",
        0,
        900,
    ));
    let events = assembler.ingest_segment(segment_of(
        AudioSource::Microphone,
        second,
        1,
        "agora com o outro",
        1_000,
        1_900,
    ));

    let finalized = finalized_turns(&events);
    assert_eq!(
        finalized.len(),
        1,
        "troca de dispositivo abre um turno novo em vez de misturar dois fluxos"
    );
    assert_eq!(finalized[0].capture_stream_id, first);
    assert_eq!(
        assembler.open_turn.as_ref().unwrap().capture_stream_id,
        second
    );
}

#[test]
fn a_segment_whose_source_diverges_from_its_envelope_is_rejected_not_corrected() {
    let envelope = TranscriptionResultEnvelope {
        session_id: SessionId::from_value(1),
        segment_id: SegmentId::next(),
        source: AudioSource::SystemOutput,
        capture_stream_id: CaptureStreamId::next(),
        sequence_number: 1,
    };
    // O transcritor devolveu a fonte errada. O envelope diz `SystemOutput`.
    let transcript = final_transcript(AudioSource::Microphone, "e como você faria isso?", 0, 1_000);
    let normalization = TranscriptNormalizationResult::unchanged("e como você faria isso?".into());

    let error = TranscriptSegment::from_normalized(
        &envelope,
        &transcript,
        &normalization,
        1,
        Instant::now(),
    )
    .expect_err("divergência de origem tem que virar erro");

    assert_eq!(error.expected_source, AudioSource::SystemOutput);
    assert_eq!(error.observed_source, AudioSource::Microphone);
    assert_eq!(error.segment_id, envelope.segment_id);
}

#[test]
fn a_rejected_segment_never_reaches_the_timeline() {
    let timeline = ConversationTimeline::new(ConversationAssemblerConfig::default());
    let envelope = TranscriptionResultEnvelope {
        session_id: timeline.session_id(),
        segment_id: SegmentId::next(),
        source: AudioSource::SystemOutput,
        capture_stream_id: CaptureStreamId::next(),
        sequence_number: 1,
    };
    let transcript = final_transcript(AudioSource::Microphone, "pergunta importante", 0, 1_000);
    let normalization = TranscriptNormalizationResult::unchanged("pergunta importante".into());

    let events = timeline.ingest_normalized_transcript(
        &envelope,
        &transcript,
        &normalization,
        Instant::now(),
    );

    assert!(
        events.is_empty(),
        "nenhum evento é emitido para dado rejeitado"
    );
    let snapshot = timeline.snapshot();
    assert!(snapshot.turns.is_empty());
    assert!(snapshot.utterances.is_empty());
    assert!(timeline.raw_segments().is_empty());
}

// ---------------------------------------------------------------------------
// 15. Snapshot e eventos visuais concordam.
// ---------------------------------------------------------------------------

#[test]
fn the_snapshot_agrees_with_the_events_that_were_emitted() {
    let timeline = ConversationTimeline::new(ConversationAssemblerConfig::default());
    let mut emitted = Vec::new();
    for (source, text, start, end) in [
        (AudioSource::Microphone, "boa tarde", 0u64, 600u64),
        (
            AudioSource::SystemOutput,
            "em qual situação você usaria microserviços?",
            2_600,
            4_000,
        ),
    ] {
        let stream = CaptureStreamId::next();
        let (_, envelope) = produced(source, stream, 1, text, start, end);
        let transcript = final_transcript(source, text, start, end);
        let normalization = TranscriptNormalizationResult::unchanged(text.into());
        emitted.extend(timeline.ingest_normalized_transcript(
            &envelope,
            &transcript,
            &normalization,
            Instant::now(),
        ));
    }

    let snapshot = timeline.snapshot();
    for event in &emitted {
        if let ConversationTimelineEvent::UtteranceStarted {
            utterance_id,
            speaker,
            source,
            ..
        } = event
        {
            let same = snapshot
                .utterances
                .iter()
                .find(|u| u.id == *utterance_id)
                .expect("a utterance emitida existe no snapshot");
            assert_eq!(same.source, *source, "evento visual e snapshot discordaram");
            assert_eq!(same.speaker, *speaker);
        }
    }
    for utterance in &snapshot.utterances {
        assert_eq!(utterance.speaker, speaker_for_source(utterance.source));
    }
    for turn in &snapshot.turns {
        assert_eq!(turn.speaker, speaker_for_source(turn.source));
        for utterance in utterances_of(&snapshot, turn) {
            assert_eq!(
                utterance.source, turn.source,
                "nenhum turno contém utterance de outra fonte"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 16–19. Diagnóstico cruzado: classifica, nunca reescreve.
// ---------------------------------------------------------------------------

#[test]
fn an_acoustic_duplicate_is_diagnosed_without_becoming_user_speech() {
    let remote = segment_of(
        AudioSource::SystemOutput,
        CaptureStreamId::next(),
        1,
        "em qual situação você escolheria usar monolitos?",
        0,
        2_000,
    );
    let leaked = segment_of(
        AudioSource::Microphone,
        CaptureStreamId::next(),
        1,
        "em qual situação você escolheria usar monolitos",
        120,
        2_100,
    );

    let diagnosis = diagnose_cross_source(
        &remote.cross_source_candidate(),
        &leaked.cross_source_candidate(),
        CrossSourceConfig::default(),
    );
    assert_eq!(diagnosis, CrossSourceDiagnosis::ProbableAcousticLeak);

    // O diagnóstico não muda o segmento: o que foi capturado pelo microfone continua sendo
    // do microfone, e é isso que impede a "correção" de virar fala atribuída a quem não
    // falou. A supressão só entraria com `TranscriptSegmentOrigin::ProbableSystemAudioLeak`,
    // e não está ligada — ver o relatório.
    assert_eq!(leaked.source, AudioSource::Microphone);
    assert_eq!(leaked.speaker, ConversationSpeaker::User);
    assert_eq!(leaked.origin, TranscriptSegmentOrigin::Live);
}

#[test]
fn genuinely_simultaneous_speech_is_not_diagnosed_as_a_leak() {
    let remote = segment_of(
        AudioSource::SystemOutput,
        CaptureStreamId::next(),
        1,
        "então a gente precisa decidir a arquitetura",
        0,
        2_000,
    );
    let mine = segment_of(
        AudioSource::Microphone,
        CaptureStreamId::next(),
        1,
        "sim, eu concordo com isso",
        100,
        1_800,
    );

    assert_eq!(
        diagnose_cross_source(
            &remote.cross_source_candidate(),
            &mine.cross_source_candidate(),
            CrossSourceConfig::default(),
        ),
        CrossSourceDiagnosis::IndependentSpeech
    );
}

#[test]
fn low_similarity_is_never_treated_as_duplicate() {
    let remote = segment_of(
        AudioSource::SystemOutput,
        CaptureStreamId::next(),
        1,
        "me conta um caso real de monolito",
        0,
        2_000,
    );
    let mine = segment_of(
        AudioSource::Microphone,
        CaptureStreamId::next(),
        1,
        "me conta outra coisa qualquer sobre kubernetes",
        50,
        2_050,
    );

    assert_eq!(
        diagnose_cross_source(
            &remote.cross_source_candidate(),
            &mine.cross_source_candidate(),
            CrossSourceConfig::default(),
        ),
        CrossSourceDiagnosis::IndependentSpeech
    );
}

#[test]
fn the_same_sentence_far_apart_in_time_is_not_an_echo() {
    let text = "em qual situação você escolheria usar microserviços?";
    let first = segment_of(
        AudioSource::SystemOutput,
        CaptureStreamId::next(),
        1,
        text,
        0,
        2_000,
    );
    let much_later = segment_of(
        AudioSource::Microphone,
        CaptureStreamId::next(),
        1,
        text,
        60_000,
        62_000,
    );

    assert_eq!(
        diagnose_cross_source(
            &first.cross_source_candidate(),
            &much_later.cross_source_candidate(),
            CrossSourceConfig::default(),
        ),
        CrossSourceDiagnosis::IndependentSpeech,
        "eco é um fenômeno de milissegundos; a mesma frase um minuto depois é fala nova"
    );
}

// ---------------------------------------------------------------------------
// 10 e 20. Sessão e o defeito real.
// ---------------------------------------------------------------------------

#[test]
fn a_new_session_does_not_reuse_state_from_the_previous_one() {
    let timeline = ConversationTimeline::new(ConversationAssemblerConfig::default());
    let text = "pergunta da sessão anterior";
    let (_, envelope) = produced(
        AudioSource::SystemOutput,
        CaptureStreamId::next(),
        1,
        text,
        0,
        1_500,
    );
    let transcript = final_transcript(AudioSource::SystemOutput, text, 0, 1_500);
    let normalization = TranscriptNormalizationResult::unchanged(text.into());
    timeline.ingest_normalized_transcript(&envelope, &transcript, &normalization, Instant::now());
    assert!(!timeline.raw_segments().is_empty());

    let previous_session = timeline.session_id();
    timeline.start_session();
    assert_ne!(timeline.session_id(), previous_session);

    assert!(timeline.raw_segments().is_empty());
    let snapshot = timeline.snapshot();
    assert!(snapshot.turns.is_empty());
    assert!(snapshot.utterances.is_empty());
}

/// **O defeito relatado**, reproduzido com as três falas exatas da sessão real.
///
/// O usuário falava, parava, e a utterance do microfone finalizava sozinha pelo timer
/// dedicado — que por contrato deixa o *turno* aberto. As falas seguintes, vindas da saída
/// de sistema, encontravam `open_utterance == None` e entravam pelo atalho de
/// `ingest_segment`, que ia direto para `start_utterance_and_maybe_turn` sem passar por
/// `segment_decision`, a única comparação de speaker/source que existia. Resultado: turno do
/// microfone com onze utterances da outra pessoa, `is_eligible_turn` falso, nenhuma geração.
#[test]
fn the_reported_bug_system_output_questions_never_land_in_a_microphone_turn() {
    let mut assembler = assembler();
    let mic_stream = CaptureStreamId::next();
    let system_stream = CaptureStreamId::next();

    // O usuário fala e para.
    assembler.ingest_segment(segment_of(
        AudioSource::Microphone,
        mic_stream,
        1,
        "então a arquitetura ficou assim",
        0,
        2_000,
    ));
    let (utterance_id, revision) = assembler
        .open_utterance_identity()
        .expect("a fala do usuário abriu uma utterance");
    // Exatamente o que o timer dedicado faz: finaliza a utterance por silêncio e **deixa o
    // turno do microfone aberto**.
    assembler.finalize_utterance_if_stale(utterance_id, revision);
    assert!(assembler.open_utterance.is_none());
    assert!(
        assembler.open_turn.is_some(),
        "o turno segue aberto por contrato — é esse estado que expunha o defeito"
    );

    let questions = [
        "Em qual situação você escolheria usar monolitos?",
        "Em qual situação você escolheria usar microserviços?",
        "Perfeito. Me conta um caso real em que você optou por usar monólito.",
    ];

    let mut eligible_turns = Vec::new();
    let mut start = 3_000u64;
    for (index, question) in questions.iter().enumerate() {
        let events = assembler.ingest_segment(segment_of(
            AudioSource::SystemOutput,
            system_stream,
            index as u64 + 1,
            question,
            start,
            start + 2_000,
        ));

        for event in &events {
            match event {
                ConversationTimelineEvent::UtteranceStarted {
                    speaker, source, ..
                }
                | ConversationTimelineEvent::TurnStarted {
                    speaker, source, ..
                } => {
                    assert_eq!(
                        *source,
                        AudioSource::SystemOutput,
                        "a fala da outra pessoa nunca vira fala do microfone"
                    );
                    assert_eq!(*speaker, ConversationSpeaker::OtherPerson);
                }
                _ => {}
            }
        }

        let open = assembler.open_turn.as_ref().expect("um turno está aberto");
        assert_eq!(
            open.source,
            AudioSource::SystemOutput,
            "o turno aberto tem que ser da saída de sistema, não do microfone"
        );
        assert_eq!(open.speaker, ConversationSpeaker::OtherPerson);
        assert_eq!(open.capture_stream_id, system_stream);
        assert!(
            is_eligible_turn(open),
            "um turno com a pergunta da outra pessoa tem que ser elegível para geração"
        );
        eligible_turns.push(open.id);

        // Silêncio entre perguntas: a utterance fecha pelo timer, o turno remoto continua.
        let (id, rev) = assembler
            .open_utterance_identity()
            .expect("utterance aberta");
        assembler.finalize_utterance_if_stale(id, rev);
        start += 25_000;
    }

    assert_eq!(
        eligible_turns.len(),
        questions.len(),
        "cada pergunta produziu um turno elegível"
    );

    // E o turno do microfone, fechado no começo de tudo, não recebeu nenhuma delas.
    let snapshot = assembler.snapshot();
    for turn in &snapshot.turns {
        for utterance in utterances_of(&snapshot, turn) {
            assert_eq!(
                utterance.source, turn.source,
                "turno {:?} misturou fontes",
                turn.id
            );
            assert_eq!(utterance.speaker, turn.speaker);
        }
    }
    let mic_turns: Vec<_> = snapshot
        .turns
        .iter()
        .filter(|turn| turn.source == AudioSource::Microphone)
        .collect();
    assert_eq!(mic_turns.len(), 1);
    assert_eq!(mic_turns[0].utterances.len(), 1);
    assert!(!is_eligible_turn(mic_turns[0]));
}
