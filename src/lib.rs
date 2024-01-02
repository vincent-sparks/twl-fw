#![feature(get_mut_unchecked)]
use tokio::sync::{oneshot, mpsc, Mutex};
use twilight_model::application::interaction::{Interaction, InteractionType, InteractionData, application_command::CommandData, modal::ModalInteractionData, message_component::MessageComponentInteractionData};
use tracing::{info};
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use twilight_http::Client;
use twilight_model::http::interaction::{InteractionResponse, InteractionResponseType, InteractionResponseData};
use twilight_model::channel::message::component::{ActionRow,TextInput,Component};
use twilight_model::id::{Id, marker::InteractionMarker};
use twilight_util::builder::InteractionResponseDataBuilder;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use rand::{thread_rng, seq::SliceRandom};
use once_cell::sync::Lazy;

pub type CommandFunc = Box<dyn Fn(Arc<InteractionHandler>, Interaction, CommandData) -> Pin<Box<dyn Future<Output=anyhow::Result<()>> + Send>> + Send + Sync>;

pub type CommandMap = phf::Map<&'static str, &'static Lazy<CommandFunc>>;

#[derive(Default)]
struct _CommandIDMappings {
    interaction_id_to_command_id: HashMap<Id<InteractionMarker>, usize>,
    command_id_to_interaction: HashMap<usize, (Id<InteractionMarker>, String)>,
}

#[cfg(not(test))]
type PossiblyFakeClient = Client;
#[cfg(test)]
type PossiblyFakeClient = tests::FakeClient;

pub struct InteractionHandler {
    pub client: Arc<PossiblyFakeClient>,
    commands: &'static CommandMap,
    modal_waiters: Mutex<HashMap<String, oneshot::Sender<(Interaction, ModalInteractionData)>>>,
    message_waiters: Mutex<HashMap<String, oneshot::Sender<(Interaction, MessageComponentInteractionData)>>>,
    message_waiters_multishot: Mutex<HashMap<String, mpsc::Sender<(Interaction, MessageComponentInteractionData)>>>,
    
    current_command_id: AtomicUsize,
    command_id_mappings: Mutex<_CommandIDMappings>,
}

// I would have liked to build this from a string, but I don't know if you can do that at compile time.
const CUSTOM_ID_CHARS: &'static [char] = &['a','b','c','d','e','f','g','h','i','j','k','l','m','n','o','p','q','r','s','t','u','v','w','x','y','z','A','B','C','D','E','F','G','H','I','J','K','L','M','N','O','P','Q','R','S','T','U','V','W','X','Y','Z','0','1','2','3','4','5','6','7','8','9',];
const CUSTOM_ID_LEN: usize = 6;


fn build_custom_id(ctx: &str) -> String {
    let mut custom_id = String::from(ctx);
    custom_id.push('-');
    custom_id.extend(CUSTOM_ID_CHARS.choose_multiple(&mut thread_rng(), CUSTOM_ID_LEN));
    custom_id
}

macro_rules! log_err {
    ($expr:expr, $message: literal $(, $arg: expr)*) => {
        if let Err(e) = $expr {
            tracing::error!(?e, $message $(, $arg)*);
        }
    }
}

macro_rules! unwrap_or_log {
    ($expr: expr, $message: literal $(, $arg: expr)*) => {
        match $expr {
            Some(x) => x,
            None => {
                tracing::error!($message $(, $arg)*);
                return;
            }
        }
    }
}

#[macro_export]
macro_rules! build_command {
    (|$client: ident, $inter: ident, $data: ident| $($body:tt)*) => {
        ::once_cell::sync::Lazy::new(|| ::std::boxed::Box::new(|$client, $inter, $data| ::std::boxed::Box::pin({$($body)*})))
    }
}

impl InteractionHandler {
    pub fn new(client: Arc<PossiblyFakeClient>, commands: &'static CommandMap) -> Self {
        Self {
            client, commands,
            current_command_id: AtomicUsize::new(0),
            message_waiters: Default::default(),
            message_waiters_multishot: Default::default(),
            modal_waiters: Default::default(),
            command_id_mappings: Default::default(),
        }
    }

    pub async fn handle(self: Arc<Self>, mut interaction: Interaction) {
        if interaction.kind == InteractionType::Ping {
            info!("Got a ping from the Discord API");
            let interaction_client = self.client.interaction(interaction.application_id);
            log_err!(interaction_client.create_response(interaction.id, &interaction.token, &InteractionResponse {
                kind: InteractionResponseType::Pong, data: None
            }).await, "Error answering ping");
            return;
        }
        // ping is the only interaction type that doesn't have data, so the unwrap here should be
        // OK
        match interaction.data.take().unwrap() {
            InteractionData::ApplicationCommand(data) => {
                if let Some(func) = self.commands.get(&data.name) {
                    let id = self.current_command_id.fetch_add(1, Ordering::Relaxed);
                    // the app ID shouldn't be able to change between interactions in the same
                    // chain but this still makes me nervous.
                    let app_id = interaction.application_id;
                    let mut mappings = self.command_id_mappings.lock().await;
                    mappings.interaction_id_to_command_id.insert(interaction.id, id);
                    mappings.command_id_to_interaction.insert(id, (interaction.id, interaction.token.clone()));
                    std::mem::drop(mappings);

                    let this = self.clone();
                    let fut = func(self.clone(), interaction, *data);
                    let fut = async move {
                        let res = fut.await;

                        let mut mappings = this.command_id_mappings.lock().await;
                        let inter = mappings.command_id_to_interaction.remove(&id);

                        if let Some((id, _)) = &inter {
                            // tiny memory leaks are still memory leaks!
                            mappings.interaction_id_to_command_id.remove(id);
                        }

                        if let Err(e) = res {
                            if let Some((id, token)) = inter {
                                log_err!(this.client.interaction(app_id).create_response(id, &token, &InteractionResponse {
                                    kind: InteractionResponseType::ChannelMessageWithSource,
                                    data: Some(InteractionResponseDataBuilder::new()
                                               .content(format!("Internal error! {}", e))
                                               .build()),
                                }).await, "error notifying user of error: {}", e);
                            } else {
                                tracing::error!(?e, "an error occurred in a slash command but no interaction was present to report it");
                            }
                        }
                    };
                    #[cfg(test)]
                    fut.await;
                    #[cfg(not(test))]
                    tokio::spawn(fut);
                }
            },
            InteractionData::ModalSubmit(modal) => {
                let waiter = unwrap_or_log!(self.modal_waiters.lock().await.remove(&modal.custom_id), "unknown custom id: {}", &modal.custom_id);
                unwrap_or_log!(waiter.send((interaction, modal)).ok(), "task was cancelled while waiting for an interaction");
            },
            InteractionData::MessageComponent(message) => {
                let mut waiters = self.message_waiters_multishot.lock().await;
                let id = message.custom_id.clone();
                let waiter = unwrap_or_log!(waiters.get_mut(&message.custom_id), "unknown custom id: {}", &message.custom_id);
                if waiter.send((interaction, message)).await.is_err() {
                    // the task dropped the receiver, which means they no longer care.
                    waiters.remove(&id);
                    // ... and clear the interaction buttons from the message.
                    //log_err!(self.client.interaction(interaction.application_id).update_response(&interaction.token).components(None).unwrap().await, "removing components from message");
                }
            },
            _ => todo!(),
        }
    }
    pub async fn show_modal(&self, interaction: &Interaction, title: String, fields: Vec<TextInput>, id: &'static str) -> Result<(Interaction, ModalInteractionData), twilight_http::Error> {
        let components: Vec<Component> = fields.into_iter().map(Component::TextInput).collect();
        let components = vec![Component::ActionRow(ActionRow{components})];
        let (sender, receiver) = oneshot::channel();
        let custom_id = build_custom_id(id);
        self.modal_waiters.lock().await.insert(custom_id.clone(), sender);
        self.client.interaction(interaction.application_id).create_response(interaction.id, &interaction.token, &InteractionResponse {
            kind: InteractionResponseType::Modal,
            data: Some(InteractionResponseDataBuilder::new()
                       .title(title)
                       .components(components)
                       .custom_id(custom_id)
                       .build()),
        }).await?;

        // receiver wil only fail if the sender is dropped before it sends
        // that can theoretically only happen if self gets dropped while a future that depends on
        // it is still running
        let (inter, response) = receiver.await.unwrap();
        let mut mappings = self.command_id_mappings.lock().await;
        let cmd_id = mappings.interaction_id_to_command_id.remove(&interaction.id).expect("callback disappeared from inter->command mapping");
        mappings.command_id_to_interaction.insert(cmd_id, (inter.id, inter.token.clone()));
        mappings.interaction_id_to_command_id.insert(inter.id, cmd_id);
        Ok((inter, response))
    }
    pub async fn send_response_with_components(&self, interaction: &Interaction, mut response: InteractionResponseData) -> Result<mpsc::Receiver<(Interaction, MessageComponentInteractionData)>, twilight_http::Error> {
        if response.custom_id.is_none() {
            response.custom_id = Some(build_custom_id(""));
        }
        let (sender, receiver) = mpsc::channel(5);
        self.message_waiters_multishot.lock().await.insert(response.custom_id.clone().unwrap(), sender);
        self.client.interaction(interaction.application_id).create_response(interaction.id, &interaction.token, &InteractionResponse {kind: InteractionResponseType::ChannelMessageWithSource, data: Some(response)}).await?;
        Ok(receiver)
    }

    pub async fn manually_add_component_interaction_waiter(&self, custom_id: String) -> oneshot::Receiver<(Interaction, MessageComponentInteractionData)> {
        let (sender, receiver) = oneshot::channel();
        self.message_waiters.lock().await.insert(custom_id, sender);
        receiver
    }
}

#[cfg(test)]
pub mod tests;
