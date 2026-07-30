//! `tern` — command-line control client for the `ternd` daemon (session bus). Talks the same JSON contract
//! as the GUI (`tern_core::ipc`). Commands that don't need the daemon (`man`, `completions`, `--llm`, `--help`)
//! work anywhere; the rest require `ternd` to be running.

use std::io;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use tern_core::ipc::{ActionResult, BUS_NAME};
use tern_core::model::Host;
use tern_core::state::Snapshot;

const LLM_GUIDE: &str = "\
tern — control client for the tern UniFi Identity daemon (ternd), over the session bus.

Commands:
  tern status                     show sign-in / Access / drive state
  tern hosts                      list consoles/sites available to you
  tern sign-in <TOKEN>            complete sign-in with a bearer token (placeholder; browser flow pending)
  tern sign-out                   sign out
  tern connect <CONSOLE_ID>       turn on Access (One-Click VPN) for a console
  tern disconnect                 turn off Access
  tern drives                     list drives and their mount state
  tern drives enable <DRIVE_ID>   auto-mount this drive when reachable
  tern drives disable <DRIVE_ID>  stop auto-mounting this drive

Global: --json (machine-readable output), --llm (this guide).
All state is produced by ternd; the CLI only renders it. Requires ternd running on the session bus (\
phd.hviid.Tern).
";

#[zbus::proxy(
    interface = "phd.hviid.Tern",
    default_service = "phd.hviid.Tern",
    default_path = "/phd/hviid/Tern"
)]
trait Tern {
    async fn snapshot(&self) -> zbus::Result<String>;
    async fn hosts(&self) -> zbus::Result<String>;
    async fn start_sign_in(&self) -> zbus::Result<String>;
    async fn complete_sign_in(&self, token: &str) -> zbus::Result<String>;
    async fn sign_out(&self) -> zbus::Result<String>;
    async fn connect(&self, console_id: &str) -> zbus::Result<String>;
    async fn disconnect(&self) -> zbus::Result<String>;
    async fn set_auto_mount(&self, drive_id: &str, on: bool) -> zbus::Result<String>;
}

#[derive(Parser)]
#[command(name = "tern", version, about = "Control the tern UniFi Identity client")]
struct Cli {
    /// Output machine-readable JSON instead of human text.
    #[arg(long, global = true)]
    json: bool,
    /// Print an embedded guide (for agents/LLMs) and exit.
    #[arg(long, global = true)]
    llm: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Show current status (sign-in, Access, drives).
    Status,
    /// Sign in with your web browser (passkeys / SSO supported).
    Login,
    /// List the consoles/sites available to you.
    Hosts,
    /// Complete sign-in with a bearer token (for testing; prefer `login`).
    SignIn { token: String },
    /// Sign out.
    SignOut,
    /// Turn on Access for a console.
    Connect { console_id: String },
    /// Turn off Access.
    Disconnect,
    /// List drives, or toggle whether one auto-mounts.
    Drives {
        #[command(subcommand)]
        action: Option<DrivesAction>,
    },
    /// Print the man page (roff) to stdout.
    #[command(hide = true)]
    Man,
    /// Print a shell completion script to stdout.
    #[command(hide = true)]
    Completions { shell: Shell },
}

#[derive(Subcommand)]
enum DrivesAction {
    /// Auto-mount this drive when reachable.
    Enable { drive_id: String },
    /// Stop auto-mounting this drive.
    Disable { drive_id: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.llm {
        print!("{LLM_GUIDE}");
        return Ok(());
    }

    let Some(command) = cli.command else {
        Cli::command().print_help()?;
        println!();
        return Ok(());
    };

    // Commands that work without the daemon.
    match &command {
        Command::Man => {
            clap_mangen::Man::new(Cli::command()).render(&mut io::stdout())?;
            return Ok(());
        }
        Command::Completions { shell } => {
            clap_complete::generate(*shell, &mut Cli::command(), "tern", &mut io::stdout());
            return Ok(());
        }
        _ => {}
    }

    // Everything else talks to ternd.
    let conn = zbus::Connection::session().await.context("connecting to the session bus")?;
    let proxy = TernProxy::new(&conn)
        .await
        .with_context(|| format!("reaching {BUS_NAME} — is ternd running?"))?;

    match command {
        Command::Status => render_snapshot(&proxy.snapshot().await?, cli.json)?,
        Command::Login => render_action(&proxy.start_sign_in().await?, cli.json)?,
        Command::Hosts => render_hosts(&proxy.hosts().await?, cli.json)?,
        Command::SignIn { token } => render_action(&proxy.complete_sign_in(&token).await?, cli.json)?,
        Command::SignOut => render_action(&proxy.sign_out().await?, cli.json)?,
        Command::Connect { console_id } => render_action(&proxy.connect(&console_id).await?, cli.json)?,
        Command::Disconnect => render_action(&proxy.disconnect().await?, cli.json)?,
        Command::Drives { action } => match action {
            None => render_drives(&proxy.snapshot().await?, cli.json)?,
            Some(DrivesAction::Enable { drive_id }) => {
                render_action(&proxy.set_auto_mount(&drive_id, true).await?, cli.json)?
            }
            Some(DrivesAction::Disable { drive_id }) => {
                render_action(&proxy.set_auto_mount(&drive_id, false).await?, cli.json)?
            }
        },
        Command::Man | Command::Completions { .. } => unreachable!("handled before connecting"),
    }
    Ok(())
}

fn render_snapshot(json: &str, as_json: bool) -> Result<()> {
    if as_json {
        println!("{json}");
        return Ok(());
    }
    let snap: Snapshot = serde_json::from_str(json)?;
    println!("{}", snap.summary_line());
    for d in &snap.drives {
        println!("  {:<16} {}", d.drive.name, d.state.label());
    }
    Ok(())
}

fn render_drives(json: &str, as_json: bool) -> Result<()> {
    let snap: Snapshot = serde_json::from_str(json)?;
    if as_json {
        println!("{}", serde_json::to_string(&snap.drives)?);
        return Ok(());
    }
    if snap.drives.is_empty() {
        println!("No drives yet — turn on Access to discover them.");
    }
    for d in &snap.drives {
        println!("  {:<16} {}", d.drive.name, d.state.label());
    }
    Ok(())
}

fn render_hosts(json: &str, as_json: bool) -> Result<()> {
    if as_json {
        println!("{json}");
        return Ok(());
    }
    let hosts: Vec<Host> = serde_json::from_str(json)?;
    if hosts.is_empty() {
        println!("No sites yet — sign in first.");
    }
    for h in &hosts {
        println!("  {}  ({})", h.name, h.console_id);
    }
    Ok(())
}

fn render_action(json: &str, as_json: bool) -> Result<()> {
    if as_json {
        println!("{json}");
        return Ok(());
    }
    let res: ActionResult = serde_json::from_str(json)?;
    match res.error {
        None => println!("OK"),
        Some(uf) => {
            println!("{}", uf.title);
            println!("  → {}", uf.action.label());
            if let Some(detail) = uf.detail {
                println!("  (details: {detail})");
            }
        }
    }
    Ok(())
}
