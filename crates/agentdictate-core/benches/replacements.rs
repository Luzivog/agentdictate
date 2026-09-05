use std::hint::black_box;
use std::time::{Duration, Instant};

use agentdictate_core::{ReplacementRule, apply_replacements};

fn measure(text: &str, rules: &[ReplacementRule], iterations: u32) -> Duration {
    let start = Instant::now();
    for _ in 0..iterations {
        black_box(apply_replacements(black_box(text), black_box(rules)).unwrap());
    }
    start.elapsed()
}

fn main() {
    // Cargo test --all-targets executes custom harnesses without --bench.
    if !std::env::args().any(|argument| argument == "--bench") {
        return;
    }
    let rule = ReplacementRule {
        id: Some(1),
        source_phrase: "shoe".into(),
        replacement_phrase: "SHU".into(),
        enabled: true,
        case_sensitive: false,
        whole_word_only: true,
    };
    let short = "Shoe and shoelace, then shoe. ";
    let long = short.repeat(1_000);
    let rules = [rule];
    let disabled = [ReplacementRule {
        enabled: false,
        ..rules[0].clone()
    }];
    println!("case,bytes,rules,iterations,min_ns,median_ns,max_ns");
    for (name, text, rules) in [
        ("no_rules", short, &[][..]),
        ("disabled", short, &disabled[..]),
        ("no_match", "unrelated dictation text", &rules[..]),
        ("short", short, &rules[..]),
        ("long", long.as_str(), &rules[..]),
    ] {
        let mut iterations = 1;
        while measure(text, rules, iterations) < Duration::from_millis(30) {
            iterations *= 2;
        }
        let mut samples = [0; 9];
        for sample in &mut samples {
            *sample = measure(text, rules, iterations).as_nanos() / u128::from(iterations);
        }
        samples.sort_unstable();
        println!(
            "{name},{},{},{iterations},{},{},{}",
            text.len(),
            rules.len(),
            samples[0],
            samples[4],
            samples[8]
        );
    }
}
