//! Connecting AI agents to the desktop app without a terminal (Edit › Preferences › AI
//! Integrations).
//!
//! While *Let AI agents connect* is on, the app serves the control protocol on loopback and
//! publishes where in its settings folder ([`CONNECTION_FILE`]): the port, its process id and the
//! bearer-token file ([`TOKEN_FILE`], created `0600` on Unix). `photocraft-cli mcp --live`
//! ([`crate::live`]) reads that file on every tool call, so MCP client configurations never hold a
//! port or a token, and keep working when the app restarts on another port.
//!
//! [`install`] adds that command to an MCP client's own configuration (Claude Desktop, Claude
//! Code, Cursor) or returns the JSON to paste into any other client; the app's buttons and
//! `photocraft-cli mcp install` share it.

use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use photocraft_engine::prefs::Integrations;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::AutomationError;

/// The connection file in the settings folder, present while the app accepts agents.
pub const CONNECTION_FILE: &str = "agent-connection.json";
/// The bearer-token file the app creates (once) in its settings folder for agent connections.
pub const TOKEN_FILE: &str = "agent-control.token";
/// The server name MCP clients list PhotoCraft under.
pub const SERVER_NAME: &str = "photocraft";
/// How long [`probe`] waits for the app to accept a connection.
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(500);
/// Version of the [`Connection`] file format.
const CONNECTION_VERSION: u32 = 1;
/// Largest connection or client configuration file read; anything bigger is not ours.
const MAX_CONFIG_BYTES: u64 = 16 << 20;

/// What the app publishes in [`CONNECTION_FILE`]. Holds no secret: the token stays in its own
/// user-only file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub version: u32,
    /// The app's process id: only that process removes the file.
    pub pid: u32,
    /// Loopback port of the control server.
    pub port: u16,
    pub token_file: PathBuf,
    /// The app's version, for diagnostics.
    pub app: String,
}

impl Connection {
    pub fn new(port: u16, token_file: PathBuf) -> Self {
        Connection { version: CONNECTION_VERSION, pid: std::process::id(), port, token_file, app: photocraft_engine::build_info::long_version() }
    }

    /// `127.0.0.1:<port>`.
    pub fn addr(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }
}

fn io_error(path: &Path, e: impl std::fmt::Display) -> AutomationError {
    AutomationError::Io(format!("{}: {e}", path.display()))
}

/// Read a small text file, refusing anything larger than [`MAX_CONFIG_BYTES`].
fn read_small(path: &Path) -> Result<Option<String>, AutomationError> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_error(path, e)),
    };
    if meta.len() > MAX_CONFIG_BYTES {
        return Err(io_error(path, format!("larger than {MAX_CONFIG_BYTES} bytes")));
    }
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_error(path, e)),
    }
}

/// Publish `c` in `dir` (written atomically, so a reader never sees half a file).
pub fn write_connection(dir: &Path, c: &Connection) -> Result<(), AutomationError> {
    std::fs::create_dir_all(dir).map_err(|e| io_error(dir, e))?;
    let path = dir.join(CONNECTION_FILE);
    let text = serde_json::to_string_pretty(c).map_err(|e| AutomationError::Other(e.to_string()))?;
    photocraft_format::atomic_write(&path, text.as_bytes()).map_err(|e| io_error(&path, e))
}

/// The connection published in `dir`, if any. A file from another version or one that doesn't
/// parse counts as none: the app rewrites it when it next starts listening.
pub fn read_connection(dir: &Path) -> Result<Option<Connection>, AutomationError> {
    let Some(text) = read_small(&dir.join(CONNECTION_FILE))? else { return Ok(None) };
    Ok(serde_json::from_str::<Connection>(&text).ok().filter(|c| c.version == CONNECTION_VERSION && c.port != 0))
}

/// Remove the connection file in `dir` if process `pid` wrote it (another instance may have
/// published its own since). Errors are ignored: a stale file only fails [`probe`].
pub fn remove_connection(dir: &Path, pid: u32) {
    if let Ok(Some(c)) = read_connection(dir)
        && c.pid == pid
    {
        let _ = std::fs::remove_file(dir.join(CONNECTION_FILE));
    }
}

/// Does something accept connections on loopback `port`? (A crashed app leaves its connection
/// file behind; nothing listens on its port any more.)
pub fn probe(port: u16, timeout: Duration) -> bool {
    port != 0 && TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), timeout).is_ok()
}

/// The AI Integrations preferences saved in `dir/preferences.json` (defaults when the file or
/// the section is missing or unreadable: the CLI must not fail over the app's settings).
pub fn read_integrations(dir: &Path) -> Integrations {
    read_small(&dir.join("preferences.json"))
        .ok()
        .flatten()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|v| v.get("integrations").cloned())
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

/// A folder preference as a path (`None` when empty).
pub fn folder(pref: &str) -> Option<PathBuf> {
    let pref = pref.trim();
    (!pref.is_empty()).then(|| PathBuf::from(pref))
}

// ---- MCP clients --------------------------------------------------------------------------------

/// How an MCP client starts PhotoCraft's MCP server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpLaunch {
    pub command: String,
    pub args: Vec<String>,
}

impl McpLaunch {
    /// `<cli> mcp --live`.
    pub fn cli(cli: &Path) -> Self {
        McpLaunch { command: cli.to_string_lossy().into_owned(), args: vec!["mcp".into(), "--live".into()] }
    }

    /// How to start the CLI that ships beside the executable in `exe_dir`. `appimage` is the
    /// AppImage this process runs from (`$APPIMAGE`): its mount point disappears when the app
    /// quits, so the AppImage itself starts the CLI (`--cli`, see `packaging/linux/AppRun`).
    pub fn beside(exe_dir: &Path, appimage: Option<&Path>) -> Self {
        match appimage {
            Some(image) => McpLaunch { command: image.to_string_lossy().into_owned(), args: vec!["--cli".into(), "mcp".into(), "--live".into()] },
            None => Self::cli(&exe_dir.join(cli_file_name())),
        }
    }

    /// The `mcpServers` entry (`{"command", "args"}`).
    pub fn entry(&self) -> Value {
        json!({"command": self.command, "args": self.args})
    }

    /// A complete configuration to paste into an MCP client.
    pub fn config_json(&self) -> String {
        let v = json!({"mcpServers": {SERVER_NAME: self.entry()}});
        serde_json::to_string_pretty(&v).unwrap_or_default()
    }

    /// The command line, quoted for a POSIX shell (Windows paths are double-quoted).
    pub fn shell_line(&self) -> String {
        std::iter::once(&self.command).chain(&self.args).map(|s| shell_quote(s)).collect::<Vec<_>>().join(" ")
    }
}

fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-+:,@=".contains(&b)) {
        s.to_string()
    } else if cfg!(windows) {
        format!("\"{}\"", s.replace('"', "\\\""))
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// The CLI's file name on this platform.
pub fn cli_file_name() -> &'static str {
    if cfg!(windows) { "photocraft-cli.exe" } else { "photocraft-cli" }
}

/// MCP clients PhotoCraft can add itself to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Client {
    ClaudeDesktop,
    ClaudeCode,
    Cursor,
    /// No client: the JSON to paste into any MCP client.
    Json,
}

impl Client {
    pub const ALL: [Client; 4] = [Client::ClaudeDesktop, Client::ClaudeCode, Client::Cursor, Client::Json];

    pub fn id(self) -> &'static str {
        match self {
            Client::ClaudeDesktop => "claude-desktop",
            Client::ClaudeCode => "claude-code",
            Client::Cursor => "cursor",
            Client::Json => "json",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.id() == s)
    }

    pub fn name(self) -> &'static str {
        match self {
            Client::ClaudeDesktop => "Claude Desktop",
            Client::ClaudeCode => "Claude Code",
            Client::Cursor => "Cursor",
            Client::Json => "MCP client",
        }
    }
}

/// The environment [`install`] looks at, so tests can point it at a temporary home.
pub struct ClientEnv {
    pub home: Option<PathBuf>,
    /// `%APPDATA%` (Windows).
    pub appdata: Option<PathBuf>,
    /// `$XDG_CONFIG_HOME` (Linux).
    pub xdg_config: Option<PathBuf>,
    /// `$PATH`, searched for the `claude` command.
    pub path: Option<std::ffi::OsString>,
}

impl ClientEnv {
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
        ClientEnv {
            home: var("HOME").or_else(|| var("USERPROFILE")).map(PathBuf::from),
            appdata: var("APPDATA").map(PathBuf::from),
            xdg_config: var("XDG_CONFIG_HOME").map(PathBuf::from),
            path: var("PATH"),
        }
    }

    /// Where `client` keeps its MCP servers in a JSON file (`None` for Claude Code, which is
    /// configured through its own command, and for [`Client::Json`]).
    pub fn config_file(&self, client: Client) -> Option<PathBuf> {
        match client {
            Client::ClaudeDesktop if cfg!(target_os = "macos") => {
                self.home.as_ref().map(|h| h.join("Library/Application Support/Claude/claude_desktop_config.json"))
            }
            Client::ClaudeDesktop if cfg!(windows) => self.appdata.as_ref().map(|a| a.join("Claude").join("claude_desktop_config.json")),
            Client::ClaudeDesktop => {
                self.xdg_config.clone().or_else(|| self.home.as_ref().map(|h| h.join(".config"))).map(|c| c.join("Claude").join("claude_desktop_config.json"))
            }
            Client::Cursor => self.home.as_ref().map(|h| h.join(".cursor").join("mcp.json")),
            Client::ClaudeCode | Client::Json => None,
        }
    }

    /// The `claude` command: on `PATH`, else where its installers put it (an app started from
    /// the Dock or Start menu has a minimal `PATH`).
    pub fn claude_command(&self) -> Option<PathBuf> {
        let exe = if cfg!(windows) { "claude.exe" } else { "claude" };
        let on_path = self.path.iter().flat_map(std::env::split_paths).map(|d| d.join(exe));
        let known = self.home.iter().flat_map(|h| [h.join(".local/bin").join(exe), h.join(".claude/local").join(exe)]);
        let system = ["/opt/homebrew/bin/claude", "/usr/local/bin/claude"].map(PathBuf::from).into_iter().filter(|_| !cfg!(windows));
        on_path.chain(known).chain(system).find(|p| p.is_file())
    }
}

/// What [`install`] did, for the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Installed {
    /// Wrote `file` (the previous version is kept beside it as `backup`).
    File { file: PathBuf, backup: Option<PathBuf>, unchanged: bool },
    /// Ran `claude mcp add-json`.
    ClaudeCode,
    /// The configuration to paste.
    Json(String),
}

impl Installed {
    /// One sentence for a status line or the terminal.
    pub fn message(&self, client: Client) -> String {
        let restart = match client {
            Client::ClaudeDesktop => " Restart Claude Desktop to connect.",
            Client::Cursor => " Cursor picks it up in Settings › MCP.",
            _ => "",
        };
        match self {
            Installed::File { file, unchanged: true, .. } => format!("{} already starts PhotoCraft ({}).{restart}", client.name(), file.display()),
            Installed::File { file, .. } => format!("Added PhotoCraft to {} ({}).{restart}", client.name(), file.display()),
            Installed::ClaudeCode => "Added PhotoCraft to Claude Code (user scope). Start a new Claude Code session to connect.".into(),
            Installed::Json(text) => text.clone(),
        }
    }
}

/// Add PhotoCraft (started by `launch`) to `client`'s MCP servers, replacing an older
/// `photocraft` entry and keeping everything else in its configuration.
pub fn install(client: Client, launch: &McpLaunch, env: &ClientEnv) -> Result<Installed, AutomationError> {
    match client {
        Client::Json => Ok(Installed::Json(launch.config_json())),
        Client::ClaudeCode => install_claude_code(launch, env),
        Client::ClaudeDesktop | Client::Cursor => {
            let file =
                env.config_file(client).ok_or_else(|| AutomationError::Other(format!("can't find {}'s settings folder (no home folder)", client.name())))?;
            merge_into_file(&file, launch)
        }
    }
}

/// Merge the `photocraft` entry into a JSON configuration with an `mcpServers` object.
fn merge_into_file(file: &Path, launch: &McpLaunch) -> Result<Installed, AutomationError> {
    let existing = read_small(file)?;
    let mut root = match existing.as_deref().map(str::trim) {
        None | Some("") => Map::new(),
        Some(text) => match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(m)) => m,
            Ok(_) => return Err(io_error(file, "is not a JSON object; fix or remove it, then try again")),
            Err(e) => return Err(io_error(file, format!("is not valid JSON ({e}); fix or remove it, then try again"))),
        },
    };
    let servers = root.entry("mcpServers").or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(servers) = servers else {
        return Err(io_error(file, "`mcpServers` is not an object; fix it, then try again"));
    };
    let entry = launch.entry();
    if servers.get(SERVER_NAME) == Some(&entry) {
        return Ok(Installed::File { file: file.to_path_buf(), backup: None, unchanged: true });
    }
    servers.insert(SERVER_NAME.into(), entry);
    let text = serde_json::to_string_pretty(&Value::Object(root)).map_err(|e| AutomationError::Other(e.to_string()))?;
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
    }
    // Keys are rewritten in sorted order, so keep the user's file as it was.
    let backup = match &existing {
        Some(old) => {
            let mut name = file.file_name().unwrap_or_default().to_os_string();
            name.push(".photocraft-backup");
            let backup = file.with_file_name(name);
            photocraft_format::atomic_write(&backup, old.as_bytes()).map_err(|e| io_error(&backup, e))?;
            Some(backup)
        }
        None => None,
    };
    photocraft_format::atomic_write(file, format!("{text}\n").as_bytes()).map_err(|e| io_error(file, e))?;
    Ok(Installed::File { file: file.to_path_buf(), backup, unchanged: false })
}

fn install_claude_code(launch: &McpLaunch, env: &ClientEnv) -> Result<Installed, AutomationError> {
    let claude = env.claude_command().ok_or_else(|| {
        AutomationError::Other(format!(
            "the `claude` command wasn't found; run this in a terminal instead: claude mcp add --scope user {SERVER_NAME} -- {}",
            launch.shell_line()
        ))
    })?;
    let entry = json!({"type": "stdio", "command": launch.command, "args": launch.args});
    let run = |args: &[&str]| {
        std::process::Command::new(&claude)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| AutomationError::Other(format!("{}: {e}", claude.display())))
    };
    // `add-json` refuses a name that exists: replace an older entry (ours, by name).
    let _ = run(&["mcp", "remove", "--scope", "user", SERVER_NAME]);
    let out = run(&["mcp", "add-json", "--scope", "user", SERVER_NAME, &entry.to_string()])?;
    if out.status.success() {
        Ok(Installed::ClaudeCode)
    } else {
        let why = String::from_utf8_lossy(if out.stderr.is_empty() { &out.stdout } else { &out.stderr }).trim().to_string();
        Err(AutomationError::Other(format!("claude mcp add-json failed: {why}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp(tag: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!("photocraft-agent-link-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn env_at(home: &Path) -> ClientEnv {
        ClientEnv { home: Some(home.to_path_buf()), appdata: Some(home.join("AppData")), xdg_config: None, path: None }
    }

    #[test]
    fn connection_round_trips_and_only_its_writer_removes_it() {
        let dir = temp("conn");
        assert_eq!(read_connection(&dir).unwrap(), None);
        let c = Connection::new(4242, dir.join(TOKEN_FILE));
        write_connection(&dir, &c).unwrap();
        assert_eq!(read_connection(&dir).unwrap(), Some(c.clone()));
        assert_eq!(c.addr(), "127.0.0.1:4242");
        remove_connection(&dir, c.pid.wrapping_add(1));
        assert!(dir.join(CONNECTION_FILE).exists(), "another instance's file stays");
        remove_connection(&dir, c.pid);
        assert!(!dir.join(CONNECTION_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_damaged_or_foreign_connection_file_counts_as_none() {
        let dir = temp("bad-conn");
        for text in
            ["", "{", "[]", r#"{"version":99,"pid":1,"port":1,"tokenFile":"t","app":"x"}"#, r#"{"version":1,"pid":1,"port":0,"tokenFile":"t","app":"x"}"#]
        {
            std::fs::write(dir.join(CONNECTION_FILE), text).unwrap();
            assert_eq!(read_connection(&dir).unwrap(), None, "{text}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn probe_sees_a_listener_and_not_a_closed_port() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(probe(port, PROBE_TIMEOUT));
        drop(listener);
        assert!(!probe(port, PROBE_TIMEOUT));
        assert!(!probe(0, PROBE_TIMEOUT));
    }

    #[test]
    fn integrations_come_from_the_saved_preferences_or_default() {
        let dir = temp("prefs");
        assert_eq!(read_integrations(&dir), Integrations::default());
        std::fs::write(dir.join("preferences.json"), "not json").unwrap();
        assert_eq!(read_integrations(&dir), Integrations::default());
        // A section of the wrong type doesn't fail the CLI either.
        std::fs::write(dir.join("preferences.json"), r#"{"integrations": {"connectAgents": "yes"}}"#).unwrap();
        assert_eq!(read_integrations(&dir), Integrations::default());
        std::fs::write(
            dir.join("preferences.json"),
            r#"{"general": {}, "integrations": {"connectAgents": true, "whenAppClosed": "headless", "readFolder": "/in"}}"#,
        )
        .unwrap();
        let i = read_integrations(&dir);
        assert!(i.connect_agents);
        assert_eq!(i.when_app_closed, photocraft_engine::prefs::WhenAppClosed::Headless);
        assert_eq!(folder(&i.read_folder), Some(PathBuf::from("/in")));
        assert_eq!(folder(&i.write_folder), None);
        assert_eq!(folder("  "), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn launch_commands_and_config_json() {
        let l = McpLaunch::cli(Path::new("/opt/PhotoCraft/photocraft-cli"));
        assert_eq!(l.args, ["mcp", "--live"]);
        let v: Value = serde_json::from_str(&l.config_json()).unwrap();
        assert_eq!(v["mcpServers"]["photocraft"]["command"], "/opt/PhotoCraft/photocraft-cli");
        assert_eq!(v["mcpServers"]["photocraft"]["args"], json!(["mcp", "--live"]));
        let beside = McpLaunch::beside(Path::new("/opt/PhotoCraft"), None);
        assert!(beside.command.ends_with(cli_file_name()), "{}", beside.command);
        let image = McpLaunch::beside(Path::new("/tmp/.mount_x/usr/bin"), Some(Path::new("/home/u/PhotoCraft.AppImage")));
        assert_eq!(image.command, "/home/u/PhotoCraft.AppImage");
        assert_eq!(image.args, ["--cli", "mcp", "--live"]);
        if !cfg!(windows) {
            assert_eq!(McpLaunch::cli(Path::new("/a b/it's")).shell_line(), r"'/a b/it'\''s' mcp --live");
        }
        for c in Client::ALL {
            assert_eq!(Client::parse(c.id()), Some(c));
        }
        assert_eq!(Client::parse("vscode"), None);
    }

    #[test]
    fn install_adds_the_entry_and_keeps_other_servers_and_settings() {
        let home = temp("install");
        let env = env_at(&home);
        let launch = McpLaunch::cli(Path::new("/opt/PhotoCraft/photocraft-cli"));
        for client in [Client::ClaudeDesktop, Client::Cursor] {
            let file = env.config_file(client).unwrap();
            assert!(file.starts_with(&home), "{}", file.display());
            // A fresh install creates the file.
            let first = install(client, &launch, &env).unwrap();
            assert_eq!(first, Installed::File { file: file.clone(), backup: None, unchanged: false });
            let v: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
            assert_eq!(v["mcpServers"]["photocraft"], launch.entry());
            // Other servers and settings survive; an old photocraft entry is replaced.
            std::fs::write(&file, r#"{"theme": "dark", "mcpServers": {"other": {"command": "x"}, "photocraft": {"command": "old"}}}"#).unwrap();
            let again = install(client, &launch, &env).unwrap();
            let Installed::File { backup: Some(backup), unchanged: false, .. } = &again else { panic!("{again:?}") };
            assert!(std::fs::read_to_string(backup).unwrap().contains("\"old\""), "the previous file is kept");
            let v: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
            assert_eq!(v["theme"], "dark");
            assert_eq!(v["mcpServers"]["other"]["command"], "x");
            assert_eq!(v["mcpServers"]["photocraft"], launch.entry());
            // Installing again changes nothing.
            assert!(matches!(install(client, &launch, &env).unwrap(), Installed::File { unchanged: true, .. }));
            assert!(again.message(client).contains(client.name()));
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn install_refuses_to_overwrite_a_file_it_cannot_read_as_json() {
        let home = temp("install-bad");
        let env = env_at(&home);
        let launch = McpLaunch::cli(Path::new("/x/photocraft-cli"));
        let file = env.config_file(Client::Cursor).unwrap();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        for text in ["{ broken", "[1, 2]", r#"{"mcpServers": []}"#] {
            std::fs::write(&file, text).unwrap();
            let e = install(Client::Cursor, &launch, &env).unwrap_err().to_string();
            assert!(e.contains("try again"), "{e}");
            assert_eq!(std::fs::read_to_string(&file).unwrap(), text, "left untouched");
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn claude_code_without_the_claude_command_names_the_command_to_run() {
        let home = temp("claude-code");
        let env = ClientEnv { home: Some(home.clone()), appdata: None, xdg_config: None, path: Some(home.join("empty").into_os_string()) };
        if env.claude_command().is_some() {
            return; // A system-wide `claude` (Homebrew, /usr/local) is installed here.
        }
        let e = install(Client::ClaudeCode, &McpLaunch::cli(Path::new("/x/photocraft-cli")), &env).unwrap_err().to_string();
        assert!(e.contains("claude mcp add --scope user photocraft -- /x/photocraft-cli mcp --live"), "{e}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn json_client_returns_the_configuration() {
        let launch = McpLaunch::cli(Path::new("/x/photocraft-cli"));
        let env = ClientEnv { home: None, appdata: None, xdg_config: None, path: None };
        let Installed::Json(text) = install(Client::Json, &launch, &env).unwrap() else { panic!() };
        assert_eq!(text, launch.config_json());
        assert!(install(Client::Cursor, &launch, &env).is_err(), "no home folder: an error, not a panic");
    }
}
