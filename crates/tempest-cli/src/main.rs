//! The desktop Tempest CLI.
//!
//! The command surface is upstream's, so an existing install keeps working:
//! `setup`, `login`, `list`, `play`, `update`, `doctor`, `uri-handler`,
//! `token`, `uninstall`. Everything below the argument parsing now goes through
//! `tempest-core`, which is the same code the Android app runs.

use clap::{Parser, Subcommand};
use colored::Colorize;
use std::sync::Arc;
use tempest_core::api::Tempest;
use tempest_core::net::CancelToken;
use tempest_core::platform::linux::LinuxPlatform;
use tempest_core::runtime::manifest::{self, ComponentId};
use tempest_core::runtime::InstallPhase;
use tempest_core::{PlatformRef, TempestError};

#[derive(Parser)]
#[command(name = "tempest", version, about = "Linux launcher for Vortex")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Install the runtime, create the Wine prefix and register vortex://
    Setup,
    /// Print the stored session token
    Token,
    /// Launch a game by id
    Play { game_id: u32 },
    /// Sign in to Vortex
    Login,
    /// List available games
    List {
        /// Use the cached list instead of re-querying Vortex
        #[arg(long)]
        cached: bool,
    },
    /// Re-download the Vortex client
    Update,
    /// Handle a vortex:// URI (invoked by the desktop)
    #[command(hide = true)]
    UriHandler { uri: String },
    /// Diagnose the whole stack
    Doctor,
    /// Remove everything Tempest installed
    Uninstall,
    /// Show or install runtime components
    Runtime {
        /// Component to install; omit to list them
        component: Option<String>,
    },
    /// Sign out
    Logout,
}

fn platform() -> PlatformRef {
    match LinuxPlatform::new() {
        Ok(p) => Arc::new(p),
        Err(e) => {
            eprintln!("{} {e}", "[ERROR]".red());
            std::process::exit(1);
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("TEMPEST_LOG").unwrap_or_else(|_| "warn".into()))
        .init();

    let cli = Cli::parse();
    let app = match Tempest::new(platform()) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{} {e}", "[ERROR]".red());
            std::process::exit(1);
        }
    };

    let code = match run(&app, cli.command).await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{} {e}", "[ERROR]".red());
            1
        }
    };
    std::process::exit(code);
}

async fn run(app: &Tempest, command: Commands) -> tempest_core::Result<()> {
    match command {
        Commands::Token => match tempest_core::auth::stored_token(app.platform())? {
            Some(t) => {
                println!("{t}");
                Ok(())
            }
            None => Err(TempestError::Auth(
                "not signed in — run `tempest login` first".into(),
            )),
        },

        Commands::Login => {
            println!("{}", "=== Vortex Login ===".bold().cyan());
            let username = prompt("Username: ");
            let password = prompt_hidden("Password: ");
            println!("{} Signing in…", "[INFO]".cyan());
            let name = app.login(&username, &password).await?;
            println!("{} Signed in as {}", "[DONE]".green(), name.bold());
            Ok(())
        }

        Commands::Logout => {
            app.logout()?;
            println!("{} Signed out.", "[DONE]".green());
            Ok(())
        }

        Commands::List { cached } => {
            let catalogue = if cached {
                app.cached_games()
            } else {
                println!("{} Querying Vortex…", "[INFO]".cyan());
                app.refresh_games(&CancelToken::new(), None).await?
            };
            if catalogue.games.is_empty() {
                println!("{} No games found for this account.", "[INFO]".cyan());
                return Ok(());
            }
            println!("{} {} game(s)\n", "[INFO]".cyan(), catalogue.games.len());
            for g in &catalogue.games {
                println!("  {:>5}  {}", g.id.to_string().bold(), g.name.bold());
            }
            println!("\n  Run {} to launch one.", "tempest play <id>".cyan());
            Ok(())
        }

        Commands::Play { game_id } => {
            println!(
                "{} Fetching the launch link for game {game_id}…",
                "[INFO]".cyan()
            );
            app.play(game_id).await?;
            follow_session(app)
        }

        Commands::UriHandler { uri } => {
            let link = app.play_uri(&uri)?;
            println!("{} Launching game {}…", "[INFO]".cyan(), link.game_id);
            follow_session(app)
        }

        Commands::Update => install_component(app, ComponentId::Vortex).await,

        Commands::Runtime { component } => match component {
            None => {
                for c in app.component_status() {
                    let mark = if c.installed {
                        "[installed]".green()
                    } else if c.required {
                        "[required] ".red()
                    } else {
                        "[optional] ".yellow()
                    };
                    println!("  {mark} {:<32} {}", c.display_name, c.available_version);
                    println!("      {}", c.purpose.replace("                      ", " "));
                    println!("      licence: {}", c.license);
                    if let Some(sha) = &c.sha256 {
                        println!("      sha256:  {sha}");
                    }
                }
                Ok(())
            }
            Some(name) => {
                let id = manifest::ComponentId::parse(&name).ok_or_else(|| {
                    TempestError::other(format!(
                        "unknown component '{name}'; try one of: {}",
                        manifest::ComponentId::all()
                            .iter()
                            .map(|c| c.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                })?;
                install_component(app, id).await
            }
        },

        Commands::Setup => {
            println!("{}", "=== Tempest Setup ===".bold().cyan());
            app.install_required(Some(&progress_sink()), &CancelToken::new())
                .await?;
            match app.platform().register_uri_handler() {
                Ok(r) => println!("{} vortex:// handler: {r:?}", "[PASS]".green()),
                Err(e) => println!("{} could not register vortex://: {e}", "[WARN]".yellow()),
            }
            print_report(app);
            Ok(())
        }

        Commands::Doctor => {
            print_report(app);
            Ok(())
        }

        Commands::Uninstall => {
            let paths = app.platform().paths();
            println!("{}", "=== Tempest Uninstall ===".bold().red());
            println!("This removes:");
            for p in [paths.root(), paths.config_dir(), paths.cache_dir()] {
                println!("  {}", p.display());
            }
            if !confirm("Are you sure?") {
                println!("Cancelled.");
                return Ok(());
            }
            for p in [paths.root(), paths.config_dir(), paths.cache_dir()] {
                if p.exists() {
                    std::fs::remove_dir_all(p).ok();
                    println!("{} removed {}", "[DONE]".green(), p.display());
                }
            }
            Ok(())
        }
    }
}

async fn install_component(app: &Tempest, id: ComponentId) -> tempest_core::Result<()> {
    let spec = manifest::spec(id);
    println!(
        "{} Installing {} ({} MB)…",
        "[INFO]".cyan(),
        spec.display_name,
        spec.approx_bytes / (1024 * 1024)
    );
    app.install_component(id.as_str(), Some(&progress_sink()), &CancelToken::new())
        .await?;
    println!("{} {} installed.", "[DONE]".green(), spec.display_name);
    Ok(())
}

fn progress_sink() -> tempest_core::runtime::ProgressSink {
    use indicatif::{ProgressBar, ProgressStyle};
    use std::sync::Mutex;

    let bar: Arc<Mutex<Option<ProgressBar>>> = Arc::new(Mutex::new(None));
    Arc::new(move |id: ComponentId, phase: InstallPhase| {
        let mut guard = bar.lock().expect("progress bar lock");
        match phase {
            InstallPhase::Downloading { done, total } => {
                let pb = guard.get_or_insert_with(|| {
                    let pb = ProgressBar::new(total.unwrap_or(0));
                    pb.set_style(
                        ProgressStyle::default_bar()
                            .template("  [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
                            .expect("static template")
                            .progress_chars("=>-"),
                    );
                    pb
                });
                if let Some(t) = total {
                    pb.set_length(t);
                }
                pb.set_position(done);
            }
            InstallPhase::Verifying => {
                if let Some(pb) = guard.take() {
                    pb.finish_and_clear();
                }
                println!("  verifying checksum…");
            }
            InstallPhase::Extracting => println!("  extracting…"),
            InstallPhase::Configuring { step } => println!("  {step}…"),
            InstallPhase::Failed { error, .. } => {
                if let Some(pb) = guard.take() {
                    pb.finish_and_clear();
                }
                eprintln!("{} {}: {error}", "[FAIL]".red(), id.as_str());
            }
            InstallPhase::Done | InstallPhase::Queued => {}
        }
    })
}

/// Stream the session's output until the game exits, mirroring upstream's
/// behaviour of staying attached to the launch.
fn follow_session(app: &Tempest) -> tempest_core::Result<()> {
    use tempest_core::session::SessionState;

    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        ctrlc_handler(move || stop.store(true, std::sync::atomic::Ordering::SeqCst));
    }

    let mut shown = 0usize;
    loop {
        let snapshot = app.session().snapshot();
        let output = app.session().output();
        for line in output.iter().skip(shown) {
            println!("{line}");
        }
        shown = output.len();

        if stop.load(std::sync::atomic::Ordering::SeqCst) {
            println!("{} Stopping…", "[INFO]".cyan());
            app.stop()?;
            return Ok(());
        }

        match snapshot.state {
            SessionState::Exited => {
                println!("{} {}", "[DONE]".green(), snapshot.status_text);
                return Ok(());
            }
            SessionState::Failed => {
                return Err(TempestError::other(
                    snapshot.error.unwrap_or(snapshot.status_text),
                ));
            }
            _ => std::thread::sleep(std::time::Duration::from_millis(250)),
        }
    }
}

/// Install a SIGINT handler without pulling in a crate for it.
fn ctrlc_handler<F: Fn() + Send + Sync + 'static>(f: F) {
    use std::sync::OnceLock;
    static HANDLER: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
    if HANDLER.set(Box::new(f)).is_err() {
        return;
    }
    extern "C" fn on_signal(_: libc::c_int) {
        if let Some(h) = HANDLER.get() {
            h();
        }
    }
    let handler = on_signal as extern "C" fn(libc::c_int) as usize as libc::sighandler_t;
    // SAFETY: `on_signal` only reads a OnceLock that is already initialised,
    // and stores to an AtomicBool — both async-signal-safe.
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
}

fn print_report(app: &Tempest) {
    use tempest_core::diagnostics::Verdict;
    let report = app.diagnostics();
    println!("\n{}", "=== Tempest Doctor ===".bold().cyan());
    for c in &report.checks {
        let mark = match c.verdict {
            Verdict::Pass => "[PASS]".green().bold(),
            Verdict::Warn => "[WARN]".yellow().bold(),
            Verdict::Fail => "[FAIL]".red().bold(),
        };
        println!("{mark} {}: {}", c.name.bold(), c.detail);
        if let Some(fix) = &c.fix {
            println!("       {} {}", "-->".yellow(), fix.cyan());
        }
    }
    println!();
    if report.failures == 0 {
        println!("{} All checks passed.", "[DONE]".green().bold());
    } else {
        println!(
            "{} {} check(s) failed, {} warning(s).",
            "[WARN]".yellow().bold(),
            report.failures,
            report.warnings
        );
    }
}

fn prompt(label: &str) -> String {
    use std::io::Write;
    print!("{} {label}", ">>>".cyan());
    std::io::stdout().flush().ok();
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf).ok();
    buf.trim().to_string()
}

fn prompt_hidden(label: &str) -> String {
    use std::io::{BufRead, Write};
    print!("{} {label}", ">>>".cyan());
    std::io::stdout().flush().ok();

    let fd = 0;
    let mut term: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is stdin and `term` is a correctly sized, zeroed struct.
    let have_term = unsafe { libc::tcgetattr(fd, &mut term) } == 0;
    let restore = term;
    if have_term {
        term.c_lflag &= !libc::ECHO;
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &term) };
    }

    let mut buf = String::new();
    std::io::stdin().lock().read_line(&mut buf).ok();

    if have_term {
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &restore) };
        println!();
    }
    buf.trim().to_string()
}

fn confirm(msg: &str) -> bool {
    matches!(
        prompt(&format!("{} [y/N] ", msg.yellow()))
            .to_lowercase()
            .as_str(),
        "y" | "yes"
    )
}
