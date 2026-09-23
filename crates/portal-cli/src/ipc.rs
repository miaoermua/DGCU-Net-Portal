use interprocess::local_socket::{
    tokio::{prelude::*, Stream},
    GenericNamespaced,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub const SOCKET_NAME: &str = "portal-cli.sock";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Request {
    Status,
    Shutdown,
    Up {
        username: String,
        password: String,
        portal_url: String,
        backend_only: bool,
    },
    Down {
        session_id: String,
    },
    Select {
        session_id: String,
    },
    Sessions,
    Logs,
    Reload,
    ClearLogs,
    Forget,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    pub message: String,
    pub snapshot: Option<crate::controller::Snapshot>,
    pub logs: Option<Vec<crate::logging::LogEntry>>,
}

impl Response {
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
            snapshot: None,
            logs: None,
        }
    }
}

pub async fn request(
    request: Request,
) -> Result<Response, Box<dyn std::error::Error + Send + Sync>> {
    let name = SOCKET_NAME.to_ns_name::<GenericNamespaced>()?;
    let stream = Stream::connect(name).await?;
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    writer
        .write_all(serde_json::to_string(&request)?.as_bytes())
        .await?;
    writer.write_all(b"\n").await?;
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    Ok(serde_json::from_str(&line)?)
}
