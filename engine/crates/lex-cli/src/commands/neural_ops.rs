use std::path::Path;
use std::process;
use std::time::Instant;

use lex_core::converter::{convert, convert_nbest, convert_nbest_with_history};
use lex_core::dict::connection::ConnectionMatrix;
use lex_core::dict::TrieDictionary;
use lex_core::neural::NeuralScorer;
use lex_core::user_history::UserHistory;

macro_rules! die {
    ($result:expr, $($arg:tt)*) => {
        $result.unwrap_or_else(|e| {
            eprintln!($($arg)*, e);
            process::exit(1);
        })
    };
}

pub fn neural_score_cmd(
    dict_file: &str,
    conn_file: &str,
    model_file: &str,
    kana: &str,
    n: usize,
    history: Option<&str>,
    context: &str,
) {
    let dict = die!(
        TrieDictionary::open(Path::new(dict_file)),
        "Error opening dictionary: {}"
    );
    let conn = die!(
        ConnectionMatrix::open(Path::new(conn_file)),
        "Error opening connection matrix: {}"
    );

    let user_history = history.map(|path| {
        die!(
            UserHistory::open(Path::new(path)),
            "Error opening history: {}"
        )
    });

    // 1) Viterbi N-best
    let viterbi_start = Instant::now();
    let nbest = if let Some(ref h) = user_history {
        convert_nbest_with_history(&dict, Some(&conn), h, kana, n)
    } else {
        convert_nbest(&dict, Some(&conn), kana, n)
    };
    let viterbi_elapsed = viterbi_start.elapsed();

    println!("Input: {kana}");
    if !context.is_empty() {
        println!("Context: {context}");
    } else {
        println!("Context: (none)");
    }
    println!();

    println!("Viterbi N-best (before neural):");
    for (i, path) in nbest.iter().enumerate() {
        let surface: String = path.iter().map(|s| s.surface.as_str()).collect();
        println!("#{:>2}: {surface}", i + 1);
    }
    println!();

    if nbest.is_empty() {
        eprintln!("No Viterbi candidates to score.");
        return;
    }

    // 2) Load neural model
    eprintln!("Loading neural model from {model_file}...");
    let model_start = Instant::now();
    let mut scorer = die!(
        NeuralScorer::open(Path::new(model_file)),
        "Error loading neural model: {}"
    );
    let model_elapsed = model_start.elapsed();
    eprintln!("  Model loaded in {:.0}ms", model_elapsed.as_millis());
    eprintln!("  {}", scorer.config_summary());

    // 3) Neural scoring
    let neural_start = Instant::now();
    let scores = die!(
        scorer.score_paths(context, kana, &nbest),
        "Error scoring paths: {}"
    );
    let neural_elapsed = neural_start.elapsed();

    println!("Neural re-scored:");
    for (rank, (path_idx, log_prob)) in scores.iter().enumerate() {
        let path = &nbest[*path_idx];
        let surface: String = path.iter().map(|s| s.surface.as_str()).collect();
        println!(
            "#{:>2}: {surface}  (log_prob: {log_prob:.2}, viterbi_rank: {})",
            rank + 1,
            path_idx + 1
        );
    }
    println!();
    println!(
        "Latency: {:.0}ms (viterbi: {:.1}ms, neural: {:.0}ms, model_load: {:.0}ms)",
        (viterbi_elapsed + neural_elapsed).as_millis(),
        viterbi_elapsed.as_secs_f64() * 1000.0,
        neural_elapsed.as_millis(),
        model_elapsed.as_millis(),
    );
}

pub fn generate_cmd(model_file: &str, context: &str, max_tokens: usize) {
    use lex_core::neural::GenerateConfig;

    println!(
        "Context: {}",
        if context.is_empty() {
            "(none)"
        } else {
            context
        }
    );

    eprintln!("Loading neural model from {model_file}...");
    let model_start = Instant::now();
    let mut scorer = die!(
        NeuralScorer::open(Path::new(model_file)),
        "Error loading neural model: {}"
    );
    let model_elapsed = model_start.elapsed();
    eprintln!("  Model loaded in {:.0}ms", model_elapsed.as_millis());
    eprintln!("  {}", scorer.config_summary());

    let config = GenerateConfig {
        max_tokens,
        ..GenerateConfig::default()
    };

    let gen_start = Instant::now();
    let text = die!(
        scorer.generate_text(context, &config),
        "Error generating text: {}"
    );
    let gen_elapsed = gen_start.elapsed();

    println!("Generated: {text}");
    println!(
        "Latency: {:.0}ms (model_load: {:.0}ms)",
        gen_elapsed.as_millis(),
        model_elapsed.as_millis(),
    );
}

#[allow(clippy::too_many_arguments)]
pub fn speculative_decode_cmd(
    dict_file: &str,
    conn_file: &str,
    model_file: &str,
    kana: &str,
    context: &str,
    threshold: f64,
    max_iter: usize,
    compare: bool,
) {
    use lex_core::neural::speculative::{speculative_decode, SpeculativeConfig};

    let dict = die!(
        TrieDictionary::open(Path::new(dict_file)),
        "Error opening dictionary: {}"
    );
    let conn = die!(
        ConnectionMatrix::open(Path::new(conn_file)),
        "Error opening connection matrix: {}"
    );

    println!("Input: {kana}");
    if !context.is_empty() {
        println!("Context: {context}");
    } else {
        println!("Context: (none)");
    }
    println!();

    // Load neural model
    eprintln!("Loading neural model from {model_file}...");
    let model_start = Instant::now();
    let mut scorer = die!(
        NeuralScorer::open(Path::new(model_file)),
        "Error loading neural model: {}"
    );
    let model_elapsed = model_start.elapsed();
    eprintln!("  Model loaded in {:.0}ms", model_elapsed.as_millis());
    eprintln!("  {}", scorer.config_summary());

    // Compare mode: show Viterbi 1-best and N-best reranking
    if compare {
        let viterbi_start = Instant::now();
        let viterbi_1best = convert(&dict, Some(&conn), kana);
        let viterbi_elapsed = viterbi_start.elapsed();
        let viterbi_surface: String = viterbi_1best.iter().map(|s| s.surface.as_str()).collect();
        println!(
            "Viterbi 1-best:  {viterbi_surface}  ({:.1}ms)",
            viterbi_elapsed.as_secs_f64() * 1000.0
        );

        let nbest = convert_nbest(&dict, Some(&conn), kana, 10);
        let neural_start = Instant::now();
        let scores = die!(
            scorer.score_paths(context, kana, &nbest),
            "Error scoring paths: {}"
        );
        let neural_elapsed = neural_start.elapsed();

        if let Some(&(best_idx, _)) = scores.first() {
            let rerank_surface: String =
                nbest[best_idx].iter().map(|s| s.surface.as_str()).collect();
            println!(
                "N-best rerank:   {rerank_surface}  ({:.0}ms neural)",
                neural_elapsed.as_millis()
            );
        }
        println!();
    }

    // Speculative decoding
    let config = SpeculativeConfig {
        threshold,
        max_iterations: max_iter,
        ..SpeculativeConfig::default()
    };
    let result = die!(
        speculative_decode(&mut scorer, &dict, Some(&conn), context, kana, &config),
        "Error in speculative decoding: {}"
    );

    let surface: String = result.segments.iter().map(|s| s.surface.as_str()).collect();
    println!("Speculative:     {surface}");
    println!("Segments:");
    for (i, (seg, &score)) in result
        .segments
        .iter()
        .zip(result.segment_scores.iter())
        .enumerate()
    {
        let status = if score >= threshold { "OK" } else { "LOW" };
        println!(
            "  [{i}] {}({})      score={:.2}/char  {status}",
            seg.surface, seg.reading, score
        );
    }
    println!();
    println!("Iterations: {}", result.metadata.iterations);
    println!("Confirmed: {:?}", result.metadata.confirmed_counts);
    println!("Converged: {}", result.metadata.converged);
    if result.metadata.fell_back {
        println!(
            "Fell back to N-best reranking (< {} segments)",
            config.min_segments
        );
    }
    println!(
        "Latency: {:.0}ms (viterbi: {:.1}ms, neural: {:.0}ms, model_load: {:.0}ms)",
        result.metadata.total_latency.as_millis(),
        result.metadata.viterbi_latency.as_secs_f64() * 1000.0,
        result.metadata.neural_latency.as_millis(),
        model_elapsed.as_millis(),
    );
}
