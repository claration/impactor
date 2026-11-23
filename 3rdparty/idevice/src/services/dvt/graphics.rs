// Jackson Coxson
//! Graphics monitoring service - Monitor GPU/graphics performance

use std::fmt;

use plist::{Dictionary, Value};

use super::message::AuxValue;
use super::remote_server::{Channel, RemoteServerClient};
use crate::{IdeviceError, ReadWrite, obf};

/// Client for graphics monitoring operations on iOS devices
#[derive(Debug)]
pub struct GraphicsClient<'a, R: ReadWrite> {
    channel: Channel<'a, R>,
}

/// Graphics sample data from `com.apple.instruments.server.services.graphics.opengl`
#[derive(Debug, Clone)]
pub struct GraphicsSample {
    /// Timestamp in microseconds (XRVideoCardRunTimeStamp)
    pub timestamp: u64,
    /// Core Animation frames per second
    pub fps: f64,
    /// Allocated GPU/system memory in bytes
    pub alloc_system_memory: u64,
    /// In-use system memory in bytes
    pub in_use_system_memory: u64,
    /// In-use system memory attributed to the driver
    pub in_use_system_memory_driver: u64,
    /// GPU bundle name (e.g. "Built-In")
    pub gpu_bundle_name: String,
    /// GPU recovery count
    pub recovery_count: u64,
}

impl GraphicsSample {
    /// Parse a graphics sample from the plist dict pushed by the device.
    /// Returns `Err` if the dict doesn't look like a graphics frame (so callers
    /// can skip non-data messages such as the startSamplingAtTimeInterval: reply).
    pub fn from_plist(data: Value) -> Result<Self, IdeviceError> {
        let dict = data
            .into_dictionary()
            .ok_or_else(|| IdeviceError::UnexpectedResponse("expected dictionary".into()))?;

        // Require at least one key that's unique to graphics data frames.
        if !dict.contains_key("XRVideoCardRunTimeStamp") {
            return Err(IdeviceError::UnexpectedResponse(
                "not a graphics data frame".into(),
            ));
        }

        Ok(Self {
            timestamp: get_u64(&dict, "XRVideoCardRunTimeStamp"),
            fps: get_f64(&dict, "CoreAnimationFramesPerSecond"),
            alloc_system_memory: get_u64(&dict, "Alloc system memory"),
            in_use_system_memory: get_u64(&dict, "In use system memory"),
            in_use_system_memory_driver: get_u64(&dict, "In use system memory (driver)"),
            gpu_bundle_name: dict
                .get("IOGLBundleName")
                .and_then(|v| v.as_string())
                .unwrap_or("")
                .to_string(),
            recovery_count: get_u64(&dict, "recoveryCount"),
        })
    }
}

fn get_u64(dict: &Dictionary, key: &str) -> u64 {
    dict.get(key)
        .and_then(|v| match v {
            Value::Integer(i) => i.as_unsigned(),
            Value::Real(f) => Some(*f as u64),
            _ => None,
        })
        .unwrap_or(0)
}

fn get_f64(dict: &Dictionary, key: &str) -> f64 {
    dict.get(key)
        .and_then(|v| match v {
            Value::Real(f) => Some(*f),
            Value::Integer(i) => i.as_signed().map(|i| i as f64),
            _ => None,
        })
        .unwrap_or(0.0)
}

impl<'a, R: ReadWrite> GraphicsClient<'a, R> {
    /// Creates a new GraphicsClient
    pub async fn new(client: &'a mut RemoteServerClient<R>) -> Result<Self, IdeviceError> {
        let channel = client
            .make_channel(obf!(
                "com.apple.instruments.server.services.graphics.opengl"
            ))
            .await?;
        Ok(Self { channel })
    }

    /// Starts graphics sampling at the specified interval.
    /// Consumes the device's reply before returning.
    pub async fn start_sampling(&mut self, interval: f64) -> Result<(), IdeviceError> {
        self.channel
            .call_method(
                Some(Value::String("startSamplingAtTimeInterval:".into())),
                Some(vec![AuxValue::Double(interval)]),
                true,
            )
            .await?;
        // Consume the reply so it doesn't pollute the notification stream.
        self.channel.read_message().await?;
        Ok(())
    }

    /// Stops graphics sampling
    pub async fn stop_sampling(&mut self) -> Result<(), IdeviceError> {
        self.channel
            .call_method(Some(Value::String("stopSampling".into())), None, false)
            .await
    }

    /// Reads the next graphics data frame pushed by the device.
    /// Skips any non-data messages (e.g. ACKs, capability notices).
    pub async fn sample(&mut self) -> Result<GraphicsSample, IdeviceError> {
        loop {
            let msg = self.channel.read_message().await?;
            if let Some(data) = msg.data
                && let Ok(sample) = GraphicsSample::from_plist(data)
            {
                return Ok(sample);
            }
        }
    }
}

impl fmt::Display for GraphicsSample {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "t={:>8}µs | fps={:>5.1} | mem_alloc={:>10} | mem_used={:>10} | gpu={}",
            self.timestamp,
            self.fps,
            self.alloc_system_memory,
            self.in_use_system_memory,
            self.gpu_bundle_name,
        )
    }
}
