//! Executável do harness de benchmark de transcrição.
//!
//! ```bash
//! cargo run --bin benchmark -- \
//!   --manifest ../benchmarks/fixtures.json \
//!   --provider whisper_local \
//!   --model ~/.local/share/helppye/models/ggml-base.bin \
//!   --out ../benchmarks/results
//! ```
//!
//! `--model` é obrigatório para o Whisper local e opcional para providers remotos. O
//! provider fake existe só dentro dos testes
//! (`#[cfg(test)]`) — ele devolve o texto que lhe mandaram, então rodar o harness contra ele
//! produziria um relatório com WER perfeito que não diz nada sobre transcritor nenhum.
//!
//! Parsing de argumentos à mão em vez de um crate de CLI: são quatro flags, e uma dependência
//! nova entraria no binário do app junto.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use helppye_lib::audio::types::AudioSource;
use helppye_lib::benchmark::{
    run_fixture, write_csv, write_json, BenchmarkCaseResult, CostModel, FixtureManifest,
};
use helppye_lib::normalization::{DeterministicNormalizer, TranscriptionVocabulary};
use helppye_lib::transcription::provider::{TranscriptionProvider, TranscriptionProviderId};
use helppye_lib::transcription::segment_transcriber::SegmentTranscriber;
use helppye_lib::transcription::settings::{LanguageCode, TranscriptionSettings};
use helppye_lib::transcription::types::{InferenceDevice, ModelConfig};
use helppye_lib::transcription::whisper_provider::WhisperCppProvider;

struct Args {
    manifest: PathBuf,
    out_dir: PathBuf,
    provider: TranscriptionProviderId,
    model: Option<PathBuf>,
    cost: CostModel,
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("não foi possível iniciar o runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    match runtime.block_on(run(&args)) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

const USAGE: &str = "uso: benchmark --manifest <fixtures.json> [--out <dir>] \
[--provider <provider-id>] [--model <modelo-ou-ggml.bin>] \
[--usd-per-audio-minute <preço>]";

fn parse_args() -> Result<Args, String> {
    let mut manifest = None;
    let mut out_dir = PathBuf::from("benchmarks/results");
    let mut provider = TranscriptionProviderId::WhisperLocal;
    let mut model = None;
    let mut cost = CostModel::default();

    let mut argv = std::env::args().skip(1);
    while let Some(flag) = argv.next() {
        let mut value = || argv.next().ok_or(format!("{flag} exige um valor"));
        match flag.as_str() {
            "--manifest" => manifest = Some(PathBuf::from(value()?)),
            "--out" => out_dir = PathBuf::from(value()?),
            "--provider" => provider = value()?.parse()?,
            "--model" => model = Some(PathBuf::from(value()?)),
            "--usd-per-audio-minute" => {
                let raw = value()?;
                cost.usd_per_audio_minute =
                    Some(raw.parse().map_err(|_| format!("preço inválido: {raw}"))?);
            }
            "-h" | "--help" => return Err(USAGE.to_string()),
            other => return Err(format!("argumento desconhecido: {other}")),
        }
    }

    Ok(Args {
        manifest: manifest.ok_or("--manifest é obrigatório")?,
        out_dir,
        provider,
        model,
        cost,
    })
}

async fn run(args: &Args) -> Result<ExitCode, String> {
    let manifest = FixtureManifest::load(&args.manifest).map_err(|e| e.to_string())?;
    if manifest.fixtures.is_empty() {
        return Err("o manifesto não tem nenhum fixture".to_string());
    }

    let (provider, settings, cost) = build_provider(args).await?;
    let normalizer = DeterministicNormalizer::new(TranscriptionVocabulary::default());

    let mut results: Vec<BenchmarkCaseResult> = Vec::new();
    for fixture in &manifest.fixtures {
        let audio = FixtureManifest::audio_path(&args.manifest, fixture);
        let settings = TranscriptionSettings {
            language: if provider.capabilities().language_selection {
                fixture.language.clone()
            } else {
                settings.language.clone()
            },
            ..settings.clone()
        };
        match run_fixture(
            provider.as_ref(),
            &settings,
            &normalizer,
            fixture,
            &audio,
            cost,
        )
        .await
        {
            Ok(result) => {
                print_summary(&result);
                results.push(result);
            }
            // Um fixture que falha não interrompe os demais: o valor do harness é a
            // comparação, e abortar no primeiro erro devolveria uma tabela vazia.
            Err(e) => eprintln!("fixture '{}' falhou: {e}", fixture.id),
        }
    }

    let stamp = timestamp();
    let json = args.out_dir.join(format!("benchmark-{stamp}.json"));
    let csv = args.out_dir.join(format!("benchmark-{stamp}.csv"));
    write_json(&json, &results).map_err(|e| e.to_string())?;
    write_csv(&csv, &results).map_err(|e| e.to_string())?;

    println!(
        "\n{} caso(s) → {}\n{}",
        results.len(),
        json.display(),
        csv.display()
    );
    Ok(if results.len() == manifest.fixtures.len() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

async fn build_provider(
    args: &Args,
) -> Result<
    (
        Arc<dyn TranscriptionProvider>,
        TranscriptionSettings,
        CostModel,
    ),
    String,
> {
    let model_path =
        if args.provider == TranscriptionProviderId::WhisperLocal {
            Some(args.model.clone().ok_or(
                "--model é obrigatório: o Whisper local precisa de um .bin do whisper.cpp",
            )?)
        } else {
            args.model.clone()
        };

    let whisper = Arc::new(WhisperCppProvider::new());
    if args.provider == TranscriptionProviderId::WhisperLocal {
        let model_path = model_path.as_ref().expect("validated above");
        whisper
            .load(ModelConfig {
                model_path: model_path.clone(),
                model_name: file_name(model_path),
                language: LanguageCode::default().into(),
                device: InferenceDevice::Cpu,
            })
            .await
            .map_err(|e| format!("não foi possível carregar o modelo: {e}"))?;
    }

    let registry = helppye_lib::transcription::build_provider_registry(whisper);
    let provider = registry
        .get(args.provider)
        .map_err(|error| error.to_string())?;
    provider
        .readiness()
        .await
        .map_err(|error| error.to_string())?;
    let capabilities = provider.capabilities();

    let settings = TranscriptionSettings {
        provider: args.provider,
        language: if capabilities.automatic_language_detection {
            LanguageCode::Automatic
        } else {
            LanguageCode::default()
        },
        model: model_path.map(|model| model.display().to_string()),
        ..TranscriptionSettings::default()
    };
    let cost = if args.cost.usd_per_audio_minute.is_some() || !capabilities.local {
        args.cost
    } else {
        CostModel::FREE_LOCAL
    };
    Ok((provider, settings, cost))
}

fn print_summary(result: &BenchmarkCaseResult) {
    let source = match result.source {
        AudioSource::Microphone => "mic",
        AudioSource::SystemOutput => "sistema",
    };
    println!(
        "{:<24} {:<16} {source:<8} rtf={:<6.2} wer={:<6.3} utterances={} termos_perdidos={}",
        result.fixture_id,
        result.provider,
        result.latencies.real_time_factor,
        result.word_error_rate,
        result.utterance_count,
        result.vocabulary_misses.len(),
    );
    for error in &result.errors {
        println!("    erro: {error}");
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

/// Segundos desde a época, só para dar nome único ao arquivo de saída. Não é medida de
/// latência — essas são todas monotônicas, dentro do runner.
fn timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
