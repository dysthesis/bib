use std::fs;
use std::io::{self, Write};
use std::path::Path;

fn main() -> io::Result<()> {
    let lcov_path = Path::new("target/coverage/lcov.info");
    if !lcov_path.exists() {
        eprintln!(
            "target/coverage/lcov.info not found.\n\nRun:\n  cargo tarpaulin --out Lcov --output-dir target/coverage\nthen re-run:\n  cargo run --bin coverage-badge\n"
        );
        std::process::exit(2);
    }

    let content = fs::read_to_string(lcov_path)?;
    let mut total_found: u64 = 0;
    let mut total_hit: u64 = 0;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("LF:") {
            if let Ok(v) = rest.trim().parse::<u64>() {
                total_found += v;
            }
        } else if let Some(rest) = line.strip_prefix("LH:")
            && let Ok(v) = rest.trim().parse::<u64>()
        {
            total_hit += v;
        }
    }

    let percent = if total_found > 0 {
        (total_hit as f64) * 100.0 / (total_found as f64)
    } else {
        0.0
    };
    let percent_str = format!("{:.1}%", percent);

    // Choose a simple color scale.
    let color = if percent < 50.0 {
        "#e05d44" // red
    } else if percent < 80.0 {
        "#dfb317" // yellow
    } else {
        "#4c1" // green
    };

    // Minimal SVG badge (not a full Shields style, but simple and readable)
    let label = "coverage";
    let label_width = 70; // px
    let value_width = 60; // px
    let total_width = label_width + value_width;

    let svg = format!(
        r#"<svg xmlns='http://www.w3.org/2000/svg' width='{w}' height='20' role='img' aria-label='{label}: {pct}'>
  <linearGradient id='s' x2='0' y2='100%'>
    <stop offset='0' stop-color='#bbb' stop-opacity='.1'/>
    <stop offset='1' stop-opacity='.1'/>
  </linearGradient>
  <mask id='m'>
    <rect width='{w}' height='20' rx='3' fill='#fff'/>
  </mask>
  <g mask='url(#m)'>
    <rect width='{lw}' height='20' fill='#555'/>
    <rect x='{lw}' width='{vw}' height='20' fill='{color}'/>
    <rect width='{w}' height='20' fill='url(#s)'/>
  </g>
  <g fill='#fff' text-anchor='middle' font-family='DejaVu Sans,Verdana,Geneva,sans-serif' font-size='11'>
    <text x='{lcenter}' y='15' fill='#010101' fill-opacity='.3'>{label}</text>
    <text x='{lcenter}' y='14'>{label}</text>
    <text x='{vcenter}' y='15' fill='#010101' fill-opacity='.3'>{pct}</text>
    <text x='{vcenter}' y='14'>{pct}</text>
  </g>
</svg>
"#,
        w = total_width,
        lw = label_width,
        vw = value_width,
        color = color,
        label = label,
        pct = percent_str,
        lcenter = label_width / 2,
        vcenter = label_width + value_width / 2
    );

    let out_dir = Path::new("docs");
    fs::create_dir_all(out_dir)?;
    let out_path = out_dir.join("coverage-badge.svg");
    let mut f = fs::File::create(&out_path)?;
    f.write_all(svg.as_bytes())?;
    eprintln!(
        "Wrote {} ({} / {} lines ≈ {}).",
        out_path.display(),
        total_hit,
        total_found,
        percent_str
    );
    Ok(())
}
