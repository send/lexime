use std::fs;
use std::process;

macro_rules! die {
    ($result:expr, $($arg:tt)*) => {
        $result.unwrap_or_else(|e| {
            eprintln!($($arg)*, e);
            process::exit(1);
        })
    };
}

pub fn romaji_export() {
    print!("{}", lex_core::romaji::default_toml());
}

pub fn romaji_validate(file: &str) {
    let content = die!(fs::read_to_string(file), "Error reading {file}: {}");
    let map = die!(lex_core::romaji::parse_romaji_toml(&content), "Error: {}");
    println!("OK: {} mappings", map.len());
}

pub fn settings_export() {
    print!("{}", lex_core::settings::default_toml());
}

pub fn settings_validate(file: &str) {
    let content = die!(fs::read_to_string(file), "Error reading {file}: {}");
    let s = die!(
        lex_core::settings::parse_settings_toml(&content),
        "Error: {}"
    );
    println!(
        "OK: cost.segment_penalty={}, candidates.nbest={}, candidates.max_results={}",
        s.cost.segment_penalty, s.candidates.nbest, s.candidates.max_results
    );
}
