//! Example OAuth (Discord) implementation.
//!
//! Run with
//!
//! ```not_rust
//! CLIENT_ID=123 CLIENT_SECRET=secret cargo run -p example-oauth
//! ```

use async_session::{MemoryStore, Session, SessionStore};
use axum::{
    async_trait,
    body::{Bytes, Empty},
    extract::{Extension, FromRequest, Query, RequestParts, TypedHeader},
    handler::get,
    http::{header::SET_COOKIE, HeaderMap, Response},
    response::{IntoResponse, Redirect},
    AddExtensionLayer, Router,
};
use oauth2::{
    basic::BasicClient, reqwest::async_http_client, AuthUrl, AuthorizationCode, ClientId,
    ClientSecret, CsrfToken, RedirectUrl, Scope, TokenResponse, TokenUrl,
};
use serde::{Deserialize, Serialize};
use std::{env, net::SocketAddr};

// Quick instructions:
// 1) create a new application at https://discord.com/developers/applications
// 2) visit the OAuth2 tab to get your CLIENT_ID and CLIENT_SECRET
// 3) add a new redirect URI (For this example: http://localhost:3000/auth/authorized)
// 4) AUTH_URL and TOKEN_URL may stay the same for discord.
// More information: https://discord.com/developers/applications/792730475856527411/oauth2

static COOKIE_NAME: &str = "SESSION";

#[tokio::main]
async fn main() {
    // Set the RUST_LOG, if it hasn't been explicitly defined
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "example_oauth=debug")
    }
    tracing_subscriber::fmt::init();

    // `MemoryStore` just used as an example. Don't use this in production.
    let store = MemoryStore::new();

    let oauth_client = oauth_client();

    let app = Router::new()
        .route("/", get(index))
        .route("/auth/discord", get(discord_auth))
        .route("/auth/authorized", get(login_authorized))
        .route("/protected", get(protected))
        .route("/logout", get(logout))
        .layer(AddExtensionLayer::new(store))
        .layer(AddExtensionLayer::new(oauth_client));

    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    tracing::debug!("listening on {}", addr);

    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .unwrap();
}

fn oauth_client() -> BasicClient {
    // Environment variables (* = required):
    // *"CLIENT_ID"     "123456789123456789";
    // *"CLIENT_SECRET" "rAn60Mch4ra-CTErsSf-r04utHcLienT";
    //  "REDIRECT_URL"  "http://127.0.0.1:3000/auth/authorized";
    //  "AUTH_URL"      "https://discord.com/api/oauth2/authorize?response_type=code";
    //  "TOKEN_URL"     "https://discord.com/api/oauth2/token";

    let client_id = env::var("CLIENT_ID").expect("Missing CLIENT_ID!");
    let client_secret = env::var("CLIENT_SECRET").expect("Missing CLIENT_SECRET!");
    let redirect_url = env::var("REDIRECT_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:3000/auth/authorized".to_string());

    let auth_url = env::var("AUTH_URL").unwrap_or_else(|_| {
        "https://discord.com/api/oauth2/authorize?response_type=code".to_string()
    });

    let token_url = env::var("TOKEN_URL")
        .unwrap_or_else(|_| "https://discord.com/api/oauth2/token".to_string());

    BasicClient::new(
        ClientId::new(client_id),
        Some(ClientSecret::new(client_secret)),
        AuthUrl::new(auth_url).unwrap(),
        Some(TokenUrl::new(token_url).unwrap()),
    )
    .set_redirect_uri(RedirectUrl::new(redirect_url).unwrap())
}

// The user data we'll get back from Discord.
// https://discord.com/developers/docs/resources/user#user-object-user-structure
#[derive(Debug, Serialize, Deserialize)]
struct User {
    id: String,
    avatar: Option<String>,
    username: String,
    discriminator: String,
}

// Session is optional
async fn index(user: Option<User>) -> impl IntoResponse {
    match user {
        Some(u) => format!(
            "Hey {}! You're logged in!\nYou may now access `/protected`.\nLog out with `/logout`.",
            u.username
        ),
        None => "You're not logged in.\nVisit `/auth/discord` to do so.".to_string(),
    }
}

async fn discord_auth(Extension(client): Extension<BasicClient>) -> impl IntoResponse {
    let (auth_url, _csrf_token) = client
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new("identify".to_string()))
        .url();

    // Redirect to Discord's oauth service
    Redirect::found(auth_url.to_string().parse().unwrap())
}

// Valid user session required. If there is none, redirect to the auth page
async fn protected(user: User) -> impl IntoResponse {
    format!(
        "Welcome to the protected area :)\nHere's your info:\n{:?}",
        user
    )
}

async fn logout(
    Extension(store): Extension<MemoryStore>,
    TypedHeader(cookies): TypedHeader<headers::Cookie>,
) -> impl IntoResponse {
    let cookie = cookies.get(COOKIE_NAME).unwrap();
    let session = match store.load_session(cookie.to_string()).await.unwrap() {
        Some(s) => s,
        // No session active, just redirect
        None => return Redirect::found("/".parse().unwrap()),
    };

    store.destroy_session(session).await.unwrap();

    Redirect::found("/".parse().unwrap())
}

#[derive(Debug, Deserialize)]
struct AuthRequest {
    code: String,
    state: String,
}

async fn login_authorized(
    Query(query): Query<AuthRequest>,
    Extension(store): Extension<MemoryStore>,
    Extension(oauth_client): Extension<BasicClient>,
) -> impl IntoResponse {
    // Get an auth token
    let token = oauth_client
        .exchange_code(AuthorizationCode::new(query.code.clone()))
        .request_async(async_http_client)
        .await
        .unwrap();

    // Fetch user data from discord
    let client = reqwest::Client::new();
    let user_data: User = client
        // https://discord.com/developers/docs/resources/user#get-current-user
        .get("https://discordapp.com/api/users/@me")
        .bearer_auth(token.access_token().secret())
        .send()
        .await
        .unwrap()
        .json::<User>()
        .await
        .unwrap();

    // Create a new session filled with user data
    let mut session = Session::new();
    session.insert("user", &user_data).unwrap();

    // Store session and get corresponding cookie
    let cookie = store.store_session(session).await.unwrap().unwrap();

    // Build the cookie
    let cookie = format!("{}={}; SameSite=Lax; Path=/", COOKIE_NAME, cookie);

    // Set cookie
    let mut headers = HeaderMap::new();
    headers.insert(SET_COOKIE, cookie.parse().unwrap());

    (headers, Redirect::found("/".parse().unwrap()))
}

struct AuthRedirect;

impl IntoResponse for AuthRedirect {
    type Body = Empty<Bytes>;
    type BodyError = <Self::Body as axum::body::HttpBody>::Error;

    fn into_response(self) -> Response<Self::Body> {
        Redirect::found("/auth/discord".parse().unwrap()).into_response()
    }
}

#[async_trait]
impl<B> FromRequest<B> for User
where
    B: Send,
{
    // If anything goes wrong or no session is found, redirect to the auth page
    type Rejection = AuthRedirect;

    async fn from_request(req: &mut RequestParts<B>) -> Result<Self, Self::Rejection> {
        let Extension(store) = Extension::<MemoryStore>::from_request(req)
            .await
            .expect("`MemoryStore` extension is missing");

        let cookies = TypedHeader::<headers::Cookie>::from_request(req)
            .await
            .expect("could not get cookies");

        let session_cookie = cookies.get(COOKIE_NAME).ok_or(AuthRedirect)?;

        let session = store
            .load_session(session_cookie.to_string())
            .await
            .unwrap()
            .ok_or(AuthRedirect)?;

        let user = session.get::<User>("user").ok_or(AuthRedirect)?;

        Ok(user)
    }
}
