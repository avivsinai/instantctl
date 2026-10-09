#![doc = include_str!("../README.md")]

pub mod api;
pub mod auth;
pub mod client;
pub mod device;
pub mod error;
mod inventory;
pub mod keystore;
pub mod locator;
pub mod mutation;
pub mod ports;
pub mod profile_lock;
pub mod refreshing;
pub mod secret;
pub mod site;
pub mod sso;

pub use auth::{StaticToken, TokenSource};
pub use client::Client;
pub use error::{Error, ErrorKind};
pub use inventory::validate_site_id;
pub use keystore::{CredentialStore, KeychainStore, MemoryStore, StoredCredential};
pub use profile_lock::{ProfileGuard, ProfileLock};
pub use refreshing::{ProfileMetadata, RefreshingTokenSource};
pub use site::Resource;
pub use sso::{SsoClient, SsoError, Tokens};
