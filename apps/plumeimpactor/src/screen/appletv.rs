use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use iced::widget::{button, column, container, opaque, row, rule, text, text_input};
use iced::{Center, Color, Element, Length, Task};
use plume_utils::wireless::{
    DiscoveredDevice, PairingRecord, PairingStore, ServiceKind, discover, pair,
};
use rust_i18n::t;
use tokio::sync::{Mutex, Notify, mpsc};

use crate::appearance;
use crate::defaults::get_data_path;

const HOST_NAME: &str = "plume";
const SCAN_SECONDS: u64 = 5;
const RECONNECT_SECONDS: u64 = 3;
const PIN_INPUT_ID: &str = "appletv-pin";

#[derive(Debug, Clone)]
pub enum Message {
    Scan,
    Scanned(Result<Scan, String>),
    StartPair(usize),
    PinChanged(String),
    SubmitPin,
    CancelPair,
    Resend,
    Paired(u64, Result<PairingRecord, String>),
    Forget(String),
    Forgotten(String, Result<(), String>),
}

#[derive(Debug, Clone)]
pub struct Scan {
    devices: Vec<DiscoveredDevice>,
    paired: Vec<PairingRecord>,
}

const PIN_LEN: usize = 6;

#[derive(Debug, Clone)]
struct Session {
    id: u64,
    address: SocketAddr,
    name: String,
    pin: String,
    pin_tx: mpsc::UnboundedSender<String>,
    pin_sent: bool,
    error: Option<String>,
    cancel: Arc<Notify>,
}

#[derive(Debug, Clone, Default)]
pub struct AppleTvSection {
    scanning: bool,
    scanned: bool,
    discovered: Vec<DiscoveredDevice>,
    paired: Vec<PairingRecord>,
    session: Option<Session>,
    next_session: u64,
    status: Option<(String, bool)>,
}

fn store() -> PairingStore {
    PairingStore::new(get_data_path().join("remotepairing"))
}

fn on_runtime<T, F>(
    future: F,
    map: impl Fn(Result<T, String>) -> Message + Send + 'static,
) -> Task<Message>
where
    T: Send + 'static,
    F: std::future::Future<Output = Result<T, String>> + Send + 'static,
{
    Task::perform(
        async move {
            let (tx, rx) = tokio::sync::oneshot::channel();
            std::thread::spawn(move || {
                let result = tokio::runtime::Runtime::new()
                    .map_err(|e| e.to_string())
                    .and_then(|rt| rt.block_on(future));
                let _ = tx.send(result);
            });
            rx.await
                .unwrap_or_else(|_| Err("Worker thread stopped".to_string()))
        },
        map,
    )
}

impl AppleTvSection {
    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Scan => {
                self.scanning = true;
                self.status = None;
                on_runtime(
                    async {
                        let store = store();
                        let devices = discover(Duration::from_secs(SCAN_SECONDS))
                            .await
                            .map_err(|e| e.to_string())?;
                        let paired = store.list().await.map_err(|e| e.to_string())?;

                        let mut ours = HashSet::new();
                        for device in &devices {
                            if let (ServiceKind::RemotePairing, Some(identifier), Some(auth_tag)) =
                                (device.kind, &device.identifier, &device.auth_tag)
                            {
                                if store
                                    .find(identifier, auth_tag)
                                    .await
                                    .map_err(|e| e.to_string())?
                                    .is_some()
                                {
                                    ours.insert(device.address.ip());
                                }
                            }
                        }

                        let devices = devices
                            .into_iter()
                            .filter(|device| {
                                device.kind == ServiceKind::ManualPairing
                                    && !ours.contains(&device.address.ip())
                            })
                            .collect();

                        Ok(Scan { devices, paired })
                    },
                    Message::Scanned,
                )
            }
            Message::Scanned(result) => {
                self.scanning = false;
                self.scanned = true;
                match result {
                    Ok(scan) => {
                        self.discovered = scan.devices;
                        self.paired = scan.paired;
                    }
                    Err(e) => self.status = Some((e, true)),
                }
                Task::none()
            }
            Message::StartPair(index) => {
                let Some(device) = self.discovered.get(index) else {
                    return Task::none();
                };
                if self.session.is_some() {
                    return Task::none();
                }

                let (address, name) = (device.address, device.display_name().to_string());
                self.begin(address, name, false)
            }
            Message::PinChanged(pin) => {
                if let Some(session) = &mut self.session {
                    session.pin = pin.chars().take(PIN_LEN).collect();
                }
                Task::none()
            }
            Message::SubmitPin => {
                if let Some(session) = &mut self.session {
                    let pin = session.pin.clone();
                    if pin.len() == PIN_LEN && !session.pin_sent {
                        session.pin_sent = true;
                        let _ = session.pin_tx.send(pin);
                    }
                }
                Task::none()
            }
            Message::CancelPair => {
                if let Some(session) = self.session.take() {
                    session.cancel.notify_one();
                }
                Task::none()
            }
            Message::Resend => self.restart(),
            Message::Paired(id, result) => {
                if self.session.as_ref().map(|s| s.id) != Some(id) {
                    return Task::none();
                }
                match result {
                    Ok(record) => {
                        let Some(session) = self.session.take() else {
                            return Task::none();
                        };
                        self.discovered
                            .retain(|device| device.address.ip() != session.address.ip());
                        self.paired.retain(|existing| existing.udid != record.udid);
                        self.paired.push(record);
                        self.paired
                            .sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.udid.cmp(&b.udid)));
                    }
                    Err(e) => {
                        log::warn!("Apple TV pairing failed: {e}");

                        if let Some(session) = &mut self.session {
                            session.error = Some(if session.pin_sent {
                                t!("appletv_wrong_pin").to_string()
                            } else {
                                t!("appletv_pair_failed").to_string()
                            });
                            session.pin.clear();
                            session.pin_sent = false;
                        }
                    }
                }
                Task::none()
            }
            Message::Forget(udid) => {
                let forgotten = udid.clone();
                on_runtime(
                    async move {
                        store()
                            .remove(&udid)
                            .await
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    },
                    move |result| Message::Forgotten(forgotten.clone(), result),
                )
            }
            Message::Forgotten(udid, result) => {
                match result {
                    Ok(()) => self.paired.retain(|record| record.udid != udid),
                    Err(e) => self.status = Some((e, true)),
                }
                Task::none()
            }
        }
    }

    fn begin(&mut self, address: SocketAddr, name: String, refresh: bool) -> Task<Message> {
        let (pin_tx, pin_rx) = mpsc::unbounded_channel();
        let pin_rx = Arc::new(Mutex::new(pin_rx));

        let id = self.next_session;
        self.next_session += 1;
        let cancel = Arc::new(Notify::new());
        let cancelled = cancel.clone();

        self.status = None;
        self.session = Some(Session {
            id,
            address,
            name,
            pin: String::new(),
            pin_tx,
            pin_sent: false,
            error: None,
            cancel,
        });

        let pairing = on_runtime(
            async move {
                let work = async {
                    let address = if refresh {
                        discover(Duration::from_secs(RECONNECT_SECONDS))
                            .await
                            .map_err(|e| e.to_string())?
                            .into_iter()
                            .find(|device| {
                                device.kind == ServiceKind::ManualPairing
                                    && device.address.ip() == address.ip()
                            })
                            .map(|device| device.address)
                            .ok_or_else(|| "The device stopped offering to pair".to_string())?
                    } else {
                        address
                    };

                    let (file, device) = pair(address, HOST_NAME, || {
                        let pin_rx = pin_rx.clone();
                        async move {
                            match pin_rx.lock().await.recv().await {
                                Some(pin) => pin,
                                None => std::future::pending::<String>().await,
                            }
                        }
                    })
                    .await
                    .map_err(|e| e.to_string())?;

                    let pairing_file_path = store()
                        .save(&device, &file)
                        .await
                        .map_err(|e| e.to_string())?;

                    Ok(PairingRecord {
                        udid: device.udid,
                        name: device.name,
                        model: device.model,
                        account_id: device.account_id,
                        pairing_file_path,
                    })
                };

                tokio::select! {
                    result = work => result,
                    _ = cancelled.notified() => Err("Cancelled".to_string()),
                }
            },
            move |result| Message::Paired(id, result),
        );

        Task::batch([pairing, iced::widget::operation::focus(PIN_INPUT_ID)])
    }

    fn restart(&mut self) -> Task<Message> {
        let Some(session) = self.session.take() else {
            return Task::none();
        };
        session.cancel.notify_one();

        self.begin(session.address, session.name, true)
    }

    pub fn dialog(&self) -> Option<Element<'_, Message>> {
        let session = self.session.as_ref()?;
        let can_submit = session.pin.len() == PIN_LEN && !session.pin_sent;

        let failed = session.error.is_some();

        let mut body = column![
            text(t!("appletv_pair_title", name = session.name))
                .size(appearance::THEME_FONT_SIZE + 2.0)
        ]
        .spacing(appearance::THEME_PADDING);

        match &session.error {
            Some(error) => {
                body = body.push(text(error).color(Color::from_rgb(0.9, 0.2, 0.2)));
            }
            None => {
                body = body.push(text(t!("appletv_enter_pin")));
                body = body.push(if session.pin_sent {
                    Element::from(text(t!("appletv_pairing")))
                } else {
                    text_input("PIN", &session.pin)
                        .id(PIN_INPUT_ID)
                        .on_input(Message::PinChanged)
                        .on_submit(Message::SubmitPin)
                        .into()
                });
            }
        }

        let mut actions = row![
            button(text(t!("appletv_cancel")))
                .on_press(Message::CancelPair)
                .style(appearance::s_button),
            button(text(t!("appletv_resend")))
                .on_press(Message::Resend)
                .style(if failed {
                    appearance::p_button
                } else {
                    appearance::s_button
                }),
        ]
        .spacing(appearance::THEME_PADDING);

        if !failed {
            actions = actions.push(
                button(text(t!("appletv_pair")))
                    .on_press_maybe(can_submit.then_some(Message::SubmitPin))
                    .style(appearance::p_button),
            );
        }

        let dialog = container(body.push(actions))
            .padding(appearance::THEME_PADDING * 2.0)
            .max_width(380.0)
            .style(|theme: &iced::Theme| container::Style {
                background: Some(iced::Background::Color(theme.palette().background)),
                border: iced::Border {
                    width: 1.0,
                    color: theme.palette().primary,
                    radius: appearance::THEME_CORNER_RADIUS.into(),
                },
                ..Default::default()
            });

        Some(
            opaque(
                container(dialog)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .center(Length::Fill)
                    .style(|_theme: &iced::Theme| container::Style {
                        background: Some(iced::Background::Color(Color::from_rgba(
                            0.0, 0.0, 0.0, 0.45,
                        ))),
                        ..Default::default()
                    }),
            )
            .into(),
        )
    }

    pub fn view(&self) -> Element<'_, Message> {
        let scan_label = if self.scanning {
            t!("appletv_scanning")
        } else {
            t!("appletv_scan")
        };

        let mut content = column![
            button(text(scan_label).align_x(Center))
                .on_press_maybe((!self.scanning).then_some(Message::Scan))
                .style(appearance::s_button)
                .width(Length::Fill)
        ]
        .spacing(8);

        if self.scanned && self.discovered.is_empty() {
            content =
                content.push(text(t!("appletv_none_found")).color(Color::from_rgb(0.7, 0.7, 0.7)));
        }

        if let Some((message, is_error)) = &self.status {
            let color = if *is_error {
                Color::from_rgb(0.9, 0.2, 0.2)
            } else {
                Color::from_rgb(0.2, 0.8, 0.4)
            };
            content = content.push(text(message).size(14).color(color));
        }

        for (index, device) in self.discovered.iter().enumerate() {
            content = content.push(
                row![
                    text(device.display_name().to_string())
                        .size(14)
                        .width(Length::Fill),
                    button(text(t!("appletv_pair")))
                        .on_press_maybe(self.session.is_none().then_some(Message::StartPair(index)))
                        .style(appearance::s_button),
                ]
                .align_y(Center),
            );
        }

        if !self.paired.is_empty() {
            content = content.push(rule::horizontal(1));
        }

        for record in &self.paired {
            content = content.push(
                row![
                    text(record.to_string()).size(14).width(Length::Fill),
                    button(text(t!("appletv_forget")))
                        .on_press(Message::Forget(record.udid.clone()))
                        .style(appearance::s_button),
                ]
                .align_y(Center),
            );
        }

        content.into()
    }
}
