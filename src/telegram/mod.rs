//! Everything that talks to Telegram.
//!
//! The rest of the program depends on this module, never the other way around:
//! it takes no interest in the CLI, the config format or the terminal UI. The
//! interactive parts of signing in are expressed as the [`auth::LoginPrompt`]
//! trait so that the login flow can be driven by prompts, by a test, or by
//! anything else.

pub mod auth;
pub mod dialogs;

use std::sync::Arc;

use color_eyre::eyre::{Context, Result};
use grammers_client::client::{UpdateStream, UpdatesConfiguration};
use grammers_client::sender::SenderPoolHandle;
use grammers_client::{Client, SenderPool};
use grammers_session::updates::UpdatesLike;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;

use crate::config::{Paths, TelegramConfig};
use crate::session::FileSession;

/// A live connection to Telegram, owning the background sender pool.
///
/// Dropping this without calling [`Connection::shutdown`] still works, but the
/// shutdown path gives in-flight requests a chance to finish and persists the
/// session, which avoids re-authenticating on the next run.
pub struct Connection {
    /// The high-level client every other module uses.
    pub client: Client,

    /// The session backing the client, kept so it can be flushed to disk.
    pub session: Arc<FileSession>,

    /// Raw update feed. Taken exactly once by whoever streams updates.
    updates: Option<UnboundedReceiver<UpdatesLike>>,

    /// Handle used to ask the pool to quit.
    pool_handle: SenderPoolHandle,

    /// The task driving the connection.
    pool_task: JoinHandle<()>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("session", &self.session)
            .field("updates_taken", &self.updates.is_none())
            .finish_non_exhaustive()
    }
}

impl Connection {
    /// Load the session and bring up the connection to Telegram.
    ///
    /// This does not sign in; see [`auth`]. An unauthorized connection is still
    /// useful, since it is what the login flow itself runs on.
    pub fn open(paths: &Paths, credentials: &TelegramConfig) -> Result<Self> {
        paths.ensure_dirs()?;

        let session_path = paths.session_file();
        let session = Arc::new(
            FileSession::load(&session_path)
                .wrap_err_with(|| format!("failed to load session {}", session_path.display()))?,
        );

        let SenderPool {
            runner,
            updates,
            handle,
        } = SenderPool::new(Arc::clone(&session), credentials.api_id);

        let client = Client::new(handle.clone());
        let pool_handle = handle.thin;
        let pool_task = tokio::spawn(runner.run());

        Ok(Self {
            client,
            session,
            updates: Some(updates),
            pool_handle,
            pool_task,
        })
    }

    /// Begin streaming updates.
    ///
    /// Can only succeed once per connection, because the underlying channel has a
    /// single consumer.
    pub async fn stream_updates(&mut self, catch_up: bool) -> Result<UpdateStream> {
        let updates = self
            .updates
            .take()
            .ok_or_else(|| color_eyre::eyre::eyre!("updates are already being streamed"))?;

        self.client
            .stream_updates(
                updates,
                UpdatesConfiguration {
                    catch_up,
                    ..Default::default()
                },
            )
            .await
            .map_err(|err| color_eyre::eyre::eyre!("failed to stream updates: {err}"))
    }

    /// Write the session to disk if it changed.
    pub fn flush_session(&self) -> Result<bool> {
        self.session.flush().map_err(Into::into)
    }

    /// Close the connection, letting pending requests observe the shutdown.
    pub async fn shutdown(self) -> Result<()> {
        // Asking the pool to quit resolves every pending request with `Dropped`
        // rather than leaving handlers hanging on a dead connection.
        self.pool_handle.quit();
        let _ = self.pool_task.await;
        self.session.flush()?;
        Ok(())
    }
}
