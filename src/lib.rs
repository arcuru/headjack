use matrix_sdk::RoomMemberships;
use matrix_sdk::RoomState;
use matrix_sdk::event_handler::{EventHandler, EventHandlerHandle};
use matrix_sdk::ruma::OwnedUserId;
use matrix_sdk::ruma::events::AnySyncMessageLikeEvent;
use matrix_sdk::ruma::events::room::member::StrippedRoomMemberEvent;
use matrix_sdk::ruma::events::room::message::MessageType;
use matrix_sdk::ruma::events::room::message::OriginalSyncRoomMessageEvent;
use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;
use matrix_sdk::{
    Client, Error, LoopCtrl, Room, config::SyncSettings,
    authentication::matrix::MatrixSession,
    ruma::api::client::filter::FilterDefinition,
};
use regex::Regex;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::fs;
use tokio::sync::Mutex;
use tokio::time::sleep;
use tracing::{error, info, warn};

mod utils;
pub use utils::*;

// Re-export key types for consumers
pub use matrix_sdk::event_handler::{self, SyncEvent};

/// The data needed to re-build a client.
#[derive(Debug, Serialize, Deserialize)]
struct ClientSession {
    /// The URL of the homeserver of the user.
    homeserver: String,

    /// The path of the database.
    db_path: PathBuf,

    /// The passphrase of the database.
    passphrase: String,
}

struct HelpText {
    /// The command string that triggers this command
    command: String,
    /// Single line of help text
    short: Option<String>,
    /// Argument format.
    args: Option<String>,
}

struct State {
    /// Descriptions of the commands
    help: Vec<HelpText>,
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("State")
            .field("help_count", &self.help.len())
            .finish()
    }
}

/// The full session to persist.
/// It contains the data to re-build the client and the Matrix user session.
/// This will be synced to disk so that we can restore the session later.
#[derive(Debug, Serialize, Deserialize)]
struct FullSession {
    /// The data to re-build the client.
    client_session: ClientSession,

    /// The Matrix user session.
    user_session: MatrixSession,

    /// The latest sync token.
    #[serde(skip_serializing_if = "Option::is_none")]
    sync_token: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Login {
    /// The homeserver URL to connect to
    pub homeserver_url: String,
    /// The username to login with
    pub username: String,
    /// Optionally specify the password, if not set it will be asked for on cmd line
    pub password: Option<String>,
}

/// Configuration for creating a bot. Call `login()` to connect and get a `Bot`.
#[derive(Debug, Clone)]
pub struct BotConfig {
    /// Login info for matrix
    pub login: Login,
    /// Name to use for the bot
    /// Defaults to login.username
    pub name: Option<String>,
    /// Allow list of which accounts we will respond to
    pub allow_list: Option<String>,
    /// Set the state directory to use
    /// Defaults to $XDG_STATE_HOME/username
    pub state_dir: Option<String>,
    /// Set the prefix for bot commands. Defaults to "!($name) "
    pub command_prefix: Option<String>,
    /// The Room size limit.
    /// Will refuse to join rooms exceeding this limit.
    pub room_size_limit: Option<usize>,
}

/// Configuration for retry behavior in `run_with_retry`.
pub struct RetryConfig {
    /// Delay between retries.
    pub delay: Duration,
    /// Maximum number of retries. `None` means unlimited.
    pub max_retries: Option<usize>,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            delay: Duration::from_secs(5),
            max_retries: None,
        }
    }
}

/// A connected Matrix Bot. Created by calling `BotConfig::login()`.
#[derive(Debug)]
pub struct Bot {
    /// Configuration for the bot.
    config: BotConfig,

    /// The current sync token.
    sync_token: Option<String>,

    /// The matrix client.
    client: Client,

    /// Help text and command state, shared with handler closures.
    state: Arc<Mutex<State>>,

    /// Path to the session file on disk.
    session_file: PathBuf,
}

impl BotConfig {
    /// Get the name from config, falling back to username.
    fn name(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| self.login.username.clone())
    }

    /// Get the state directory for this config.
    fn state_dir(&self) -> PathBuf {
        if let Some(state_dir) = &self.state_dir {
            PathBuf::from(expand_tilde(state_dir))
        } else {
            dirs::state_dir()
                .expect("no state_dir directory found")
                .join(self.name())
        }
    }

    /// Get the session file path.
    fn session_file(&self) -> PathBuf {
        self.state_dir().join("session")
    }

    /// Login to the Matrix server and return a connected `Bot`.
    pub async fn login(self) -> anyhow::Result<Bot> {
        let state_dir = self.state_dir();
        let session_file = self.session_file();

        let (client, sync_token) = if session_file.exists() {
            restore_session(&session_file).await?
        } else {
            (
                do_login(
                    &state_dir,
                    &session_file,
                    &self.login.homeserver_url,
                    &self.login.username,
                    &self.login.password,
                )
                .await?,
                None,
            )
        };

        let state = Arc::new(Mutex::new(State { help: Vec::new() }));

        Ok(Bot {
            config: self,
            sync_token,
            client,
            state,
            session_file,
        })
    }
}

impl Bot {
    /// Get the path to the session file
    fn session_file(&self) -> &Path {
        &self.session_file
    }

    /// Perform a single sync against the homeserver.
    /// Returns the next_batch token on success.
    pub async fn sync_once(&mut self) -> anyhow::Result<String> {
        let filter = FilterDefinition::with_lazy_loading();
        let mut sync_settings = SyncSettings::default().filter(filter.into());

        if let Some(sync_token) = &self.sync_token {
            sync_settings = sync_settings.token(sync_token);
        }

        let response = self.client.sync_once(sync_settings).await?;
        self.sync_token = Some(response.next_batch.clone());
        persist_sync_token(self.session_file(), response.next_batch.clone()).await?;
        Ok(response.next_batch)
    }

    /// Sync to the current state of the homeserver, retrying on transient errors.
    pub async fn sync(&mut self) -> anyhow::Result<()> {
        loop {
            match self.sync_once().await {
                Ok(_) => break,
                Err(error) => {
                    error!("An error occurred during initial sync: {error}");
                    error!("Trying again…");
                }
            }
        }
        Ok(())
    }

    /// Create the help command
    async fn register_help_command(&self) {
        let state = self.state.clone();
        let command_prefix = self.command_prefix();
        self.register_text_command(
            "help",
            None,
            Some("Show this message".to_string()),
            |_, _, room| async move {
                let state = state.lock().await;
                let help = &state.help;
                let mut response = format!("`{}help`\n\nAvailable commands:", command_prefix);

                for h in help {
                    response.push_str(&format!("\n`{}{}", command_prefix, h.command));
                    if let Some(args) = &h.args {
                        response.push_str(&format!(" {}", args));
                    }
                    if let Some(short) = &h.short {
                        response.push_str(&format!("` - {}", short));
                    }
                }
                room.send(RoomMessageEventContent::text_markdown(response))
                    .await
                    .map_err(|_| ())?;
                Ok(())
            },
        )
        .await;
    }

    /// Register a generic event handler, delegating to the underlying matrix-sdk Client.
    /// This allows handling any event type without needing explicit headjack support.
    pub fn add_event_handler<Ev, Ctx, H>(&self, handler: H) -> EventHandlerHandle
    where
        Ev: SyncEvent + DeserializeOwned + Send + 'static,
        H: EventHandler<Ev, Ctx>,
    {
        self.client.add_event_handler(handler)
    }

    /// Adds a callback to join rooms we've been invited to.
    /// Ignores invites from anyone who is not on the allow_list.
    pub fn join_rooms(&self) {
        let allow_list = self.config.allow_list.clone();
        let username = self.full_name();
        let room_size_limit = self.config.room_size_limit;
        self.client.add_event_handler(
            move |room_member: StrippedRoomMemberEvent, client: Client, room: Room| async move {
                if room_member.state_key != client.user_id().unwrap() {
                    return;
                }
                if !is_allowed(allow_list, room_member.sender.as_str(), &username) {
                    return;
                }
                info!("Received stripped room member event: {:?}", room_member);

                tokio::spawn(async move {
                    info!("Autojoining room {}", room.room_id());
                    let mut delay = 2;

                    while let Err(err) = room.join().await {
                        warn!(
                            "Failed to join room {} ({err:?}), retrying in {delay}s",
                            room.room_id()
                        );

                        sleep(Duration::from_secs(delay)).await;
                        delay *= 2;

                        if delay > 3600 {
                            error!("Can't join room {} ({err:?})", room.room_id());
                            break;
                        }
                    }
                    if is_room_too_large(&room, room_size_limit).await {
                        warn!(
                            "Room {} has too many members, refusing to join",
                            room.room_id()
                        );
                        if let Err(e) = room.leave().await {
                            error!("Error leaving room: {:?}", e);
                        }
                        return;
                    }
                    info!("Successfully joined room {}", room.room_id());
                });
            },
        );
    }

    /// Adds a callback to join rooms we've been invited to.
    /// Ignores invites from anyone who is not on the allow_list.
    /// Calls the callback each time a room is joined.
    pub fn join_rooms_callback<F, Fut>(&self, callback: Option<F>)
    where
        F: FnOnce(Room) -> Fut + Send + 'static + Clone + Sync,
        Fut: std::future::Future<Output = Result<(), ()>> + Send + 'static,
    {
        let allow_list = self.config.allow_list.clone();
        let username = self.full_name();
        let room_size_limit = self.config.room_size_limit;
        self.client.add_event_handler(
            move |room_member: StrippedRoomMemberEvent, client: Client, room: Room| async move {
                if room_member.state_key != client.user_id().unwrap() {
                    return;
                }
                if !is_allowed(allow_list, room_member.sender.as_str(), &username) {
                    return;
                }
                info!("Received stripped room member event: {:?}", room_member);

                tokio::spawn(async move {
                    info!("Autojoining room {}", room.room_id());
                    let mut delay = 2;

                    while let Err(err) = room.join().await {
                        warn!(
                            "Failed to join room {} ({err:?}), retrying in {delay}s",
                            room.room_id()
                        );

                        sleep(Duration::from_secs(delay)).await;
                        delay *= 2;

                        if delay > 3600 {
                            error!("Can't join room {} ({err:?})", room.room_id());
                            break;
                        }
                    }
                    if is_room_too_large(&room, room_size_limit).await {
                        warn!(
                            "Room {} has too many members, refusing to join",
                            room.room_id()
                        );
                        if let Err(e) = room.leave().await {
                            error!("Error leaving room: {:?}", e);
                        }
                        return;
                    }
                    info!("Successfully joined room {}", room.room_id());
                    if let Some(callback) = callback {
                        if let Err(e) = callback(room).await {
                            error!("Error joining room: {:?}", e)
                        }
                    }
                });
            },
        );
    }

    /// Register a handler that will be called for every non-command text message.
    pub fn register_text_handler<F, Fut>(&self, callback: F)
    where
        F: FnOnce(OwnedUserId, String, Room, OriginalSyncRoomMessageEvent) -> Fut
            + Send
            + 'static
            + Clone
            + Sync,
        Fut: std::future::Future<Output = Result<(), ()>> + Send + 'static,
    {
        let allow_list = self.config.allow_list.clone();
        let username = self.full_name();
        let command_prefix = self.command_prefix();
        self.client.add_event_handler(
            move |event: OriginalSyncRoomMessageEvent, room: Room| async move {
                if room.state() != RoomState::Joined {
                    return;
                }
                let MessageType::Text(text_content) = &event.content.msgtype.clone() else {
                    return;
                };
                if !is_allowed(allow_list, event.sender.as_str(), &username) {
                    return;
                }
                let body = text_content.body.trim_start();
                if is_command(&command_prefix, body) {
                    return;
                }
                if let Err(e) =
                    callback(event.sender.clone(), body.to_string(), room, event.clone()).await
                {
                    error!("Error responding to: {}\nError: {:?}", body, e);
                }
            },
        );
    }

    /// Register a text command.
    /// This will call the callback when the command is received.
    pub async fn register_text_command<F, Fut, OptString>(
        &self,
        command: &str,
        args: OptString,
        short_help: OptString,
        callback: F,
    ) where
        F: FnOnce(OwnedUserId, String, Room) -> Fut + Send + 'static + Clone + Sync,
        Fut: std::future::Future<Output = Result<(), ()>> + Send + 'static,
        OptString: Into<Option<String>>,
    {
        {
            let mut state = self.state.lock().await;
            state.help.push(HelpText {
                command: command.to_string(),
                args: args.into(),
                short: short_help.into(),
            });
        }
        let allow_list = self.config.allow_list.clone();
        let username = self.full_name();
        let command = command.to_owned();
        let command_prefix = self.command_prefix();
        self.client.add_event_handler(
            move |event: AnySyncMessageLikeEvent, room: Room| async move {
                if room.state() != RoomState::Joined {
                    return;
                }
                let AnySyncMessageLikeEvent::RoomMessage(event) = event else {
                    return;
                };
                let Some(event) = event.as_original() else {
                    return;
                };
                let MessageType::Text(_) = event.content.msgtype else {
                    return;
                };
                let text_content = event.content.body();
                if !is_allowed(allow_list, event.sender.as_str(), &username) {
                    return;
                }
                let body = text_content.trim_start();
                if let Some(input_command) = get_command(&command_prefix, body) {
                    if input_command == command {
                        if let Err(e) = callback(event.sender.clone(), body.to_string(), room).await
                        {
                            error!("Error running command: {} - {:?}", command, e);
                        }
                    }
                }
            },
        );
    }

    /// Run the bot's sync loop continuously.
    /// Returns on the first sync error.
    pub async fn run(&self) -> anyhow::Result<()> {
        self.register_help_command().await;
        self.run_sync_loop().await
    }

    /// Run the bot's sync loop with automatic retry on errors.
    pub async fn run_with_retry(&self, retry_config: RetryConfig) -> anyhow::Result<()> {
        self.register_help_command().await;

        let mut retries = 0;
        loop {
            match self.run_sync_loop().await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    retries += 1;
                    error!("Sync error (retry {retries}): {e}");
                    if let Some(max) = retry_config.max_retries {
                        if retries >= max {
                            return Err(e);
                        }
                    }
                    sleep(retry_config.delay).await;
                }
            }
        }
    }

    /// Inner sync loop used by both `run` and `run_with_retry`.
    async fn run_sync_loop(&self) -> anyhow::Result<()> {
        let filter = FilterDefinition::with_lazy_loading();
        let mut sync_settings = SyncSettings::default().filter(filter.into());

        if let Some(sync_token) = &self.sync_token {
            sync_settings = sync_settings.token(sync_token);
        }

        self.client
            .sync_with_result_callback(sync_settings, |sync_result| async move {
                let response = sync_result?;

                self.persist_sync_token(response.next_batch)
                    .await
                    .map_err(|err| Error::UnknownError(err.into()))?;

                Ok(LoopCtrl::Continue)
            })
            .await?;

        Ok(())
    }

    async fn persist_sync_token(&self, sync_token: String) -> anyhow::Result<()> {
        let serialized_session = fs::read_to_string(self.session_file()).await?;
        let mut full_session: FullSession = serde_json::from_str(&serialized_session)?;

        full_session.sync_token = Some(sync_token);
        let serialized_session = serde_json::to_string(&full_session)?;
        fs::write(self.session_file(), serialized_session).await?;

        Ok(())
    }

    /// Get the state directory for the bot.
    pub fn state_dir(&self) -> PathBuf {
        self.config.state_dir()
    }

    /// Get the name of the bot.
    pub fn name(&self) -> String {
        self.config.name()
    }

    /// Get the full Matrix user ID of the bot.
    pub fn full_name(&self) -> String {
        self.client.user_id().unwrap().to_string()
    }

    /// Get the underlying matrix-sdk Client.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Get the command prefix for the bot.
    pub fn command_prefix(&self) -> String {
        let prefix = self
            .config
            .command_prefix
            .clone()
            .unwrap_or_else(|| format!("!{} ", self.name()));
        if prefix.len() == 1 || prefix.ends_with(' ') {
            prefix
        } else {
            format!("{} ", prefix)
        }
    }
}

/// Verify if the sender is on the allow_list
fn is_allowed(allow_list: Option<String>, sender: &str, username: &str) -> bool {
    if sender == username {
        false
    } else if let Some(allow_list) = allow_list {
        let regex = Regex::new(&allow_list).expect("Invalid regular expression");
        regex.is_match(sender)
    } else {
        false
    }
}

/// Check if the message is a command.
pub fn is_command(command_prefix: &str, text: &str) -> bool {
    text.starts_with(command_prefix)
}

/// Get the command, if it is a command.
pub fn get_command<'a>(command_prefix: &str, text: &'a str) -> Option<&'a str> {
    if text.starts_with(command_prefix) {
        text.trim_start_matches(command_prefix)
            .split_whitespace()
            .next()
    } else {
        None
    }
}

/// Fixup the path if they've provided a ~
fn expand_tilde(path: &str) -> String {
    if path.starts_with("~/") {
        if let Some(home_dir) = dirs::home_dir() {
            let without_tilde = &path[1..];
            return home_dir.display().to_string() + without_tilde;
        }
    }
    path.to_string()
}

/// Restore a previous session.
async fn restore_session(session_file: &Path) -> anyhow::Result<(Client, Option<String>)> {
    info!(
        "Previous session found in '{}'",
        session_file.to_string_lossy()
    );

    let serialized_session = fs::read_to_string(session_file).await?;
    let FullSession {
        client_session,
        user_session,
        sync_token,
    } = serde_json::from_str(&serialized_session)?;

    let client = Client::builder()
        .homeserver_url(client_session.homeserver)
        .build()
        .await?;

    info!("Restoring session for {}…", &user_session.meta.user_id);

    client.restore_session(user_session).await?;

    info!("Done!");

    Ok((client, sync_token))
}

/// Login with a new device.
async fn do_login(
    state_dir: &Path,
    session_file: &Path,
    homeserver_url: &str,
    username: &str,
    password: &Option<String>,
) -> anyhow::Result<Client> {
    info!("No previous session found, logging in…");

    let (client, client_session) = build_client(state_dir, homeserver_url.to_owned()).await?;
    let matrix_auth = client.matrix_auth();

    let password = match password {
        Some(password) => password.clone(),
        None => {
            print!("Password: ");
            io::stdout().flush().expect("Unable to write to stdout");
            let mut password = String::new();
            io::stdin()
                .read_line(&mut password)
                .expect("Unable to read user input");
            password.trim().to_owned()
        }
    };

    match matrix_auth
        .login_username(username, &password)
        .initial_device_display_name("headjack client")
        .await
    {
        Ok(_) => {
            info!("Logged in as {username}");
        }
        Err(error) => {
            error!("Error logging in: {error}");
            return Err(error.into());
        }
    }

    let user_session = matrix_auth
        .session()
        .expect("A logged-in client should have a session");
    let serialized_session = serde_json::to_string(&FullSession {
        client_session,
        user_session,
        sync_token: None,
    })?;
    fs::write(session_file, serialized_session).await?;

    info!("Session persisted in {}", session_file.to_string_lossy());

    Ok(client)
}

/// Build a new client.
async fn build_client(
    _state_dir: &Path,
    homeserver: String,
) -> anyhow::Result<(Client, ClientSession)> {
    match Client::builder()
        .homeserver_url(&homeserver)
        .build()
        .await
    {
        Ok(client) => Ok((
            client,
            ClientSession {
                homeserver,
                db_path: PathBuf::new(),
                passphrase: String::new(),
            },
        )),
        Err(error) => Err(error.into()),
    }
}

/// Write the sync_token to the session file
async fn persist_sync_token(session_file: &Path, sync_token: String) -> anyhow::Result<()> {
    let serialized_session = fs::read_to_string(session_file).await?;
    let mut full_session: FullSession = serde_json::from_str(&serialized_session)?;

    full_session.sync_token = Some(sync_token);
    let serialized_session = serde_json::to_string(&full_session)?;
    fs::write(session_file, serialized_session).await?;

    Ok(())
}

/// Check if the room exceeds the size limit
async fn is_room_too_large(room: &Room, room_size_limit: Option<usize>) -> bool {
    if let Some(room_size_limit) = room_size_limit {
        if let Ok(members) = room.members(RoomMemberships::ACTIVE).await {
            members.len() > room_size_limit
        } else {
            false
        }
    } else {
        false
    }
}
