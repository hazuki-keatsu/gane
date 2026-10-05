// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Hazuki Keatsu

use std::{env, fs, process};

use gane_parser::{ast::Expr, parser::parse_expr};
use unicode_general_category::{GeneralCategory as G, UNICODE_VERSION, get_general_category};

const EXPECTED_GO_UNICODE_VERSION: &str = "17.0.0";

#[derive(Clone, Copy)]
struct GoRange {
    start: u32,
    end: u32,
    mask: u8,
}

fn gane_mask(c: char) -> u8 {
    let category = get_general_category(c);
    let mut mask = 0;

    // Mirrors scanner_impl.rs::unicode_is_letter.
    if matches!(
        category,
        G::UppercaseLetter
            | G::LowercaseLetter
            | G::TitlecaseLetter
            | G::ModifierLetter
            | G::OtherLetter
    ) {
        mask |= 1;
    }

    // Mirrors scanner_impl.rs::unicode_is_digit.
    if category == G::DecimalNumber {
        mask |= 2;
    }

    // Mirrors scanner_impl.rs::unicode_is_print.
    if c == ' '
        || !matches!(
            category,
            G::SpaceSeparator
                | G::LineSeparator
                | G::ParagraphSeparator
                | G::Control
                | G::Format
                | G::Surrogate
                | G::PrivateUse
                | G::Unassigned
        )
    {
        mask |= 4;
    }

    mask
}

fn parse_go_ranges(contents: &str) -> Result<(String, String, Vec<GoRange>), String> {
    let mut lines = contents.lines();
    let header = lines.next().ok_or("missing Go reference header")?;
    let mut fields = header.split('\t');
    if fields.next() != Some("GANE_UNICODE_DIFF_V1") {
        return Err("unrecognized Go reference file format".into());
    }
    let go_version = fields
        .next()
        .ok_or("missing Go toolchain version")?
        .to_owned();
    let unicode_version = fields
        .next()
        .ok_or("missing Go Unicode version")?
        .to_owned();
    if fields.next().is_some() {
        return Err("unexpected fields in Go reference header".into());
    }

    let mut ranges = Vec::new();
    let mut previous_end = None;
    for (line_number, line) in lines.enumerate() {
        let mut fields = line.split('\t');
        let start = u32::from_str_radix(
            fields
                .next()
                .ok_or_else(|| format!("missing range start on line {}", line_number + 2))?,
            16,
        )
        .map_err(|error| format!("invalid range start on line {}: {error}", line_number + 2))?;
        let end = u32::from_str_radix(
            fields
                .next()
                .ok_or_else(|| format!("missing range end on line {}", line_number + 2))?,
            16,
        )
        .map_err(|error| format!("invalid range end on line {}: {error}", line_number + 2))?;
        let mask = u8::from_str_radix(
            fields
                .next()
                .ok_or_else(|| format!("missing mask on line {}", line_number + 2))?,
            16,
        )
        .map_err(|error| format!("invalid mask on line {}: {error}", line_number + 2))?;
        if fields.next().is_some() || start > end || mask == 0 || mask & !7 != 0 {
            return Err(format!("invalid Go range on line {}", line_number + 2));
        }
        if previous_end.is_some_and(|previous| start <= previous) {
            return Err(format!(
                "overlapping or unordered Go ranges on line {}",
                line_number + 2
            ));
        }
        previous_end = Some(end);
        ranges.push(GoRange { start, end, mask });
    }

    Ok((go_version, unicode_version, ranges))
}

fn compressed_ranges(code_points: &[u32]) -> Vec<(u32, u32)> {
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for &code_point in code_points {
        if let Some((_, end)) = ranges.last_mut()
            && code_point == *end + 1
        {
            *end = code_point;
        } else {
            ranges.push((code_point, code_point));
        }
    }
    ranges
}

fn parser_accepts_identifier(source: &str) -> bool {
    let (expression, errors) = parse_expr(source);
    errors.is_none() && matches!(expression, Some(Expr::Ident(_)))
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let reference_path = args
        .next()
        .ok_or("usage: gane-unicode-diff <go-mask-file> [--check-equal]")?;
    let check_equal = args.any(|arg| arg == "--check-equal");
    let contents = fs::read_to_string(reference_path).map_err(|error| error.to_string())?;
    let (go_version, go_unicode_version, go_ranges) = parse_go_ranges(&contents)?;
    if go_unicode_version != EXPECTED_GO_UNICODE_VERSION {
        return Err(format!(
            "expected Go Unicode data {EXPECTED_GO_UNICODE_VERSION}, got {go_unicode_version} ({go_version})"
        ));
    }

    let rust_unicode_version = format!(
        "{}.{}.{}",
        UNICODE_VERSION.0, UNICODE_VERSION.1, UNICODE_VERSION.2
    );
    println!("Go reference: {go_version}, Unicode {go_unicode_version}");
    println!("Rust crate:   unicode-general-category 1.1.0, Unicode {rust_unicode_version}");

    let mut go_range_index = 0;
    let mut mismatches: [Vec<u32>; 6] = Default::default();
    for code_point in 0..=0x10_FFFF {
        let Some(c) = char::from_u32(code_point) else {
            continue;
        };
        while go_range_index < go_ranges.len() && go_ranges[go_range_index].end < code_point {
            go_range_index += 1;
        }
        let go_mask = go_ranges
            .get(go_range_index)
            .filter(|range| range.start <= code_point)
            .map_or(0, |range| range.mask);
        let rust_mask = gane_mask(c);

        for bit_index in 0..3 {
            let bit = 1 << bit_index;
            if (go_mask & bit) != (rust_mask & bit) {
                let direction_index = bit_index * 2 + usize::from(go_mask & bit == 0);
                mismatches[direction_index].push(code_point);
            }
        }
    }

    let labels = [
        "Letter: Go accepts, Rust crate rejects",
        "Letter: Rust crate accepts, Go rejects",
        "Digit: Go accepts, Rust crate rejects",
        "Digit: Rust crate accepts, Go rejects",
        "Print: Go accepts, Rust crate rejects",
        "Print: Rust crate accepts, Go rejects",
    ];
    let has_differences = mismatches.iter().any(|points| !points.is_empty());
    for (label, points) in labels.iter().zip(&mismatches) {
        println!("{label}: {} code points", points.len());
        for (start, end) in compressed_ranges(points) {
            if start == end {
                println!("  U+{start:04X}");
            } else {
                println!("  U+{start:04X}..U+{end:04X}");
            }
        }
    }

    println!("\nGane parser probes:");
    for (label, source, code_point, bit) in [
        ("Go-17 letter at identifier start", "", 0x088F, 1),
        ("Go-17 decimal digit after a letter", "a", 0x11DE0, 2),
        ("older Unicode letter at identifier start", "", 0x03A9, 1),
    ] {
        let expected_by_go = go_ranges
            .iter()
            .find(|range| range.start <= code_point && code_point <= range.end)
            .is_some_and(|range| range.mask & bit != 0);
        let character = char::from_u32(code_point).expect("probe is a Unicode scalar value");
        let identifier = format!("{source}{character}x");
        let accepted_by_gane = parser_accepts_identifier(&identifier);
        println!(
            "  {label} (U+{code_point:04X}): Go={expected_by_go}, Gane parser={accepted_by_gane}"
        );
    }

    if check_equal && has_differences {
        return Err("Unicode predicate differences found (--check-equal was requested)".into());
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        process::exit(1);
    }
}
