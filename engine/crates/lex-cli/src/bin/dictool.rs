use std::path::Path;

use clap::{Parser, Subcommand};

use lex_cli::commands::{config_ops, convert_ops, dict_ops, user_dict_ops};

#[derive(Parser)]
#[command(name = "dictool", about = "Lexime dictionary build tool")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download raw dictionary files
    Fetch {
        /// Dictionary source
        #[arg(long, default_value = "mozc")]
        source: String,
        /// Output directory
        output_dir: String,
    },
    /// Compile dictionary from raw files
    Compile {
        /// Dictionary source
        #[arg(long, default_value = "mozc")]
        source: String,
        /// Input directory
        input_dir: String,
        /// Output file
        output_file: String,
    },
    /// Compile connection matrix
    CompileConn {
        /// Input text file
        input_txt: String,
        /// Output binary file
        output_file: String,
        /// Mozc id.def for function-word range extraction
        #[arg(long)]
        id_def: Option<String>,
    },
    /// Show dictionary or connection matrix info (auto-detected by magic bytes)
    Info {
        /// Dictionary (.dict) or connection matrix (.conn) file
        file: String,
    },
    /// Merge two dictionaries
    Merge {
        /// Maximum cost to keep
        #[arg(long)]
        max_cost: Option<i16>,
        /// Maximum reading length (in characters)
        #[arg(long)]
        max_reading_len: Option<usize>,
        /// First dictionary
        dict_a: String,
        /// Second dictionary
        dict_b: String,
        /// Output file
        output_file: String,
    },
    /// Show diff between two dictionaries
    Diff {
        /// First dictionary
        dict_a: String,
        /// Second dictionary
        dict_b: String,
    },
    /// Look up a reading in the dictionary (exact match)
    Lookup {
        /// Dictionary file
        dict_file: String,
        /// Reading to look up (hiragana)
        reading: String,
    },
    /// Common-prefix search (all readings that are prefixes of the query)
    Prefix {
        /// Dictionary file
        dict_file: String,
        /// Query string (hiragana)
        query: String,
    },
    /// Convert kana to kanji (N-best)
    Convert {
        /// Dictionary file
        dict_file: String,
        /// Connection matrix file
        conn_file: String,
        /// Kana input
        kana: String,
        /// Number of candidates
        #[arg(short, long, default_value = "10")]
        n: usize,
        /// User history file (optional)
        #[arg(long)]
        history: Option<String>,
    },
    /// Look up connection cost between POS IDs
    ConnCost {
        /// Connection matrix file
        conn_file: String,
        /// Left POS ID (right_id of previous morpheme)
        left: u16,
        /// Right POS ID (left_id of next morpheme)
        right: u16,
    },
    /// Export default romaji mappings as TOML
    RomajiExport,
    /// Validate a custom romaji TOML file
    RomajiValidate {
        /// Path to the TOML file
        file: String,
    },
    /// Export default settings as TOML
    SettingsExport,
    /// Validate a custom settings TOML file
    SettingsValidate {
        /// Path to the TOML file
        file: String,
    },
    /// Manage user dictionary
    UserDict {
        /// User dictionary file (default: ~/Library/Application Support/Lexime/user_dict.lxuw)
        #[arg(long)]
        file: Option<String>,
        #[command(subcommand)]
        action: UserDictAction,
    },
    /// Score N-best candidates with neural model (requires --features neural)
    #[cfg(feature = "neural")]
    NeuralScore {
        /// Dictionary file
        dict_file: String,
        /// Connection matrix file
        conn_file: String,
        /// GGUF model file path
        #[arg(long)]
        model: String,
        /// Kana input
        kana: String,
        /// Number of N-best candidates
        #[arg(short, long, default_value = "10")]
        n: usize,
        /// User history file (optional)
        #[arg(long)]
        history: Option<String>,
        /// Left context for scoring
        #[arg(long, default_value = "")]
        context: String,
    },
    /// Generate text continuation with neural model (requires --features neural)
    #[cfg(feature = "neural")]
    Generate {
        /// GGUF model file path
        #[arg(long)]
        model: String,
        /// Left context for generation
        #[arg(long, default_value = "")]
        context: String,
        /// Maximum tokens to generate
        #[arg(long, default_value = "30")]
        max_tokens: usize,
    },
    /// Speculative decoding: Viterbi draft + GPT-2 verify (requires --features neural)
    #[cfg(feature = "neural")]
    SpeculativeDecode {
        /// Dictionary file
        dict_file: String,
        /// Connection matrix file
        conn_file: String,
        /// GGUF model file path
        #[arg(long)]
        model: String,
        /// Kana input
        kana: String,
        /// Left context for scoring
        #[arg(long, default_value = "")]
        context: String,
        /// Per-char log-prob confidence threshold
        #[arg(long, default_value = "-2.0")]
        threshold: f64,
        /// Maximum verify-refine iterations
        #[arg(long, default_value = "3")]
        max_iter: usize,
        /// Compare Viterbi / N-best reranking / speculative results
        #[arg(long)]
        compare: bool,
    },
}

#[derive(Subcommand)]
enum UserDictAction {
    /// Add a word
    Add {
        /// Reading (hiragana)
        reading: String,
        /// Surface form (kanji/kana)
        surface: String,
    },
    /// Remove a word
    Remove {
        /// Reading (hiragana)
        reading: String,
        /// Surface form (kanji/kana)
        surface: String,
    },
    /// List all registered words
    List,
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::Fetch {
            source, output_dir, ..
        } => dict_ops::fetch(&source, &output_dir),
        Command::Compile {
            source,
            input_dir,
            output_file,
        } => dict_ops::compile(&source, &input_dir, &output_file),
        Command::CompileConn {
            input_txt,
            output_file,
            id_def,
        } => dict_ops::compile_conn(&input_txt, &output_file, id_def.as_deref()),
        Command::Info { file } => dict_ops::info(&file),
        Command::Merge {
            max_cost,
            max_reading_len,
            dict_a,
            dict_b,
            output_file,
        } => {
            let opts = dict_ops::MergeOptions {
                max_cost,
                max_reading_len,
            };
            dict_ops::merge(&dict_a, &dict_b, &output_file, &opts);
        }
        Command::Diff { dict_a, dict_b } => dict_ops::diff(&dict_a, &dict_b),
        Command::Lookup { dict_file, reading } => dict_ops::lookup(&dict_file, &reading),
        Command::Prefix { dict_file, query } => dict_ops::prefix(&dict_file, &query),
        Command::Convert {
            dict_file,
            conn_file,
            kana,
            n,
            history,
        } => convert_ops::convert_cmd(&dict_file, &conn_file, &kana, n, history.as_deref()),
        Command::ConnCost {
            conn_file,
            left,
            right,
        } => convert_ops::conn_cost_cmd(&conn_file, left, right),
        Command::RomajiExport => config_ops::romaji_export(),
        Command::RomajiValidate { file } => config_ops::romaji_validate(&file),
        Command::SettingsExport => config_ops::settings_export(),
        Command::SettingsValidate { file } => config_ops::settings_validate(&file),
        Command::UserDict { file, action } => {
            let path_str = file.unwrap_or_else(user_dict_ops::default_user_dict_path);
            let path = Path::new(&path_str);
            match action {
                UserDictAction::Add { reading, surface } => {
                    user_dict_ops::user_dict_add(path, &reading, &surface)
                }
                UserDictAction::Remove { reading, surface } => {
                    user_dict_ops::user_dict_remove(path, &reading, &surface)
                }
                UserDictAction::List => user_dict_ops::user_dict_list(path),
            }
        }
        #[cfg(feature = "neural")]
        Command::NeuralScore {
            dict_file,
            conn_file,
            model,
            kana,
            n,
            history,
            context,
        } => {
            use lex_cli::commands::neural_ops;
            neural_ops::neural_score_cmd(
                &dict_file,
                &conn_file,
                &model,
                &kana,
                n,
                history.as_deref(),
                &context,
            )
        }
        #[cfg(feature = "neural")]
        Command::Generate {
            model,
            context,
            max_tokens,
        } => {
            use lex_cli::commands::neural_ops;
            neural_ops::generate_cmd(&model, &context, max_tokens)
        }
        #[cfg(feature = "neural")]
        Command::SpeculativeDecode {
            dict_file,
            conn_file,
            model,
            kana,
            context,
            threshold,
            max_iter,
            compare,
        } => {
            use lex_cli::commands::neural_ops;
            neural_ops::speculative_decode_cmd(
                &dict_file, &conn_file, &model, &kana, &context, threshold, max_iter, compare,
            )
        }
    }
}
