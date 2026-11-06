//! Native core generation-1 wire codec. Requests and frame payloads are bounded.
//! Input is never replayed after a disconnect or an ambiguous acknowledgement.

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use super::{BackendError, SendTextMode, LIMIT};

trait CoreIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> CoreIo for T {}

pub(super) struct Client {
    io: BufReader<Box<dyn CoreIo>>,
    _child: Option<tokio::process::Child>,
    framed: bool,
    next_id: u32,
    pub hello: Value,
}

impl Client {
    pub async fn open<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
        stream: S,
        child: Option<tokio::process::Child>,
        framed: bool,
    ) -> Result<Self, BackendError> {
        let mut client = Self {
            io: BufReader::new(Box::new(stream)),
            _child: child,
            framed: false,
            next_id: 1,
            hello: Value::Null,
        };
        let hello = client
            .call("hello", json!({"generation":1,"framed":framed}))
            .await?;
        if hello["generation"].as_u64() != Some(1) || hello["framed"] != framed {
            return Err(BackendError::Unsupported("native core protocol generation"));
        }
        client.framed = framed;
        client.hello = hello;
        Ok(client)
    }

    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value, BackendError> {
        let id = self.next_id;
        self.next_id = id.checked_add(1).ok_or(BackendError::Unavailable)?;
        let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let mut bytes = serde_json::to_vec(&request)
            .map_err(|_| BackendError::InvalidResponse("core request"))?;
        if self.framed {
            self.write_frame(0, 0, &bytes).await?;
        } else {
            if bytes.len() as u64 >= LIMIT {
                return Err(BackendError::InvalidResponse("core request length"));
            }
            bytes.push(b'\n');
            self.io
                .write_all(&bytes)
                .await
                .map_err(|_| BackendError::Unavailable)?;
            self.io
                .flush()
                .await
                .map_err(|_| BackendError::Unavailable)?;
        }
        let bytes = if self.framed {
            let (channel, kind, payload) = self.read_frame().await?;
            if channel != 0 || kind != 0 {
                return Err(BackendError::InvalidResponse("core RPC frame"));
            }
            payload
        } else {
            let mut line = Vec::new();
            (&mut self.io)
                .take(LIMIT + 1)
                .read_until(b'\n', &mut line)
                .await
                .map_err(|_| BackendError::Unavailable)?;
            if line.len() as u64 > LIMIT || line.last() != Some(&b'\n') {
                return Err(BackendError::InvalidResponse("core response length"));
            }
            line
        };
        let response: Value = serde_json::from_slice(&bytes)
            .map_err(|_| BackendError::InvalidResponse("core JSON"))?;
        if response["jsonrpc"] != "2.0" || response["id"] != id {
            return Err(BackendError::InvalidResponse("core response identity"));
        }
        if let Some(error) = response.get("error") {
            let code = error
                .pointer("/data/kind")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| (error["code"] == -32601).then(|| "method_not_found".into()));
            if code.as_deref() == Some("not_found") {
                return Err(BackendError::InvalidTarget("terminal"));
            }
            return Err(BackendError::Refused {
                code,
                message: error["message"]
                    .as_str()
                    .unwrap_or("core request refused")
                    .chars()
                    .take(512)
                    .collect(),
            });
        }
        response
            .get("result")
            .cloned()
            .ok_or(BackendError::InvalidResponse("core result"))
    }

    pub async fn notification(&mut self) -> Result<Value, BackendError> {
        let mut line = Vec::new();
        (&mut self.io)
            .take(LIMIT + 1)
            .read_until(b'\n', &mut line)
            .await
            .map_err(|_| BackendError::Unavailable)?;
        if line.len() as u64 > LIMIT || line.last() != Some(&b'\n') {
            return Err(BackendError::InvalidResponse("core notification length"));
        }
        let value: Value = serde_json::from_slice(&line)
            .map_err(|_| BackendError::InvalidResponse("core notification"))?;
        if value["jsonrpc"] != "2.0" || value.get("id").is_some() {
            return Err(BackendError::InvalidResponse("core notification identity"));
        }
        Ok(value)
    }
    pub async fn read_frame(&mut self) -> Result<(u32, u8, Vec<u8>), BackendError> {
        let mut header = [0; 9];
        self.io
            .read_exact(&mut header)
            .await
            .map_err(|_| BackendError::Unavailable)?;
        let len = u32::from_le_bytes(header[5..9].try_into().unwrap()) as usize;
        if len as u64 > LIMIT {
            return Err(BackendError::InvalidResponse("core frame length"));
        }
        let mut payload = vec![0; len];
        self.io
            .read_exact(&mut payload)
            .await
            .map_err(|_| BackendError::Unavailable)?;
        Ok((
            u32::from_le_bytes(header[..4].try_into().unwrap()),
            header[4],
            payload,
        ))
    }

    pub async fn write_frame(
        &mut self,
        channel: u32,
        kind: u8,
        payload: &[u8],
    ) -> Result<(), BackendError> {
        if payload.len() as u64 > LIMIT {
            return Err(BackendError::InvalidResponse("core frame length"));
        }
        let mut header = [0; 9];
        header[..4].copy_from_slice(&channel.to_le_bytes());
        header[4] = kind;
        header[5..].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        self.io
            .write_all(&header)
            .await
            .map_err(|_| BackendError::Unavailable)?;
        self.io
            .write_all(payload)
            .await
            .map_err(|_| BackendError::Unavailable)?;
        self.io.flush().await.map_err(|_| BackendError::Unavailable)
    }

    pub async fn input(
        &mut self,
        terminal: &str,
        text: &str,
        mode: SendTextMode,
    ) -> Result<(), BackendError> {
        if text.len() as u64 > LIMIT - 12 {
            return Err(BackendError::InvalidResponse("core input length"));
        }
        let geometry = self
            .call("terminal.get", json!({"terminal":terminal}))
            .await?;
        let cols = geometry["cols"]
            .as_u64()
            .filter(|n| (1..=65535).contains(n))
            .ok_or(BackendError::InvalidResponse("core columns"))?;
        let rows = geometry["rows"]
            .as_u64()
            .filter(|n| (1..=65535).contains(n))
            .ok_or(BackendError::InvalidResponse("core rows"))?;
        // Resumed attach does not take size ownership until this viewer actually
        // types. Keep the existing grid and never send FOCUS, RESIZE or FACTS.
        let attached = self.call("terminal.attach", json!({"terminal":terminal,"cols":cols,"rows":rows,"history":0,"observe":false,"resumed":true})).await?;
        let channel = attached["channel"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n != 0)
            .ok_or(BackendError::InvalidResponse("core attach channel"))?;
        let mut parser = vte::Parser::new();
        let mut modes = Modes::default();
        let mut paint_bytes = 0u64;
        // Core appends mode 9001 after the authoritative first paint. Parsing
        // with vte preserves split CSI sequences and ignores mode-like OSC text.
        while !modes.paint_complete {
            let (incoming, kind, payload) = self.read_frame().await?;
            if incoming != channel || kind != 0 {
                return Err(BackendError::InvalidResponse("core initial paint"));
            }
            paint_bytes += payload.len() as u64;
            if paint_bytes > LIMIT {
                return Err(BackendError::InvalidResponse("core paint length"));
            }
            parser.advance(&mut modes, &payload);
            self.write_frame(channel, 1, &(payload.len() as u32).to_le_bytes())
                .await?;
        }
        let bytes = if mode == SendTextMode::Paste && modes.bracketed {
            // Match core's paste sanitizer: embedded terminators cannot end a
            // paste early and turn the remaining newlines into submissions.
            format!("\x1b[200~{}\x1b[201~", text.replace("\x1b[201~", "")).into_bytes()
        } else {
            text.as_bytes().to_vec()
        };
        self.write_frame(channel, 0, &bytes).await?;
        self.write_frame(channel, 2, &[]).await?;
        loop {
            let (incoming, kind, payload) = self.read_frame().await?;
            if incoming != channel {
                return Err(BackendError::InvalidResponse("core detach channel"));
            }
            match kind {
                2 if payload.is_empty() => return Ok(()),
                0 => {
                    self.write_frame(channel, 1, &(payload.len() as u32).to_le_bytes())
                        .await?
                }
                _ => return Err(BackendError::Unavailable),
            }
        }
    }
}

#[derive(Default)]
struct Modes {
    bracketed: bool,
    paint_complete: bool,
}
impl vte::Perform for Modes {
    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermediates: &[u8],
        ignore: bool,
        action: char,
    ) {
        if ignore || intermediates != b"?" || !matches!(action, 'h' | 'l') {
            return;
        }
        for param in params {
            match param {
                [2004] => self.bracketed = action == 'h',
                [9001] => self.paint_complete = true,
                _ => {}
            }
        }
    }
}
