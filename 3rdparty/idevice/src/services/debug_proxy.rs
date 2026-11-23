//! GDB Remote Debugging Protocol Implementation for iOS Devices
//!
//! Provides functionality for communicating with the iOS debug server using the
//! GDB Remote Serial Protocol as documented at:
//! https://sourceware.org/gdb/current/onlinedocs/gdb.html/Packets.html#Packets

use std::fmt::Write;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::debug;

use crate::{IdeviceError, ReadWrite, RsdService, obf};

impl RsdService for DebugProxyClient<Box<dyn ReadWrite>> {
    fn rsd_service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.internal.dt.remote.debugproxy")
    }

    async fn from_stream(stream: Box<dyn ReadWrite>) -> Result<Self, IdeviceError> {
        Ok(Self {
            socket: stream,
            noack_mode: false,
        })
    }
}

/// Client for interacting with the iOS debug proxy service
///
/// Implements the GDB Remote Serial Protocol for communicating with debugserver
/// on iOS devices. Handles packet formatting, checksums, and acknowledgments.
#[derive(Debug)]
pub struct DebugProxyClient<R: ReadWrite> {
    /// The underlying socket connection to debugproxy
    pub socket: R,
    /// Flag indicating whether ACK mode is disabled
    pub noack_mode: bool,
}

/// Represents a debugserver command with arguments
///
/// Commands follow the GDB Remote Serial Protocol format:
/// $<command>[<hex-encoded args>]#<checksum>
#[derive(Debug)]
pub struct DebugserverCommand {
    /// The command name (e.g. "qSupported", "vCont")
    pub name: String,
    /// Command arguments that will be hex-encoded
    pub argv: Vec<String>,
}

impl DebugserverCommand {
    /// Creates a new debugserver command
    ///
    /// # Arguments
    /// * `name` - The command name (without leading $)
    /// * `argv` - Arguments that will be hex-encoded in the packet
    pub fn new(name: String, argv: Vec<String>) -> Self {
        Self { name, argv }
    }
}

impl<R: ReadWrite> DebugProxyClient<R> {
    /// Creates a new debug proxy client with default settings
    ///
    /// # Arguments
    /// * `socket` - Established connection to debugproxy service
    pub fn new(socket: R) -> Self {
        Self {
            socket,
            noack_mode: false,
        }
    }

    /// Consumes the client and returns the underlying socket
    pub fn into_inner(self) -> R {
        self.socket
    }

    /// Sends a command to debugserver and waits for response
    ///
    /// Formats the command according to GDB Remote Serial Protocol:
    /// $<command>[<hex-encoded args>]#<checksum>
    ///
    /// # Arguments
    /// * `command` - The command and arguments to send
    ///
    /// # Returns
    /// The response string if successful, None if no response received
    ///
    /// # Errors
    /// Returns `IdeviceError` if communication fails
    pub async fn send_command(
        &mut self,
        command: DebugserverCommand,
    ) -> Result<Option<String>, IdeviceError> {
        // Hex-encode the arguments
        let hex_args = command
            .argv
            .iter()
            .map(|arg| hex_encode(arg.as_bytes()))
            .collect::<Vec<String>>()
            .join("");

        // Construct the packet data (command + hex-encoded arguments)
        let packet_data = format!("{}{}", command.name, hex_args);

        // Calculate the checksum
        let checksum = calculate_checksum(&packet_data);

        // Construct the full packet
        let packet = format!("${packet_data}#{checksum}");

        // Log the packet for debugging
        debug!("Sending packet: {packet}");

        // Send the packet
        self.socket.write_all(packet.as_bytes()).await?;
        self.socket.flush().await?;

        // Read the response
        let response = self.read_response().await?;
        Ok(response)
    }

    /// Reads a response packet from debugserver
    ///
    /// Handles the GDB Remote Serial Protocol response format:
    /// $<data>#<checksum>
    ///
    /// # Returns
    /// The response data without protocol framing if successful
    ///
    /// # Errors
    /// Returns `IdeviceError` if communication fails or protocol is violated
    pub async fn read_response(&mut self) -> Result<Option<String>, IdeviceError> {
        let mut buffer = Vec::new();
        let mut received_char = [0u8; 1];

        if !self.noack_mode {
            self.socket.read_exact(&mut received_char).await?;
            if received_char[0] != b'+' {
                debug!("No + ack");
                return Ok(None);
            }
        }

        self.socket.read_exact(&mut received_char).await?;
        if received_char[0] != b'$' {
            debug!("No $ response");
            return Ok(None);
        }

        loop {
            self.socket.read_exact(&mut received_char).await?;
            if received_char[0] == b'#' {
                break;
            }
            buffer.push(received_char[0]);
        }
        // swallow checksum
        let mut checksum_chars = [0u8; 2];
        self.socket.read_exact(&mut checksum_chars).await?;

        if !self.noack_mode {
            self.send_ack().await?;
        }

        let response = String::from_utf8(buffer)?;
        Ok(Some(response))
    }

    /// Sends raw bytes directly to the debugproxy connection
    ///
    /// # Arguments
    /// * `bytes` - The raw bytes to send
    ///
    /// # Errors
    /// Returns `IdeviceError` if writing fails
    pub async fn send_raw(&mut self, bytes: &[u8]) -> Result<(), IdeviceError> {
        self.socket.write_all(bytes).await?;
        self.socket.flush().await?;
        Ok(())
    }

    /// Reads raw bytes from the debugproxy connection
    ///
    /// # Arguments
    /// * `len` - Maximum number of bytes to read
    ///
    /// # Returns
    /// The received data as a string
    ///
    /// # Errors
    /// Returns `IdeviceError` if reading fails or data isn't valid UTF-8
    pub async fn read(&mut self, len: usize) -> Result<String, IdeviceError> {
        let mut buf = vec![0; len];
        let r = self.socket.read(&mut buf).await?;

        Ok(String::from_utf8_lossy(&buf[..r]).to_string())
    }

    /// Sets program arguments using the 'A' command
    ///
    /// Formats arguments according to GDB protocol:
    /// A<arglen>,<argnum>,<argdata>
    ///
    /// # Arguments
    /// * `argv` - Program arguments to set
    ///
    /// # Returns
    /// The debugserver response
    ///
    /// # Errors
    /// Returns `IdeviceError` if arguments are empty or communication fails
    pub async fn set_argv(&mut self, argv: Vec<String>) -> Result<String, IdeviceError> {
        if argv.is_empty() {
            return Err(IdeviceError::InvalidArgument);
        }

        // Calculate the total length of the packet
        let mut pkt_len = 0;
        for (i, arg) in argv.iter().enumerate() {
            let prefix = format!(",{},{},", arg.len() * 2, i);
            pkt_len += prefix.len() + arg.len() * 2;
        }

        // Allocate and initialize the packet
        let mut pkt = vec![0u8; pkt_len + 1];
        let mut pktp = 0;

        for (i, arg) in argv.iter().enumerate() {
            let prefix = format!(",{},{},", arg.len() * 2, i);
            let prefix_bytes = prefix.as_bytes();

            // Copy prefix to the packet
            pkt[pktp..pktp + prefix_bytes.len()].copy_from_slice(prefix_bytes);
            pktp += prefix_bytes.len();

            // Hex encode the argument
            for byte in arg.bytes() {
                let hex = format!("{byte:02X}");
                pkt[pktp..pktp + 2].copy_from_slice(hex.as_bytes());
                pktp += 2;
            }
        }

        // Set the first byte of the packet
        pkt[0] = b'A';

        // Simulate sending the command and receiving a response
        self.send_raw(&pkt).await?;
        let response = self.read(16).await?;

        Ok(response)
    }

    /// Sends an acknowledgment (+)
    ///
    /// # Errors
    /// Returns `IdeviceError` if writing fails
    pub async fn send_ack(&mut self) -> Result<(), IdeviceError> {
        self.socket.write_all(b"+").await?;
        self.socket.flush().await?;
        Ok(())
    }

    /// Sends a negative acknowledgment (-)
    ///
    /// # Errors
    /// Returns `IdeviceError` if writing fails
    pub async fn send_noack(&mut self) -> Result<(), IdeviceError> {
        self.socket.write_all(b"-").await?;
        self.socket.flush().await?;
        Ok(())
    }

    /// Enables or disables ACK mode
    ///
    /// When disabled, the client won't expect or send acknowledgments
    ///
    /// # Arguments
    /// * `enabled` - Whether to enable ACK mode
    pub fn set_ack_mode(&mut self, enabled: bool) {
        self.noack_mode = !enabled;
    }
}

/// Calculates the checksum for a GDB protocol packet
///
/// The checksum is computed as the modulo 256 sum of all characters
/// between '$' and '#', formatted as two lowercase hex digits.
fn calculate_checksum(data: &str) -> String {
    let checksum = data.bytes().fold(0u8, |acc, byte| acc.wrapping_add(byte));
    format!("{checksum:02x}")
}

/// Hex-encodes bytes as uppercase string
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut output, b| {
        let _ = write!(output, "{b:02X}");
        output
    })
}

impl From<String> for DebugserverCommand {
    /// Converts a string into a debugserver command by splitting on whitespace
    ///
    /// The first token becomes the command name, remaining tokens become arguments
    fn from(s: String) -> Self {
        let mut split = s.split_whitespace();
        let command = split.next().unwrap_or("").to_string();
        let arguments: Vec<String> = split.map(|s| s.to_string()).collect();
        Self::new(command, arguments)
    }
}

impl From<&str> for DebugserverCommand {
    /// Converts a string slice into a debugserver command
    fn from(s: &str) -> DebugserverCommand {
        s.to_string().into()
    }
}
