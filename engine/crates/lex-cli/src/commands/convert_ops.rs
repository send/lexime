use std::path::Path;
use std::process;

use lex_core::converter::ConversionContext;
use lex_core::dict::connection::ConnectionMatrix;
use lex_core::dict::TrieDictionary;

macro_rules! die {
    ($result:expr, $($arg:tt)*) => {
        $result.unwrap_or_else(|e| {
            eprintln!($($arg)*, e);
            process::exit(1);
        })
    };
}

pub fn convert_cmd(
    dict_file: &str,
    conn_file: &str,
    kana: &str,
    n: usize,
    history: Option<&str>,
    now: u64,
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
        // open_with_wal: uncheckpointed commits live only in the WAL; a
        // checkpoint-only read would ignore the most recent learning.
        let (h, _wal) = die!(
            lex_core::user_history::wal::open_with_wal(Path::new(path)),
            "Error opening history: {}"
        );
        h
    });

    let ctx = ConversionContext::new(&dict, Some(&conn)).with_history(user_history.as_ref(), now);
    let lattice = ctx.build_lattice(kana);
    if n <= 1 {
        let result = ctx.convert_from_lattice(&lattice);
        let segs: Vec<String> = result
            .iter()
            .map(|s| format!("{}({})", s.surface, s.reading))
            .collect();
        println!("{}", segs.join(" | "));
    } else {
        let nbest = ctx.convert_nbest_from_lattice(&lattice, n);
        for (i, path) in nbest.iter().enumerate() {
            let segs: Vec<String> = path
                .iter()
                .map(|s| format!("{}({})", s.surface, s.reading))
                .collect();
            println!("#{:>2}: {}", i + 1, segs.join(" | "));
        }
    }
}

pub fn conn_cost_cmd(conn_file: &str, left: u16, right: u16) {
    let conn = die!(
        ConnectionMatrix::open(Path::new(conn_file)),
        "Error opening connection matrix: {}"
    );
    let cost = conn.cost(left, right);
    let left_role = conn.role(left);
    let right_role = conn.role(right);
    let left_fw = conn.is_function_word(left);
    let right_fw = conn.is_function_word(right);

    let role_name = |r: u8| match r {
        0 => "CW",
        1 => "FW",
        2 => "Suffix",
        3 => "Prefix",
        _ => "?",
    };

    println!(
        "conn({left}, {right}) = {cost}  [{} {}{}→ {} {}{}]",
        left,
        role_name(left_role),
        if left_fw { "(fw)" } else { "" },
        right,
        role_name(right_role),
        if right_fw { "(fw)" } else { "" },
    );
}
