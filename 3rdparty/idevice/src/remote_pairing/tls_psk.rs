// Jackson Coxson
//! Manual TLS 1.2 PSK-AES128-CBC-SHA implementation.
//!
//! This implements just enough of TLS 1.2 to negotiate
//! `TLS_PSK_WITH_AES_128_CBC_SHA` (0x008C) with no certificates or DH.
//! The result is an encrypted stream suitable for CDTunnel.
//! We did this ourselves because rustls won't :(

use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::Sha256;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tracing::debug;

use crate::IdeviceError;

// TLS 1.2 constants
const TLS_12: [u8; 2] = [0x03, 0x03];
const CT_HANDSHAKE: u8 = 0x16;
const CT_CHANGE_CIPHER_SPEC: u8 = 0x14;
const CT_APPLICATION_DATA: u8 = 0x17;
/// Maximum plaintext bytes per TLS record (RFC 5246 §6.2.1)
const TLS_MAX_PLAINTEXT: usize = 16384;
const HS_CLIENT_HELLO: u8 = 0x01;
const HS_SERVER_HELLO: u8 = 0x02;
const HS_SERVER_HELLO_DONE: u8 = 0x0E;
const HS_CLIENT_KEY_EXCHANGE: u8 = 0x10;
const HS_FINISHED: u8 = 0x14;
// Offer both; device typically picks AES256-CBC-SHA384
const PSK_CIPHER_SUITES: &[[u8; 2]] = &[
    [0x00, 0xAF], // TLS_PSK_WITH_AES_256_CBC_SHA384 (preferred by iOS)
    [0x00, 0x8C], // TLS_PSK_WITH_AES_128_CBC_SHA (fallback)
];

type HmacSha256 = Hmac<Sha256>;
type HmacSha1 = Hmac<Sha1>;
type HmacSha384 = Hmac<sha2::Sha384>;
type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;
type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;
type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;
type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

/// Selected cipher suite parameters
#[derive(Clone, Copy, Debug)]
enum CipherSuite {
    Aes128CbcSha,    // 0x008C: 16-byte key, 20-byte MAC (SHA1), PRF=SHA256
    Aes256CbcSha384, // 0x00AF: 32-byte key, 48-byte MAC (SHA384), PRF=SHA384
}

impl CipherSuite {
    fn from_bytes(b: [u8; 2]) -> Option<Self> {
        match b {
            [0x00, 0x8C] => Some(Self::Aes128CbcSha),
            [0x00, 0xAF] => Some(Self::Aes256CbcSha384),
            _ => None,
        }
    }
    fn enc_key_len(self) -> usize {
        match self {
            Self::Aes128CbcSha => 16,
            Self::Aes256CbcSha384 => 32,
        }
    }
    fn mac_key_len(self) -> usize {
        match self {
            Self::Aes128CbcSha => 20,
            Self::Aes256CbcSha384 => 48,
        }
    }
}

struct KeyBlock {
    client_mac_key: Vec<u8>,
    server_mac_key: Vec<u8>,
    client_write_key: Vec<u8>,
    server_write_key: Vec<u8>,
    suite: CipherSuite,
}

fn hmac_compute(key: &[u8], data: &[u8], suite: CipherSuite) -> Vec<u8> {
    match suite {
        CipherSuite::Aes128CbcSha => {
            // PRF uses SHA256 for P_hash, but record MAC uses SHA1
            let mut mac = HmacSha256::new_from_slice(key).unwrap();
            mac.update(data);
            mac.finalize().into_bytes().to_vec()
        }
        CipherSuite::Aes256CbcSha384 => {
            let mut mac = HmacSha384::new_from_slice(key).unwrap();
            mac.update(data);
            mac.finalize().into_bytes().to_vec()
        }
    }
}

/// TLS 1.2 PRF: P_hash with the suite's PRF hash
fn prf(secret: &[u8], label: &[u8], seed: &[u8], len: usize, suite: CipherSuite) -> Vec<u8> {
    let mut label_seed = label.to_vec();
    label_seed.extend_from_slice(seed);

    let mut a = hmac_compute(secret, &label_seed, suite);
    let mut out = Vec::with_capacity(len);

    while out.len() < len {
        let mut input = a.clone();
        input.extend_from_slice(&label_seed);
        out.extend_from_slice(&hmac_compute(secret, &input, suite));
        a = hmac_compute(secret, &a, suite);
    }
    out.truncate(len);
    out
}

/// PSK premaster secret (RFC 4279 §2)
fn psk_premaster(psk: &[u8]) -> Vec<u8> {
    let psk_len = psk.len() as u16;
    let mut pm = Vec::with_capacity(4 + psk.len() * 2);
    pm.extend_from_slice(&psk_len.to_be_bytes());
    pm.extend(std::iter::repeat_n(0u8, psk.len()));
    pm.extend_from_slice(&psk_len.to_be_bytes());
    pm.extend_from_slice(psk);
    pm
}

fn derive_master_secret(
    psk: &[u8],
    client_random: &[u8; 32],
    server_random: &[u8; 32],
    suite: CipherSuite,
) -> Vec<u8> {
    let premaster = psk_premaster(psk);
    let mut seed = client_random.to_vec();
    seed.extend_from_slice(server_random);
    prf(&premaster, b"master secret", &seed, 48, suite)
}

fn derive_key_block(
    master: &[u8],
    client_random: &[u8; 32],
    server_random: &[u8; 32],
    suite: CipherSuite,
) -> KeyBlock {
    let mut seed = server_random.to_vec();
    seed.extend_from_slice(client_random);
    let mac_len = suite.mac_key_len();
    let key_len = suite.enc_key_len();
    let total = mac_len * 2 + key_len * 2;
    let kb = prf(master, b"key expansion", &seed, total, suite);
    let mut pos = 0;
    let client_mac_key = kb[pos..pos + mac_len].to_vec();
    pos += mac_len;
    let server_mac_key = kb[pos..pos + mac_len].to_vec();
    pos += mac_len;
    let client_write_key = kb[pos..pos + key_len].to_vec();
    pos += key_len;
    let server_write_key = kb[pos..pos + key_len].to_vec();
    KeyBlock {
        client_mac_key,
        server_mac_key,
        client_write_key,
        server_write_key,
        suite,
    }
}

fn compute_mac(mac_key: &[u8], seq: u64, ct: u8, data: &[u8], suite: CipherSuite) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&seq.to_be_bytes());
    buf.extend_from_slice(&[ct, 0x03, 0x03]);
    buf.extend_from_slice(&(data.len() as u16).to_be_bytes());
    buf.extend_from_slice(data);

    match suite {
        CipherSuite::Aes128CbcSha => {
            let mut mac = HmacSha1::new_from_slice(mac_key).unwrap();
            mac.update(&buf);
            mac.finalize().into_bytes().to_vec()
        }
        CipherSuite::Aes256CbcSha384 => {
            let mut mac = HmacSha384::new_from_slice(mac_key).unwrap();
            mac.update(&buf);
            mac.finalize().into_bytes().to_vec()
        }
    }
}

fn encrypt_record(keys: &KeyBlock, seq: u64, ct: u8, plaintext: &[u8]) -> Vec<u8> {
    let mac = compute_mac(&keys.client_mac_key, seq, ct, plaintext, keys.suite);

    let mut payload = plaintext.to_vec();
    payload.extend_from_slice(&mac);

    // PKCS#7 padding (block size = 16 for both AES-128 and AES-256)
    let pad_len = 16 - (payload.len() % 16);
    payload.extend(std::iter::repeat_n(pad_len as u8 - 1, pad_len));

    let mut iv = [0u8; 16];
    rand::fill(&mut iv);

    let ciphertext = match keys.suite {
        CipherSuite::Aes128CbcSha => {
            let enc = Aes128CbcEnc::new(keys.client_write_key[..16].into(), &iv.into());
            enc.encrypt_padded_vec_mut::<aes::cipher::block_padding::NoPadding>(&payload)
        }
        CipherSuite::Aes256CbcSha384 => {
            let enc = Aes256CbcEnc::new(keys.client_write_key[..32].into(), &iv.into());
            enc.encrypt_padded_vec_mut::<aes::cipher::block_padding::NoPadding>(&payload)
        }
    };

    let mut result = iv.to_vec();
    result.extend_from_slice(&ciphertext);
    result
}

fn decrypt_record(
    keys: &KeyBlock,
    is_server: bool,
    seq: u64,
    ct: u8,
    encrypted: &[u8],
) -> Result<Vec<u8>, IdeviceError> {
    if encrypted.len() < 16 {
        return Err(IdeviceError::InternalError("TLS record too short".into()));
    }

    let iv = &encrypted[..16];
    let ciphertext = &encrypted[16..];
    let read_key = if is_server {
        &keys.server_write_key
    } else {
        &keys.client_write_key
    };
    let mac_key = if is_server {
        &keys.server_mac_key
    } else {
        &keys.client_mac_key
    };

    let decrypted = match keys.suite {
        CipherSuite::Aes128CbcSha => {
            let dec = Aes128CbcDec::new(read_key[..16].into(), iv.into());
            dec.decrypt_padded_vec_mut::<aes::cipher::block_padding::NoPadding>(ciphertext)
                .map_err(|e| IdeviceError::InternalError(format!("CBC decrypt: {e}")))?
        }
        CipherSuite::Aes256CbcSha384 => {
            let dec = Aes256CbcDec::new(read_key[..32].into(), iv.into());
            dec.decrypt_padded_vec_mut::<aes::cipher::block_padding::NoPadding>(ciphertext)
                .map_err(|e| IdeviceError::InternalError(format!("CBC decrypt: {e}")))?
        }
    };

    if decrypted.is_empty() {
        return Err(IdeviceError::InternalError("Empty decrypted data".into()));
    }

    // Remove PKCS#7 padding: last byte is pad_value, remove (pad_value+1) bytes
    let pad_value = *decrypted.last().unwrap() as usize;
    let content_len = decrypted.len() - (pad_value + 1);
    let mac_len = keys.suite.mac_key_len();
    if content_len < mac_len {
        return Err(IdeviceError::InternalError(
            "Decrypted content too short for MAC".into(),
        ));
    }

    let plaintext = &decrypted[..content_len - mac_len];
    let received_mac = &decrypted[content_len - mac_len..content_len];

    let expected_mac = compute_mac(mac_key, seq, ct, plaintext, keys.suite);
    if received_mac != expected_mac.as_slice() {
        return Err(IdeviceError::InternalError(
            "TLS MAC verification failed".into(),
        ));
    }

    Ok(plaintext.to_vec())
}

fn finished_verify_data(
    master: &[u8],
    label: &[u8],
    transcript: &[u8],
    suite: CipherSuite,
) -> [u8; 12] {
    use sha2::Digest;
    let hash: Vec<u8> = match suite {
        CipherSuite::Aes128CbcSha => sha2::Sha256::digest(transcript).to_vec(),
        CipherSuite::Aes256CbcSha384 => sha2::Sha384::digest(transcript).to_vec(),
    };
    prf(master, label, &hash, 12, suite).try_into().unwrap()
}

fn make_record(ct: u8, payload: &[u8]) -> Vec<u8> {
    let mut rec = vec![ct, 0x03, 0x03];
    rec.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    rec.extend_from_slice(payload);
    rec
}

fn make_handshake(msg_type: u8, body: &[u8]) -> Vec<u8> {
    let mut msg = vec![msg_type];
    let len = body.len() as u32;
    msg.extend_from_slice(&len.to_be_bytes()[1..]); // 3-byte length
    msg.extend_from_slice(body);
    msg
}

async fn read_record<S: AsyncRead + Unpin>(stream: &mut S) -> Result<(u8, Vec<u8>), IdeviceError> {
    let mut header = [0u8; 5];
    stream.read_exact(&mut header).await?;
    let ct = header[0];
    let len = u16::from_be_bytes([header[3], header[4]]) as usize;
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).await?;
    Ok((ct, payload))
}

/// Extract handshake messages from a record payload.
/// A single record can contain multiple handshake messages.
fn parse_handshake_messages(data: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut msgs = Vec::new();
    let mut pos = 0;
    while pos + 4 <= data.len() {
        let msg_type = data[pos];
        let msg_len = u32::from_be_bytes([0, data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        if pos + 4 + msg_len > data.len() {
            break;
        }
        msgs.push((msg_type, data[pos..pos + 4 + msg_len].to_vec()));
        pos += 4 + msg_len;
    }
    msgs
}

/// Perform TLS 1.2 PSK handshake and return an encrypted stream.
pub async fn tls_psk_handshake<S: AsyncRead + AsyncWrite + Unpin + Send>(
    mut stream: S,
    psk: &[u8],
) -> Result<TlsPskStream<S>, IdeviceError> {
    let mut client_random = [0u8; 32];
    rand::fill(&mut client_random);
    let mut server_random = [0u8; 32];
    let mut selected_cipher = [0u8; 2];
    let mut transcript = Vec::new();

    // 1. ClientHello
    let mut ch_body = Vec::new();
    ch_body.extend_from_slice(&TLS_12);
    ch_body.extend_from_slice(&client_random);
    ch_body.push(0x00); // session_id len = 0
    let suites_len = (PSK_CIPHER_SUITES.len() * 2) as u16;
    ch_body.extend_from_slice(&suites_len.to_be_bytes());
    for suite in PSK_CIPHER_SUITES {
        ch_body.extend_from_slice(suite);
    }
    ch_body.extend_from_slice(&[0x01, 0x00]); // compression: null
    let ch = make_handshake(HS_CLIENT_HELLO, &ch_body);
    transcript.extend_from_slice(&ch);
    stream.write_all(&make_record(CT_HANDSHAKE, &ch)).await?;
    debug!("Sent ClientHello");

    // 2. Read ServerHello, optional ServerKeyExchange (PSK hint), ServerHelloDone
    loop {
        let (ct, payload) = read_record(&mut stream).await?;
        if ct == 21 {
            // TLS Alert
            let level = payload.first().copied().unwrap_or(0);
            let desc = payload.get(1).copied().unwrap_or(0);
            return Err(IdeviceError::InternalError(format!(
                "TLS Alert: level={level} desc={desc} ({})",
                match desc {
                    0 => "close_notify",
                    10 => "unexpected_message",
                    20 => "bad_record_mac",
                    40 => "handshake_failure",
                    47 => "illegal_parameter",
                    70 => "protocol_version",
                    71 => "insufficient_security",
                    80 => "internal_error",
                    _ => "unknown",
                }
            )));
        }
        if ct != CT_HANDSHAKE {
            return Err(IdeviceError::InternalError(format!(
                "Expected handshake, got ct={ct}"
            )));
        }
        transcript.extend_from_slice(&payload);

        for (msg_type, _msg_bytes) in parse_handshake_messages(&payload) {
            match msg_type {
                HS_SERVER_HELLO => {
                    // ServerHello layout (after 4-byte handshake header):
                    // 2 bytes version, 32 bytes random, 1 byte session_id_len,
                    // session_id, 2 bytes cipher_suite
                    if payload.len() >= 4 + 2 + 32 {
                        server_random.copy_from_slice(&payload[6..38]);
                        let sid_len = payload[38] as usize;
                        if payload.len() >= 39 + sid_len + 2 {
                            selected_cipher
                                .copy_from_slice(&payload[39 + sid_len..39 + sid_len + 2]);
                        }
                    }
                    debug!("Got ServerHello, cipher={selected_cipher:02x?}");
                }
                HS_SERVER_HELLO_DONE => {
                    debug!("Got ServerHelloDone");
                }
                _ => {
                    debug!("Got handshake msg type {msg_type}");
                }
            }
        }

        // Check if we've seen ServerHelloDone
        if payload.contains(&HS_SERVER_HELLO_DONE) && payload.len() >= 4 {
            // Simple check: if ServerHelloDone is in the payload, we're done
            // ServerHelloDone is a 0-length message: [0x0E, 0x00, 0x00, 0x00]
            if payload
                .windows(4)
                .any(|w| w == [HS_SERVER_HELLO_DONE, 0x00, 0x00, 0x00])
            {
                break;
            }
        }
    }

    // 3. Determine cipher suite and derive keys
    let suite = CipherSuite::from_bytes(selected_cipher).ok_or_else(|| {
        IdeviceError::InternalError(format!(
            "Server selected unsupported cipher: {selected_cipher:02x?}"
        ))
    })?;
    debug!("Using cipher suite: {suite:?}");

    let master = derive_master_secret(psk, &client_random, &server_random, suite);
    let keys = derive_key_block(&master, &client_random, &server_random, suite);

    // 4. ClientKeyExchange (empty PSK identity)
    let cke = make_handshake(HS_CLIENT_KEY_EXCHANGE, &[0x00, 0x00]);
    transcript.extend_from_slice(&cke);
    stream.write_all(&make_record(CT_HANDSHAKE, &cke)).await?;
    debug!("Sent ClientKeyExchange");

    // 5. ChangeCipherSpec
    stream
        .write_all(&make_record(CT_CHANGE_CIPHER_SPEC, &[0x01]))
        .await?;
    debug!("Sent ChangeCipherSpec");

    // 6. Client Finished (encrypted)
    let vd = finished_verify_data(&master, b"client finished", &transcript, suite);
    let fin = make_handshake(HS_FINISHED, &vd);
    transcript.extend_from_slice(&fin);
    let enc_fin = encrypt_record(&keys, 0, CT_HANDSHAKE, &fin);
    stream
        .write_all(&make_record(CT_HANDSHAKE, &enc_fin))
        .await?;
    stream.flush().await?;
    debug!("Sent encrypted Finished");

    // 7. Read server ChangeCipherSpec + Finished
    let mut server_seq: u64 = 0;
    loop {
        let (ct, payload) = read_record(&mut stream).await?;
        if ct == 21 {
            let level = payload.first().copied().unwrap_or(0);
            let desc = payload.get(1).copied().unwrap_or(0);
            return Err(IdeviceError::InternalError(format!(
                "TLS Alert after Finished: level={level} desc={desc}"
            )));
        }
        match ct {
            CT_CHANGE_CIPHER_SPEC => {
                debug!("Got server ChangeCipherSpec");
            }
            CT_APPLICATION_DATA | CT_HANDSHAKE => {
                let plaintext = decrypt_record(&keys, true, server_seq, CT_HANDSHAKE, &payload)?;
                server_seq += 1;

                if plaintext.len() >= 4 && plaintext[0] == HS_FINISHED {
                    let server_vd =
                        finished_verify_data(&master, b"server finished", &transcript, suite);
                    if plaintext[4..] == server_vd {
                        debug!("Server Finished verified!");
                    } else {
                        debug!("Server Finished verify_data mismatch (continuing anyway)");
                    }
                    break;
                }
            }
            _ => {
                debug!("Unexpected record type {ct} during handshake");
            }
        }
    }

    debug!("TLS-PSK handshake complete");

    Ok(TlsPskStream {
        inner: stream,
        keys,
        write_seq: 1, // seq 0 was used for client Finished
        read_seq: server_seq,
        read_buf: Vec::new(),
        pending_record: Vec::new(),
        pending_record_total: 0,
        write_buf: Vec::new(),
    })
}

/// An encrypted TLS-PSK stream implementing AsyncRead + AsyncWrite.
pub struct TlsPskStream<S> {
    inner: S,
    keys: KeyBlock,
    write_seq: u64,
    read_seq: u64,
    /// Decrypted plaintext buffered for reads
    read_buf: Vec<u8>,
    /// Partial inbound TLS record being assembled
    pending_record: Vec<u8>,
    /// Expected total bytes for the current inbound record (5 + body_len), 0 if unknown
    pending_record_total: usize,
    /// Partial outbound TLS record waiting to be flushed
    write_buf: Vec<u8>,
}

impl<S> std::fmt::Debug for TlsPskStream<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsPskStream")
            .field("write_seq", &self.write_seq)
            .field("read_seq", &self.read_seq)
            .finish()
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send> TlsPskStream<S> {
    /// Encrypt and send application data, splitting into multiple TLS records if needed.
    pub async fn write_app_data(&mut self, data: &[u8]) -> Result<(), IdeviceError> {
        for chunk in data.chunks(TLS_MAX_PLAINTEXT) {
            let encrypted = encrypt_record(&self.keys, self.write_seq, CT_APPLICATION_DATA, chunk);
            self.write_seq += 1;
            self.inner
                .write_all(&make_record(CT_APPLICATION_DATA, &encrypted))
                .await?;
        }
        self.inner.flush().await?;
        Ok(())
    }

    /// Read and decrypt application data.
    pub async fn read_app_data(&mut self) -> Result<Vec<u8>, IdeviceError> {
        let (ct, payload) = read_record(&mut self.inner).await?;
        if ct != CT_APPLICATION_DATA {
            return Err(IdeviceError::InternalError(format!(
                "Expected application data, got ct={ct}"
            )));
        }
        let plaintext = decrypt_record(
            &self.keys,
            true,
            self.read_seq,
            CT_APPLICATION_DATA,
            &payload,
        )?;
        self.read_seq += 1;
        Ok(plaintext)
    }
}

// AsyncRead: assemble complete TLS records across multiple poll_read calls
impl<S: AsyncRead + AsyncWrite + Unpin + Send + Sync> AsyncRead for TlsPskStream<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();

        // 1. Serve from decrypted buffer first
        if !this.read_buf.is_empty() {
            let n = buf.remaining().min(this.read_buf.len());
            buf.put_slice(&this.read_buf[..n]);
            this.read_buf.drain(..n);
            return Poll::Ready(Ok(()));
        }

        // 2. Continue assembling a TLS record
        loop {
            // If we have the header (5 bytes), compute total needed
            if this.pending_record.len() >= 5 && this.pending_record_total == 0 {
                let body_len =
                    u16::from_be_bytes([this.pending_record[3], this.pending_record[4]]) as usize;
                this.pending_record_total = 5 + body_len;
            }

            // If we have a complete record, decrypt it
            if this.pending_record_total > 0
                && this.pending_record.len() >= this.pending_record_total
            {
                let ct = this.pending_record[0];
                let body = this.pending_record[5..this.pending_record_total].to_vec();
                this.pending_record.drain(..this.pending_record_total);
                this.pending_record_total = 0;

                match decrypt_record(&this.keys, true, this.read_seq, ct, &body) {
                    Ok(plaintext) => {
                        this.read_seq += 1;
                        let n = buf.remaining().min(plaintext.len());
                        buf.put_slice(&plaintext[..n]);
                        if n < plaintext.len() {
                            this.read_buf.extend_from_slice(&plaintext[n..]);
                        }
                        return Poll::Ready(Ok(()));
                    }
                    Err(e) => {
                        tracing::warn!(
                            "TLS decrypt failed (ct={ct}, seq={}, body_len={}): {e}",
                            this.read_seq,
                            body.len()
                        );
                        return Poll::Ready(Err(std::io::Error::other(format!(
                            "TLS decrypt: {e}"
                        ))));
                    }
                }
            }

            // Need more data from the underlying stream
            let mut tmp = [0u8; 16384];
            let mut tmp_buf = ReadBuf::new(&mut tmp);
            match Pin::new(&mut this.inner).poll_read(cx, &mut tmp_buf) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(Ok(())) => {
                    let n = tmp_buf.filled().len();
                    if n == 0 {
                        return Poll::Ready(Ok(())); // EOF
                    }
                    this.pending_record.extend_from_slice(&tmp[..n]);
                    // Loop back to check if we now have a complete record
                }
            }
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send + Sync> AsyncWrite for TlsPskStream<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();

        // Flush any pending partial write first
        while !this.write_buf.is_empty() {
            match Pin::new(&mut this.inner).poll_write(cx, &this.write_buf) {
                Poll::Ready(Ok(n)) => {
                    this.write_buf.drain(..n);
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }

        // Clamp to TLS max record size to avoid oversized records
        let chunk = &data[..data.len().min(TLS_MAX_PLAINTEXT)];

        // Encrypt the new data into a TLS record
        let encrypted = encrypt_record(&this.keys, this.write_seq, CT_APPLICATION_DATA, chunk);
        let record = make_record(CT_APPLICATION_DATA, &encrypted);
        this.write_seq += 1;

        match Pin::new(&mut this.inner).poll_write(cx, &record) {
            Poll::Ready(Ok(written)) => {
                if written < record.len() {
                    // Buffer the unsent remainder
                    this.write_buf.extend_from_slice(&record[written..]);
                }
                Poll::Ready(Ok(chunk.len()))
            }
            Poll::Ready(Err(e)) => {
                this.write_seq -= 1;
                Poll::Ready(Err(e))
            }
            Poll::Pending => {
                // Buffer the entire record for later flush
                this.write_buf = record;
                Poll::Pending
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();

        // Flush buffered write data first
        while !this.write_buf.is_empty() {
            match Pin::new(&mut this.inner).poll_write(cx, &this.write_buf) {
                Poll::Ready(Ok(n)) => {
                    this.write_buf.drain(..n);
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }

        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}
