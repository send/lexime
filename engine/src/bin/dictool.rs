use std::fs;
use std::path::Path;
use std::process;

use lex_engine::dict::connection::ConnectionMatrix;
use lex_engine::dict::source;
use lex_engine::dict::{Dictionary, TrieDictionary};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        usage();
    }

    match args[1].as_str() {
        "compile" => parse_compile(&args[2..]),
        "compile-conn" => {
            if args.len() != 4 {
                eprintln!("Usage: dictool compile-conn <input-txt> <output-file>");
                process::exit(1);
            }
            compile_conn(&args[2], &args[3]);
        }
        "fetch" => parse_fetch(&args[2..]),
        "info" => {
            if args.len() != 3 {
                eprintln!("Usage: dictool info <dict-file>");
                process::exit(1);
            }
            info(&args[2]);
        }
        _ => usage(),
    }
}

fn usage() -> ! {
    eprintln!("Usage: dictool <command>");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  fetch         [--source mozc|sudachi] <output-dir>");
    eprintln!("  compile       [--source mozc|sudachi] <input-dir> <output-file>");
    eprintln!("  compile-conn  <input-txt> <output-file>");
    eprintln!("  info          <dict-file>");
    process::exit(1);
}

/// Parse `[--source mozc|sudachi] <positional>...` and return (source_name, positional_args).
fn parse_source_args(args: &[String]) -> (&str, Vec<&str>) {
    let mut source_name = "mozc";
    let mut positional = Vec::new();

    let mut i = 0;
    while i < args.len() {
        if args[i] == "--source" {
            i += 1;
            if i >= args.len() {
                eprintln!("Error: --source requires a value (mozc, sudachi)");
                process::exit(1);
            }
            source_name = args[i].as_str();
        } else {
            positional.push(args[i].as_str());
        }
        i += 1;
    }

    (source_name, positional)
}

fn parse_compile(args: &[String]) {
    let (source_name, positional) = parse_source_args(args);
    if positional.len() != 2 {
        eprintln!("Usage: dictool compile [--source mozc|sudachi] <input-dir> <output-file>");
        process::exit(1);
    }
    compile(source_name, positional[0], positional[1]);
}

fn parse_fetch(args: &[String]) {
    let (source_name, positional) = parse_source_args(args);
    if positional.len() != 1 {
        eprintln!("Usage: dictool fetch [--source mozc|sudachi] <output-dir>");
        process::exit(1);
    }

    let dict_source = source::from_name(source_name).unwrap_or_else(|| {
        eprintln!("Error: unknown source '{source_name}' (available: mozc, sudachi)");
        process::exit(1);
    });

    let output_dir = Path::new(positional[0]);
    dict_source.fetch(output_dir).unwrap_or_else(|e| {
        eprintln!("Error fetching dictionary: {e}");
        process::exit(1);
    });
}

fn compile(source_name: &str, input_dir: &str, output_file: &str) {
    let dict_source = source::from_name(source_name).unwrap_or_else(|| {
        eprintln!("Error: unknown source '{source_name}' (available: mozc, sudachi)");
        process::exit(1);
    });

    let input_path = Path::new(input_dir);
    if !input_path.is_dir() {
        eprintln!("Error: {input_dir} is not a directory");
        process::exit(1);
    }

    eprintln!("Source: {source_name}");
    let entries = dict_source.parse_dir(input_path).unwrap_or_else(|e| {
        eprintln!("Error parsing dictionary: {e}");
        process::exit(1);
    });

    let reading_count = entries.len();
    let entry_count: usize = entries.values().map(|v| v.len()).sum();

    eprintln!("Building trie from {reading_count} readings ({entry_count} entries)...");

    let dict = TrieDictionary::from_entries(entries);
    dict.save(Path::new(output_file)).unwrap_or_else(|e| {
        eprintln!("Error writing dictionary: {e}");
        process::exit(1);
    });

    let file_size = fs::metadata(output_file).map(|m| m.len()).unwrap_or(0);
    eprintln!(
        "Wrote {output_file} ({:.1} MB)",
        file_size as f64 / 1_048_576.0
    );
}

fn compile_conn(input_txt: &str, output_file: &str) {
    let text = fs::read_to_string(input_txt).unwrap_or_else(|e| {
        eprintln!("Error reading {input_txt}: {e}");
        process::exit(1);
    });

    eprintln!("Parsing connection matrix from {input_txt}...");
    let matrix = ConnectionMatrix::from_text(&text).unwrap_or_else(|e| {
        eprintln!("Error parsing connection matrix: {e}");
        process::exit(1);
    });

    eprintln!("  Matrix size: {}x{}", matrix.num_ids(), matrix.num_ids());

    matrix.save(Path::new(output_file)).unwrap_or_else(|e| {
        eprintln!("Error writing {output_file}: {e}");
        process::exit(1);
    });

    let file_size = fs::metadata(output_file).map(|m| m.len()).unwrap_or(0);
    eprintln!(
        "Wrote {output_file} ({:.1} MB)",
        file_size as f64 / 1_048_576.0
    );
}

fn info(dict_file: &str) {
    let dict = TrieDictionary::open(Path::new(dict_file)).unwrap_or_else(|e| {
        eprintln!("Error opening dictionary: {e}");
        process::exit(1);
    });

    let file_size = fs::metadata(dict_file).map(|m| m.len()).unwrap_or(0);
    let (reading_count, entry_count) = dict.stats();

    println!("Dictionary: {dict_file}");
    println!("File size:  {:.1} MB", file_size as f64 / 1_048_576.0);
    println!("Readings:   {reading_count}");
    println!("Entries:    {entry_count}");

    // Sample some entries
    let sample_keys = ["かんじ", "にほん", "とうきょう", "たべる"];
    println!();
    println!("Sample lookups:");
    for key in &sample_keys {
        if let Some(entries) = dict.lookup(key) {
            let surfaces: Vec<&str> = entries.iter().take(5).map(|e| e.surface.as_str()).collect();
            println!("  {key} → {}", surfaces.join(", "));
        } else {
            println!("  {key} → (not found)");
        }
    }
}
