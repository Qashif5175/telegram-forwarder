//! The interactive sign-in flow.
//!
//! Signing in is expensive and heavily flood-limited, so this module is careful
//! to reuse an existing session whenever one is valid, and to let the user retry
//! a mistyped code without restarting the whole flow.
//!
//! The prompts themselves are abstracted behind [`LoginPrompt`] so that this
//! module stays free of any terminal dependency.

use color_eyre::eyre::{Result, bail};
use grammers_client::client::PasswordToken;
use grammers_client::peer::User;
use grammers_client::{Client, SignInError};

/// How many times a wrong login code or password may be re-entered.
///
/// Requesting a fresh code costs a flood-limited round trip, so allowing a few
/// retries against the same token is much friendlier than starting over.
const MAX_ATTEMPTS: usize = 3;

/// Everything the login flow needs to ask a human.
///
/// Implementations decide how to ask; this module decides what to ask and in
/// what order.
pub trait LoginPrompt {
    /// Phone number in international format, e.g. `+886912345678`.
    fn phone(&mut self) -> impl Future<Output = Result<String>>;

    /// The login code Telegram just sent.
    ///
    /// `retry` is true when a previous code was rejected, so the prompt can say so.
    fn code(&mut self, retry: bool) -> impl Future<Output = Result<String>>;

    /// The two-factor password, with the user's own hint if they set one.
    fn password(&mut self, hint: Option<&str>, retry: bool)
    -> impl Future<Output = Result<String>>;

    /// Report progress, e.g. that the code has been sent.
    fn notify(&mut self, message: &str);
}

/// Sign in, or confirm that the existing session is already signed in.
///
/// Returns the logged-in account. On success the caller should flush the session
/// so the authorization survives the process exiting.
pub async fn ensure_signed_in<P: LoginPrompt>(
    client: &Client,
    api_hash: &str,
    prompt: &mut P,
) -> Result<User> {
    if client.is_authorized().await? {
        return Ok(client.get_me().await?);
    }

    let phone = prompt.phone().await?;
    let token = client.request_login_code(&phone, api_hash).await?;
    prompt.notify("Telegram sent a login code to your other devices.");

    let mut retry = false;
    for _ in 0..MAX_ATTEMPTS {
        let code = prompt.code(retry).await?;

        match client.sign_in(&token, &code).await {
            Ok(user) => return Ok(user),

            // The token is borrowed, not consumed, so the same code request can
            // be answered again without asking Telegram for a new one.
            Err(SignInError::InvalidCode) => retry = true,

            Err(SignInError::PasswordRequired(token)) => {
                return complete_two_factor(client, token, prompt).await;
            }

            Err(SignInError::SignUpRequired) => bail!(
                "this phone number has no Telegram account yet. Third-party apps cannot \
                 register accounts; create one with an official Telegram client first."
            ),

            Err(err) => return Err(err.into()),
        }
    }

    bail!(
        "the login code was rejected {MAX_ATTEMPTS} times; run the command again to get a new code"
    )
}

/// Answer a two-factor password challenge.
async fn complete_two_factor<P: LoginPrompt>(
    client: &Client,
    token: PasswordToken,
    prompt: &mut P,
) -> Result<User> {
    let mut token = token;
    let mut retry = false;

    for _ in 0..MAX_ATTEMPTS {
        let hint = token.hint().map(str::to_owned);
        let password = prompt.password(hint.as_deref(), retry).await?;

        match client.check_password(token, password.trim()).await {
            Ok(user) => return Ok(user),
            // Telegram hands back a fresh token with each rejection, so retrying
            // means threading the new one through.
            Err(SignInError::InvalidPassword(next)) => {
                token = next;
                retry = true;
            }
            Err(err) => return Err(err.into()),
        }
    }

    bail!("the two-factor password was rejected {MAX_ATTEMPTS} times")
}

/// Sign out and invalidate the session server-side.
pub async fn sign_out(client: &Client) -> Result<()> {
    client.sign_out().await?;
    Ok(())
}
