use std::env;
use std::fs;
use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use crossterm::terminal;
use reedline::{DefaultPrompt, DefaultPromptSegment, ExternalPrinter, Reedline, Signal};
use threadbridge_rust::repository::TranscriptMirrorEntry;
use threadbridge_rust::viewer_text::{
    filter_transcript_entries, parse_transcript_mirror_line, read_transcript_mirror_jsonl,
    render_transcript_lines,
};

#[derive(Debug, Clone)]
struct Args {
    data_root: PathBuf,
    workspace: PathBuf,
    thread_key: String,
    session_id: String,
    since: String,
}

fn parse_args() -> Result<Args> {
    let mut data_root = None;
    let mut workspace = None;
    let mut thread_key = None;
    let mut session_id = None;
    let mut since = None;

    let mut iter = env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--repo-root" => {
                let _ = iter.next().context("missing value for --repo-root")?;
            }
            "--data-root" => {
                data_root = Some(PathBuf::from(
                    iter.next().context("missing value for --data-root")?,
                ));
            }
            "--workspace" => {
                workspace = Some(PathBuf::from(
                    iter.next().context("missing value for --workspace")?,
                ));
            }
            "--thread-key" => {
                thread_key = Some(iter.next().context("missing value for --thread-key")?);
            }
            "--session-id" => {
                session_id = Some(iter.next().context("missing value for --session-id")?);
            }
            "--since" => {
                since = Some(iter.next().context("missing value for --since")?);
            }
            other => bail!("unknown argument: {other}"),
        }
    }

    Ok(Args {
        data_root: data_root.context("--data-root is required")?,
        workspace: workspace.context("--workspace is required")?,
        thread_key: thread_key.context("--thread-key is required")?,
        session_id: session_id.context("--session-id is required")?,
        since: since.context("--since is required")?,
    })
}

fn mirror_path(args: &Args) -> PathBuf {
    args.data_root
        .join(&args.thread_key)
        .join("state")
        .join("transcript-mirror.jsonl")
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn resume_command(args: &Args) -> Command {
    let snippet = args.workspace.join(".threadbridge/shell/codex-sync.bash");
    let shell_command = format!(
        "source {} && hcodex resume {} --thread-key {}",
        shell_single_quote(&snippet.display().to_string()),
        shell_single_quote(&args.session_id),
        shell_single_quote(&args.thread_key),
    );
    let mut cmd = Command::new("/bin/zsh");
    cmd.arg("-lc").arg(shell_command);
    cmd
}

fn terminal_width() -> u16 {
    terminal::size()
        .map(|(width, _)| width)
        .unwrap_or(100)
        .max(40)
}

fn load_entries(path: &Path, since: &str) -> Result<Vec<TranscriptMirrorEntry>> {
    let entries = read_transcript_mirror_jsonl(path)?;
    Ok(filter_transcript_entries(&entries, Some(since)))
}

fn raw_line_count(path: &Path) -> usize {
    match fs::read_to_string(path) {
        Ok(content) => content.lines().count(),
        Err(_) => 0,
    }
}

fn render_entries(entries: &[TranscriptMirrorEntry]) -> Vec<String> {
    render_transcript_lines(entries, terminal_width())
}

fn print_terminal_title(args: &Args) {
    let title = format!(
        "threadBridge viewer | {} | r resume | q quit",
        args.thread_key
    );
    print!("\x1b]0;{title}\x07");
    let _ = io::stdout().flush();
}

fn print_header(args: &Args) -> Result<()> {
    print_terminal_title(args);
    println!("threadBridge viewer");
    println!("workspace: {}", args.workspace.display());
    println!("thread_key: {}", args.thread_key);
    println!("session_id: {}", args.session_id);
    println!("mode: reedline viewer");
    println!("commands: r/resume | q/quit | help | reload");
    println!();
    io::stdout().flush()?;
    Ok(())
}

fn print_lines(lines: &[String]) -> Result<()> {
    for line in lines {
        println!("{line}");
    }
    println!();
    io::stdout().flush()?;
    Ok(())
}

fn print_reload_snapshot(path: &Path, since: &str) -> Result<()> {
    let entries = load_entries(path, since)?;
    println!("--- reload ---");
    print_lines(&render_entries(&entries))
}

fn print_help() -> Result<()> {
    println!("commands:");
    println!("  r | resume  resume local Codex CLI");
    println!("  q | quit    exit viewer");
    println!("  h | help    show this help");
    println!("  reload      reprint the current transcript");
    println!();
    io::stdout().flush()?;
    Ok(())
}

fn spawn_follow_thread(
    path: PathBuf,
    since: String,
    mut seen_lines: usize,
    stop: Arc<AtomicBool>,
    printer: ExternalPrinter<String>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            match fs::read_to_string(&path) {
                Ok(content) => {
                    let lines = content.lines().collect::<Vec<_>>();
                    if lines.len() < seen_lines {
                        seen_lines = 0;
                    }

                    let mut appended = Vec::new();
                    for line in lines.iter().skip(seen_lines) {
                        match parse_transcript_mirror_line(line) {
                            Ok(Some(entry)) if entry.timestamp.as_str() >= since.as_str() => {
                                appended.push(entry);
                            }
                            Ok(Some(_)) | Ok(None) => {}
                            Err(_) => {}
                        }
                    }

                    seen_lines = lines.len();

                    if !appended.is_empty() {
                        let rendered = render_entries(&appended).join("\n");
                        if printer.print(rendered).is_err() {
                            break;
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    seen_lines = 0;
                }
                Err(_) => {}
            }

            thread::sleep(Duration::from_millis(250));
        }
    })
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let path = mirror_path(&args);

    print_header(&args)?;
    let initial_entries = load_entries(&path, &args.since)?;
    print_lines(&render_entries(&initial_entries))?;

    let printer = ExternalPrinter::default();
    let stop = Arc::new(AtomicBool::new(false));
    let _follow_thread = spawn_follow_thread(
        path.clone(),
        args.since.clone(),
        raw_line_count(&path),
        Arc::clone(&stop),
        printer.clone(),
    );

    let prompt = DefaultPrompt::new(
        DefaultPromptSegment::Basic("viewer".to_owned()),
        DefaultPromptSegment::Empty,
    );
    let mut editor = Reedline::create().with_external_printer(printer);

    loop {
        match editor.read_line(&prompt) {
            Ok(Signal::Success(line)) => match line.trim() {
                "" => {}
                "h" | "help" => {
                    print_help()?;
                }
                "reload" => {
                    print_reload_snapshot(&path, &args.since)?;
                }
                "q" | "quit" | "exit" => {
                    stop.store(true, Ordering::Relaxed);
                    println!("viewer exited");
                    io::stdout().flush()?;
                    break;
                }
                "r" | "resume" => {
                    stop.store(true, Ordering::Relaxed);
                    println!("resuming local Codex CLI...");
                    io::stdout().flush()?;
                    let error = resume_command(&args).exec();
                    return Err(error).context("failed to exec hcodex resume");
                }
                other => {
                    println!("unknown command: {other}");
                    println!("use help to show available commands");
                    println!();
                    io::stdout().flush()?;
                }
            },
            Ok(Signal::CtrlC) | Ok(Signal::CtrlD) => {
                stop.store(true, Ordering::Relaxed);
                println!("viewer exited");
                io::stdout().flush()?;
                break;
            }
            Err(error) => {
                stop.store(true, Ordering::Relaxed);
                return Err(error).context("reedline failed");
            }
        }
    }

    Ok(())
}
