use std::path::Path;

use lex_engine::converter::cost::{CostFunction, DefaultCostFunction};
use lex_engine::converter::{build_lattice, convert, convert_nbest};
use lex_engine::dict::connection::ConnectionMatrix;
use lex_engine::dict::{Dictionary, TrieDictionary};

fn main() {
    let dict = TrieDictionary::open(Path::new("data/lexime.dict")).expect("dict");
    let conn = ConnectionMatrix::open(Path::new("data/lexime.conn")).expect("conn");

    let input = "けんとうしたいです";

    // Dictionary lookups for key substrings
    println!("=== Dictionary lookups ===");
    for key in &[
        "けんとう",
        "けんとうし",
        "したい",
        "し",
        "たい",
        "です",
        "たいです",
    ] {
        match dict.lookup(key) {
            Some(entries) => {
                let surfaces: Vec<String> = entries
                    .iter()
                    .take(8)
                    .map(|e| {
                        format!(
                            "{}(cost={},L={},R={})",
                            e.surface, e.cost, e.left_id, e.right_id
                        )
                    })
                    .collect();
                println!("  {key} -> {}", surfaces.join(", "));
            }
            None => println!("  {key} -> NOT FOUND"),
        }
    }

    // Lattice nodes
    println!("\n=== Lattice for \"{input}\" ===");
    let lattice = build_lattice(&dict, input);
    println!(
        "  {} nodes, {} chars",
        lattice.nodes.len(),
        lattice.char_count
    );
    for (i, node) in lattice.nodes.iter().enumerate() {
        println!(
            "  [{i:3}] {}-{} {:8} {:12} cost={:6} L={:5} R={:5}",
            node.start,
            node.end,
            node.reading,
            node.surface,
            node.cost,
            node.left_id,
            node.right_id
        );
    }

    // N-best results
    println!("\n=== N-best (top 10) ===");
    let nbest = convert_nbest(&dict, Some(&conn), input, 10);
    for (i, path) in nbest.iter().enumerate() {
        let segs: Vec<String> = path
            .iter()
            .map(|s| format!("{}({})", s.surface, s.reading))
            .collect();
        println!("  #{}: {}", i + 1, segs.join(" | "));
    }

    // 1-best
    println!("\n=== 1-best ===");
    let result = convert(&dict, Some(&conn), input);
    let segs: Vec<String> = result
        .iter()
        .map(|s| format!("{}({})", s.surface, s.reading))
        .collect();
    println!("  {}", segs.join(" | "));
}
