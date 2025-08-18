use clap::Parser;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use owo_colors::OwoColorize;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::{
    cli::{Cli, Source},
    identifier::{Identifier, doi::Doi},
};

mod cli;
mod identifier;
mod registry;

fn main() -> anyhow::Result<()> {
    let args = Cli::parse();
    match args.command {
        cli::Command::Fetch { from } => {
            let start = Instant::now();
            // Collect only identifier sources for now (ignore files for the moment).
            let jobs: Vec<String> = from
                .iter()
                .filter_map(|s| match s {
                    Source::Identifier(i) => Some(i.clone()),
                    Source::File(_) => None,
                })
                .collect();
            let total = jobs.len();

            // Set up unified progress UI.
            let mp = MultiProgress::with_draw_target(ProgressDrawTarget::stderr());
            let root = mp.add(ProgressBar::new(jobs.len() as u64));
            root.enable_steady_tick(Duration::from_millis(200));
            root.set_style(
                ProgressStyle::with_template("{spinner:.white} {msg:.dim} ({pos}/{len})")
                    .unwrap()
                    .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
            );
            root.set_message("Resolving items...");

            // Spawn resolver threads; each gets its own progress bar and updates the root.
            let mut handles = Vec::with_capacity(total);
            let (tx, rx) = mpsc::channel::<(usize, Result<String, String>)>();
            for (idx, id) in jobs.into_iter().enumerate() {
                let pb = mp.add(ProgressBar::new(100));
                pb.set_style(
                    ProgressStyle::with_template("{bar:30.green/dim} {percent:>3}% {msg:.dim}")
                        .unwrap()
                        .progress_chars("--"),
                );
                pb.set_message(format!("DOI: {}", id));
                pb.set_position(0);

                let root_bar = root.clone();
                let txc = tx.clone();
                let handle = std::thread::spawn(move || {
                    // Parse within the thread so the translator can borrow from `id`.
                    let result: Result<String, String> = match Doi::parse(&id) {
                        Some(translator) => {
                            pb.set_position(10);
                            // We can't track network progress with ureq; mark as in-progress.
                            pb.set_position(50);
                            match translator.resolve() {
                                Ok(entry) => {
                                    pb.set_position(100);
                                    Ok(entry.to_biblatex_string())
                                }
                                Err(e) => Err(format!("{}: {}", id, e)),
                            }
                        }
                        None => Err(format!("Unrecognized identifier: {}", id)),
                    };
                    // Clear the per-task bar and report back to main.
                    pb.finish_and_clear();
                    let _ = txc.send((idx, result));
                    root_bar.inc(1);
                });
                handles.push(handle);
            }
            drop(tx); // Close the channel in main

            // Collect results in input order.
            let mut ok_results: Vec<Option<String>> = vec![None; total];
            let mut errors: Vec<String> = Vec::new();
            for _ in 0..total {
                if let Ok((idx, res)) = rx.recv() {
                    match res {
                        Ok(s) => ok_results[idx] = Some(s),
                        Err(e) => errors.push(e),
                    }
                }
            }

            // Ensure all workers have finished.
            for h in handles {
                let _ = h.join();
            }

            // Finalize the root bar and print all successes at once.
            root.disable_steady_tick();
            root.finish_and_clear();

            let mut output = String::new();
            let mut ok_count: usize = 0;
            for s in ok_results.into_iter().flatten() {
                ok_count += 1;
                output.push_str(&s);
                if !s.ends_with('\n') {
                    output.push('\n');
                }
            }
            if !output.is_empty() {
                print!("{}", output);
            }
            // Print errors after clearing progress, to stderr.
            for e in &errors {
                eprintln!("{} {}", "✗".red().bold(), e.red());
            }
            let fail_count = errors.len();
            let elapsed = start.elapsed();
            let elapsed_str = format_duration(elapsed);

            // Compact, colored one-line final summary on stderr.
            if total > 0 {
                let ok_s = format!(
                    "{} {}",
                    "✓".green().bold(),
                    ok_count.to_string().green().bold()
                );
                let fail_s = format!(
                    "{} {}",
                    "✗".red().bold(),
                    fail_count.to_string().red().bold()
                );
                let total_s = format!(
                    "{} {}",
                    "total".to_string().dimmed(),
                    total.to_string().bold()
                );
                let time_s = format!("{} {}", "elapsed".to_string().dimmed(), elapsed_str.cyan());
                eprintln!("{}  •  {}  •  {}  •  {}", ok_s, fail_s, total_s, time_s);
            }
        }
        cli::Command::Pull { from } => todo!(),
    }
    Ok(())
}

fn format_duration(d: Duration) -> String {
    if d.as_secs() >= 60 {
        let m = d.as_secs() / 60;
        let s = d.as_secs() % 60;
        format!("{m}m {s:02}s")
    } else if d.as_secs() >= 1 {
        let s = d.as_secs();
        let ms = d.subsec_millis();
        format!("{s}.{ms:03}s")
    } else {
        let ms = d.as_millis();
        format!("{ms}ms")
    }
}
