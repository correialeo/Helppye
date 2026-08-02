//! Escrita do relatório. Dois formatos porque servem a leitores diferentes: o JSON preserva
//! tudo (inclusive os textos e a lista de erros) para inspeção; o CSV é a forma que abre numa
//! planilha e permite comparar providers linha a linha sem ferramenta nenhuma.
//!
//! Ambos vão para um diretório fora do Git. O CSV contém a transcrição normalizada, que é
//! conteúdo de reunião — o mesmo motivo pelo qual telemetria não grava texto por padrão vale
//! aqui: um relatório de benchmark não pode virar um vazamento de fala commitado por
//! distração.

use std::path::Path;

use crate::benchmark::runner::BenchmarkCaseResult;

#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    #[error("não foi possível escrever {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("falha ao serializar relatório: {0}")]
    Serialize(#[from] serde_json::Error),
}

pub fn write_json(path: &Path, results: &[BenchmarkCaseResult]) -> Result<(), ReportError> {
    let json = serde_json::to_string_pretty(results)?;
    write(path, &json)
}

const CSV_HEADER: &str = "fixture_id,provider,model,language,source,audio_duration_ms,\
 first_partial_ms,first_final_ms,total_ms,real_time_factor,time_to_first_partial_ms,\
 time_to_final_transcript_ms,speech_end_to_final_ms,partial_revision_count,\
 provider_queue_wait_ms,websocket_send_latency_ms,pacing,word_error_rate,vocabulary_hits,\
vocabulary_misses,mean_confidence,final_transcripts,partial_transcripts,\
normalization_changes,utterances,errors,estimated_cost_usd,normalized_transcript";

pub fn write_csv(path: &Path, results: &[BenchmarkCaseResult]) -> Result<(), ReportError> {
    let mut out = String::from(CSV_HEADER);
    out.push('\n');
    for r in results {
        let row = [
            r.fixture_id.clone(),
            r.provider.clone(),
            r.model.clone().unwrap_or_default(),
            r.language.clone(),
            format!("{:?}", r.source),
            r.latencies.audio_duration_ms.to_string(),
            optional(r.latencies.first_partial_ms),
            optional(r.latencies.first_final_ms),
            r.latencies.total_ms.to_string(),
            format!("{:.3}", r.latencies.real_time_factor),
            optional(r.latencies.time_to_first_partial_ms),
            optional(r.latencies.time_to_final_transcript_ms),
            optional(r.latencies.speech_end_to_final_ms),
            r.partial_revision_count.to_string(),
            optional(r.latencies.provider_queue_wait_ms),
            optional(r.latencies.websocket_send_latency_ms),
            format!("{:?}", r.pacing),
            format!("{:.3}", r.word_error_rate),
            r.vocabulary_hits.join("; "),
            r.vocabulary_misses.join("; "),
            r.mean_confidence
                .map(|c| format!("{c:.3}"))
                .unwrap_or_default(),
            r.final_transcript_count.to_string(),
            r.partial_transcript_count.to_string(),
            r.normalization_change_count.to_string(),
            r.utterance_count.to_string(),
            r.errors.join("; "),
            r.estimated_cost_usd
                .map(|c| format!("{c:.6}"))
                .unwrap_or_default(),
            r.normalized_transcript.clone(),
        ]
        .iter()
        .map(|field| escape_csv(field))
        .collect::<Vec<_>>()
        .join(",");
        out.push_str(&row);
        out.push('\n');
    }
    write(path, &out)
}

fn write(path: &Path, contents: &str) -> Result<(), ReportError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| ReportError::Io {
            path: parent.display().to_string(),
            source,
        })?;
    }
    std::fs::write(path, contents).map_err(|source| ReportError::Io {
        path: path.display().to_string(),
        source,
    })
}

fn optional(value: Option<u64>) -> String {
    value.map(|v| v.to_string()).unwrap_or_default()
}

/// Escapa aspas e envolve o campo quando ele contém separador, aspas ou quebra de linha. Uma
/// transcrição quase sempre tem vírgula; sem isso o CSV sai com colunas deslocadas
/// exatamente nos casos mais interessantes.
fn escape_csv(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::types::AudioSource;
    use crate::benchmark::runner::BenchmarkLatencies;

    fn result() -> BenchmarkCaseResult {
        BenchmarkCaseResult {
            fixture_id: "caso-1".into(),
            provider: "whisper_local".into(),
            model: None,
            language: "pt".into(),
            source: AudioSource::SystemOutput,
            latencies: BenchmarkLatencies {
                audio_duration_ms: 4000,
                first_partial_ms: None,
                first_final_ms: Some(900),
                total_ms: 1200,
                real_time_factor: 0.3,
                time_to_first_partial_ms: None,
                time_to_final_transcript_ms: Some(900),
                speech_end_to_final_ms: Some(300),
                provider_queue_wait_ms: Some(0),
                websocket_send_latency_ms: None,
            },
            pacing: crate::benchmark::runner::BenchmarkPacing::Instant,
            expected_transcript: "usamos DDD".into(),
            raw_transcript: "usamos ddd".into(),
            normalized_transcript: "Usamos DDD, e microserviços".into(),
            word_error_rate: 0.5,
            vocabulary_hits: vec!["DDD".into()],
            vocabulary_misses: vec![],
            mean_confidence: None,
            final_transcript_count: 1,
            partial_transcript_count: 0,
            partial_revision_count: 0,
            normalization_change_count: 2,
            utterance_count: 1,
            errors: vec![],
            estimated_cost_usd: Some(0.0),
        }
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "helppye-benchmark-report-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn csv_quotes_fields_containing_separators() {
        let path = temp_path("csv");
        write_csv(&path, &[result()]).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();

        assert_eq!(lines[0], CSV_HEADER);
        assert!(
            lines[1].contains("\"Usamos DDD, e microserviços\""),
            "{}",
            lines[1]
        );
        assert_eq!(lines.len(), 2);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn absent_measurements_stay_empty_instead_of_becoming_zero() {
        let path = temp_path("empty");
        write_csv(&path, &[result()]).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        let row = contents.lines().nth(1).unwrap();
        let fields: Vec<&str> = row.split(',').collect();
        // `first_partial_ms` é a sétima coluna e não foi medida neste caso.
        assert_eq!(fields[6], "", "{row}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn json_round_trips_the_full_result() {
        let path = temp_path("json");
        write_json(&path, &[result()]).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("\"fixture_id\": \"caso-1\""));
        assert!(contents.contains("\"raw_transcript\""));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn creates_the_output_directory_when_missing() {
        let dir = temp_path("dir");
        let path = dir.join("nested").join("results.json");
        write_json(&path, &[]).unwrap();
        assert!(path.is_file());
        std::fs::remove_dir_all(&dir).ok();
    }
}
